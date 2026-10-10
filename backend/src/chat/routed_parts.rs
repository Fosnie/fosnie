// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Owned, bounded fan-out. Dropping the parent drops every pending HTTP future;
//! no detached tasks can spend after a sibling fails or the turn is cancelled.
use super::*;
use crate::model_routing::{run::RoutedRun, CallPurpose, RoutingError};
use futures_util::{stream::FuturesUnordered, StreamExt};

enum Event {
    Token(String),
    Reasoning(String),
    Done(GenTerminal),
}

async fn sources(
    state: &AppState,
    run: &RoutedRun,
    kbs: &[String],
    denied: &[String],
) -> Result<()> {
    let (chat, _) = routing::authorize(state, run).await?;
    let ctx = run.context(CallPurpose::Final);
    let (_, _, agent_id, _, _) = resolve_chat(
        state,
        &ctx.actor,
        Some(chat),
        None,
        None,
        "",
        origin::ChatOrigin::Web,
    )
    .await?;
    let allowed =
        crate::kb::retrieval_allowlist(&state.pg, &ctx.actor, chat, ctx.project_id, agent_id)
            .await?;
    let ids: Vec<Uuid> = kbs
        .iter()
        .map(|s| Uuid::parse_str(s).map_err(|_| RoutingError::Invalid("part source identity")))
        .collect::<std::result::Result<_, _>>()?;
    if ids.is_empty() || ids.iter().any(|id| !allowed.contains(id)) {
        return Err(RoutingError::Forbidden.into());
    }
    if state
        .rbac
        .denied_kb_doc_ids(&state.pg, &ctx.actor, &ids)
        .await?
        .iter()
        .any(|id| !denied.contains(&id.to_string()))
    {
        return Err(RoutingError::NeedsReplan.into());
    }
    if routing::synthesis_mode(state).await? != "per_part" || routing::search_enabled(state).await?
    {
        return Err(RoutingError::NeedsReplan.into());
    }
    Ok(())
}

async fn setting(
    state: &AppState,
    key: &str,
    default: u64,
    ceiling: u64,
    zero_disables: bool,
) -> Result<u64> {
    let value = match crate::config::runtime::get(&state.pg, key).await? {
        Some(v) => v
            .value
            .parse::<u64>()
            .map_err(|_| RoutingError::Invalid("invalid synthesis limit"))?,
        None => default,
    };
    Ok(if value == 0 && zero_disables {
        86400
    } else {
        value.min(ceiling)
    })
}

async fn part(
    state: &AppState,
    run: &RoutedRun,
    base: GenerateRequest,
    kbs: &[String],
    denied: &[String],
    asst: Uuid,
    tx: mpsc::Sender<Event>,
    sem: &Semaphore,
    idle: std::time::Duration,
    deadline: tokio::time::Instant,
    max_cont: u64,
) -> Result<()> {
    let _permit = sem.acquire().await.map_err(|_| RoutingError::Cancelled)?;
    let mut text = String::new();
    let mut reasoning_bytes = 0usize;
    let mut total = GenTerminal::default();
    for segment in 0..=max_cont {
        sources(state, run, kbs, denied).await?;
        let mut req = base.clone();
        if segment > 0 {
            req.messages = continuation_messages(&base.messages, &text);
        }
        let wait = idle.min(deadline.saturating_duration_since(tokio::time::Instant::now()));
        let mut stream = tokio::time::timeout(
            wait,
            Box::pin(routing::Generation::start(state, &req, Some(run), asst)),
        )
        .await
        .map_err(|_| AppError::Unavailable("part generation did not start".into()))??;
        let mut head = String::new();
        let mut head_open = true;
        let before = text.len();
        let terminal = loop {
            let wait = idle.min(deadline.saturating_duration_since(tokio::time::Instant::now()));
            let event = tokio::time::timeout(wait, stream.recv())
                .await
                .map_err(|_| AppError::Unavailable("part generation stalled".into()))??
                .ok_or(RoutingError::Invalid(
                    "part ended without a terminal report",
                ))?;
            match event {
                GenEvent::Token { delta } => {
                    if delta.len() > 65536
                        || text
                            .len()
                            .saturating_add(head.len())
                            .saturating_add(delta.len())
                            > 262144
                    {
                        return Err(RoutingError::Invalid("part output exceeds limit").into());
                    }
                    let clean = if head_open {
                        head.push_str(&delta);
                        match lead_gate_ready(&head, 4096) {
                            Some(s) => {
                                head_open = false;
                                head.clear();
                                s
                            }
                            None => continue,
                        }
                    } else {
                        delta
                    };
                    text.push_str(&clean);
                    tx.send(Event::Token(clean))
                        .await
                        .map_err(|_| RoutingError::Cancelled)?;
                }
                GenEvent::Reasoning { delta } => {
                    reasoning_bytes = reasoning_bytes.saturating_add(delta.len());
                    if reasoning_bytes > 262144 {
                        return Err(RoutingError::Invalid("part reasoning exceeds limit").into());
                    }
                    tx.send(Event::Reasoning(delta))
                        .await
                        .map_err(|_| RoutingError::Cancelled)?;
                }
                GenEvent::Done {
                    usage,
                    finish_reason,
                    ..
                } => {
                    if !matches!(
                        finish_reason.as_deref(),
                        Some("stop" | "length" | "max_tokens" | "max_output_tokens")
                    ) {
                        return Err(RoutingError::Invalid("invalid part finish reason").into());
                    }
                    total.prompt_tokens = sum_opt(total.prompt_tokens, usage.prompt_tokens);
                    total.completion_tokens =
                        sum_opt(total.completion_tokens, usage.completion_tokens);
                    total.reasoning_tokens =
                        sum_opt(total.reasoning_tokens, usage.reasoning_tokens);
                    break finish_reason;
                }
                GenEvent::Error { .. } => {
                    return Err(AppError::Unavailable("part generation failed".into()))
                }
                GenEvent::ToolCall { .. } => {
                    return Err(
                        RoutingError::Incompatible("per-part tools are not advertised").into(),
                    )
                }
            }
        };
        if head_open && !head.is_empty() {
            let clean = strip_tool_leak(&head);
            text.push_str(&clean);
            tx.send(Event::Token(clean))
                .await
                .map_err(|_| RoutingError::Cancelled)?;
        }
        total.finish = terminal;
        if !is_truncation(total.finish.as_deref()) || segment == max_cont || text.len() == before {
            break;
        }
    }
    total.model = Some(run.plan().assignments[&CallPurpose::Final].model.clone());
    tx.send(Event::Done(total))
        .await
        .map_err(|_| RoutingError::Cancelled)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn synthesize(
    state: &AppState,
    run: &RoutedRun,
    prompt: &str,
    memory: &[String],
    gk: bool,
    sampling: &Sampling,
    history: &[Value],
    parts: &[ml::SynthPart],
    kbs: &[String],
    denied: &[String],
    turn: Uuid,
    asst: Uuid,
    tx: &mpsc::Sender<ServerFrame>,
) -> Result<PerPartOut> {
    if parts.is_empty() || parts.len() > 16 {
        return Err(RoutingError::Incompatible("per-part count must be between 1 and 16").into());
    }
    if history
        .iter()
        .any(|m| m["role"] == "tool" || m.get("tool_calls").is_some())
    {
        return Err(
            RoutingError::Incompatible("tool evidence requires per-part attribution").into(),
        );
    }
    sources(state, run, kbs, denied).await?;
    let (capacity, max_output) = run.capacity(state, CallPurpose::Final).await?;
    let mut sampling = sampling.clone();
    sampling.max_tokens = Some(
        sampling.max_tokens.unwrap_or(4096).max(1).min(
            max_output.min(4096).min(
                run.plan()
                    .constraints
                    .max_output_tokens
                    .unwrap_or(max_output),
            ) as i32,
        ),
    );
    sampling.reasoning_effort = run.plan().assignments[&CallPurpose::Final].effort.clone();
    let ctx = run.context(CallPurpose::Final);
    let mut requests = Vec::with_capacity(parts.len());
    for p in parts {
        if p.title.trim().is_empty() || p.title.len() > 2048 {
            return Err(RoutingError::Invalid("invalid part title").into());
        }
        let mut messages = history.to_vec();
        let first = messages
            .first_mut()
            .filter(|m| m["role"] == "system")
            .ok_or(RoutingError::Invalid("part history has no system message"))?;
        first["content"] = json!(compose::build_system(
            prompt,
            &ctx.actor,
            &[],
            memory,
            Some(&p.context),
            false,
            gk,
            None
        ));
        messages.push(json!({"role":"user","content":format!("Answer ONLY this part of my request: {}. Use its supplied evidence. Do not answer the other parts.",p.title)}));
        let input = serde_json::to_vec(&json!({"messages":messages,"tools":null}))
            .map_err(|_| RoutingError::Invalid("part messages"))?
            .len()
            .saturating_add(1024);
        if input.saturating_add(sampling.max_tokens.unwrap() as usize) > capacity as usize {
            return Err(RoutingError::Incompatible("part exceeds pinned context capacity").into());
        }
        requests.push(GenerateRequest {
            messages,
            sampling: sampling.clone(),
            model: None,
            tools: None,
            overrides: Default::default(),
        });
    }
    let idle = std::time::Duration::from_secs(
        setting(state, "rag.synthesis_idle_timeout_secs", 120, 86400, true).await?,
    );
    let deadline = tokio::time::Instant::now()
        + std::time::Duration::from_secs(
            setting(state, "rag.synthesis_part_max_secs", 300, 86400, true).await?,
        );
    let max_cont = setting(state, "chat.answer_max_continuations", 4, 4, false).await?;
    let sem = Semaphore::new(3);
    let mut workers = FuturesUnordered::new();
    let mut receivers = Vec::new();
    for request in requests {
        let (ptx, prx) = mpsc::channel(16);
        receivers.push(prx);
        let sem = &sem;
        workers.push(Box::pin(async move {
            run.guard(Box::pin(async {
                tokio::time::timeout_at(
                    deadline,
                    Box::pin(part(
                        state, run, request, kbs, denied, asst, ptx, sem, idle, deadline, max_cont,
                    )),
                )
                .await
                .map_err(|_| AppError::Unavailable("per-part synthesis deadline exceeded".into()))?
            }))
            .await
        }));
    }
    let mut out = PerPartOut {
        acc: String::new(),
        reasoning: String::new(),
        prompt_tokens: None,
        completion_tokens: None,
        reasoning_tokens: None,
        model: None,
        finish_reason: None,
        detached: false,
        cancelled: false,
    };
    let result:Result<()>=tokio::time::timeout_at(deadline,async {
        let mut flushed=std::time::Instant::now();
        for (index,(mut rx,p)) in receivers.into_iter().zip(parts).enumerate() {
            let header=format!("{}## {}\n\n",if index==0 {""} else {"\n\n"},strip_tool_leak(&p.title));
            out.acc.push_str(&header);
            if !out.detached && tx.send(ServerFrame::ChatToken {turn_id:turn,delta:header}).await.is_err() {out.detached=true;}
            loop {
                tokio::select! {biased;
                    done=workers.next(),if !workers.is_empty()=>{done.ok_or(RoutingError::Invalid("part worker missing"))??;},
                    event=rx.recv()=>match event.ok_or(RoutingError::Invalid("part channel ended before completion"))? {
                        Event::Token(delta)=>{
                            out.acc.push_str(&delta);
                            if !out.detached && tx.send(ServerFrame::ChatToken {turn_id:turn,delta}).await.is_err() {out.detached=true;}
                            if flushed.elapsed()>=std::time::Duration::from_millis(750) {
                                sqlx::query("UPDATE messages SET content=$2 WHERE id=$1").bind(asst).bind(&out.acc).execute(&state.pg).await?;
                                flushed=std::time::Instant::now();
                            }
                        },
                        Event::Reasoning(delta)=>{out.reasoning.push_str(&delta);if !out.detached && tx.send(ServerFrame::ChatReasoning {turn_id:turn,delta}).await.is_err() {out.detached=true;}},
                        Event::Done(t)=>{
                            out.prompt_tokens=sum_opt(out.prompt_tokens,t.prompt_tokens);out.completion_tokens=sum_opt(out.completion_tokens,t.completion_tokens);
                            out.reasoning_tokens=sum_opt(out.reasoning_tokens,t.reasoning_tokens);out.model=t.model;
                            if out.finish_reason.is_none() || is_truncation(t.finish.as_deref()) {out.finish_reason=t.finish;}
                            break;
                        },
                    }
                }
            }
            sqlx::query("UPDATE messages SET content=$2 WHERE id=$1").bind(asst).bind(&out.acc).execute(&state.pg).await?;
        }
        while let Some(done)=workers.next().await {done?;}
        Ok(())
    }).await.map_err(|_|AppError::Unavailable("per-part synthesis deadline exceeded".into())).and_then(|r|r);
    // Dropping these futures closes all sibling streams before returning an error.
    drop(workers);
    if result.is_err() {
        let _ = sqlx::query("UPDATE messages SET content=$2 WHERE id=$1")
            .bind(asst)
            .bind(&out.acc)
            .execute(&state.pg)
            .await;
    }
    result?;
    Ok(out)
}
