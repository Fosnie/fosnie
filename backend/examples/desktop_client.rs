// Copyright 2026 Private AI Ltd (SC881079)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! A paired machine, with no window on it.
//!
//! The installed desktop client is the real thing: a window, a tray icon, an
//! updater, a credential in the operating system's store. None of that is
//! involved in whether a folder tool works, and all of it is in the way of
//! finding out. This is the other half — pair, connect a folder, answer the
//! instance's requests against a real disk — and nothing else, so the folder
//! tools can be exercised end to end from a terminal.
//!
//! It is a development tool and is not installed anywhere. What it does with a
//! request is what the client does with one, deliberately: the boundary check
//! here is the check that has to happen on the machine that owns the files, and
//! writing it beside the server that sends the requests is how the two stay
//! honest with each other.
//!
//! ```text
//! cargo run --example desktop_client -- \
//!     --url http://localhost:8080 --code K7M2-QX4D --folder C:\work\demo --tier rw
//! ```
//!
//! Then, in the same run, drive a turn from this connection:
//!
//! ```text
//!     --chat <chat-id> --say "list the folder and tell me what is in it"
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use fosnie_protocol::{ClientFrame, ServerFrame};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::Message;

/// The origin a desktop client presents. Instances admit it by name; a browser
/// cannot claim it.
const ORIGIN: &str = "tauri://localhost";

struct Args {
    url: String,
    code: Option<String>,
    token: Option<String>,
    folder: Option<PathBuf>,
    tier: String,
    chat: Option<String>,
    agent: Option<String>,
    say: Option<String>,
    workspace: Option<String>,
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        url: "http://localhost:8080".into(),
        code: None,
        token: None,
        folder: None,
        tier: "rw".into(),
        chat: None,
        agent: None,
        say: None,
        workspace: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| anyhow!("{flag} needs a value"));
        match flag.as_str() {
            "--url" => args.url = value()?,
            "--code" => args.code = Some(value()?),
            "--token" => args.token = Some(value()?),
            "--folder" => args.folder = Some(PathBuf::from(value()?)),
            "--tier" => args.tier = value()?,
            "--chat" => args.chat = Some(value()?),
            "--agent" => args.agent = Some(value()?),
            "--workspace" => args.workspace = Some(value()?),
            "--say" => args.say = Some(value()?),
            other => bail!("unknown option {other}"),
        }
    }
    args.url = args.url.trim_end_matches('/').to_string();
    Ok(args)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args()?;
    let http = reqwest::Client::builder().build()?;

    let token = match (&args.token, &args.code) {
        (Some(t), _) => t.clone(),
        (None, Some(code)) => pair(&http, &args.url, code).await?,
        (None, None) => bail!("pass --code to pair, or --token if you already have one"),
    };
    println!("device token: {token}");

    // The folder, canonicalised on this machine. Everything after this compares
    // against the real thing, not against what anybody typed.
    let root = match &args.folder {
        Some(f) => {
            let root = std::fs::canonicalize(f)
                .with_context(|| format!("no such folder: {}", f.display()))?;
            let id = connect_folder(&http, &args.url, &token, &root, &args.tier).await?;
            println!("connected {} as {id}", display(&root));
            Some(root)
        }
        None => None,
    };

    let ticket = ws_ticket(&http, &args.url, &token).await?;
    let ws_url = format!(
        "{}/ws?ticket={}",
        args.url.replacen("http", "ws", 1),
        urlencoding(&ticket)
    );
    let mut request = ws_url.into_client_request()?;
    request.headers_mut().insert("Origin", HeaderValue::from_static(ORIGIN));
    let (stream, _) = tokio_tungstenite::connect_async(request).await?;
    let (mut sink, mut source) = stream.split();

    // Frames this process produces off the read path (tool results, output as it
    // arrives) come back through here, so the reader never waits on a write.
    let (out_tx, mut out_rx) = mpsc::channel::<String>(64);

    sink.send(Message::Text(
        ClientFrame::ClientHello {
            client_kind: Some("desktop".into()),
            client_version: Some("stand-in".into()),
            capabilities: vec!["folder".into()],
        }
        .to_json(),
    ))
    .await?;

    if let Some(say) = &args.say {
        // Without a chat this starts one, which is how a conversation that
        // begins on the desktop begins.
        let chat_id = match &args.chat {
            Some(chat) => Some(chat.parse()?),
            None => None,
        };
        sink.send(Message::Text(
            ClientFrame::ChatSend {
                chat_id,
                content: say.clone(),
                agent_id: match &args.agent {
                    Some(a) => Some(a.parse()?),
                    None => None,
                },
                project_id: None,
                attachment_ids: vec![],
                thinking: None,
                reasoning: None,
                llm_provider_id: None,
                workspace_id: match &args.workspace {
                    Some(w) => Some(w.parse()?),
                    None => None,
                },
            }
            .to_json(),
        ))
        .await?;
        println!("> {say}");
    }

    loop {
        tokio::select! {
            outgoing = out_rx.recv() => match outgoing {
                Some(frame) => sink.send(Message::Text(frame)).await?,
                None => break,
            },
            incoming = source.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    handle(&text, root.as_deref(), &out_tx).await;
                }
                Some(Ok(Message::Ping(p))) => sink.send(Message::Pong(p)).await?,
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    eprintln!("socket ended: {e}");
                    break;
                }
                None => break,
            },
        }
    }
    Ok(())
}

/// React to one frame from the instance. Only a request to do something in the
/// folder is acted on; the rest is printed so a run can be read as it happens.
async fn handle(text: &str, root: Option<&Path>, out: &mpsc::Sender<String>) {
    let frame: ServerFrame = match serde_json::from_str(text) {
        Ok(f) => f,
        Err(_) => return,
    };
    match frame {
        ServerFrame::DesktopToolCall { call_id, tool, args, .. } => {
            println!("[call] {tool} {args}");
            let progress = out.clone();
            let reply = match root {
                Some(root) => run(root, &tool, &args, call_id, progress).await,
                None => Err(anyhow!("this client has no folder connected")),
            };
            let frame = match reply {
                Ok(result) => ClientFrame::DesktopToolResult { call_id, ok: true, result },
                Err(e) => ClientFrame::DesktopToolResult {
                    call_id,
                    ok: false,
                    result: json!({ "error": e.to_string() }),
                },
            };
            let _ = out.send(frame.to_json()).await;
        }
        ServerFrame::AgentApproval { tool, summary, detail, run_id, .. } => {
            println!("[approval {run_id}] {tool}: {summary}");
            if let Some(d) = detail {
                println!("           {d}");
            }
            println!("           approve with: POST /api/agent-runs/{run_id}/approve");
        }
        ServerFrame::ChatCreated { chat_id } => println!("[chat] {chat_id}"),
        ServerFrame::ChatStarted { chat_id, .. } => println!("[chat] {chat_id}"),
        ServerFrame::ChatToken { delta, .. } => print!("{delta}"),
        ServerFrame::ChatCompleted { .. } => println!("\n[done]"),
        ServerFrame::ChatTool { name, phase, detail, .. } => {
            println!("[tool] {name} {phase} {}", detail.unwrap_or_default());
        }
        ServerFrame::ChatError { message, .. } => println!("[error] {message}"),
        _ => {}
    }
}

/// Do the work, inside the folder.
///
/// The path is resolved against the real filesystem and checked against the real
/// folder before anything is opened, because the check that counts is the one on
/// the machine that has the files: links, junctions and case-folding are only
/// answerable here. The instance checked the shape of the path already; this
/// checks where it actually leads.
async fn run(
    root: &Path,
    tool: &str,
    args: &Value,
    call_id: uuid::Uuid,
    progress: mpsc::Sender<String>,
) -> Result<Value> {
    let rel = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    match tool {
        "desktop.fs_list" => {
            let dir = within(root, rel, true)?;
            let depth = args.get("depth").and_then(|v| v.as_i64()).unwrap_or(1).clamp(1, 4);
            let mut entries = Vec::new();
            walk(root, &dir, depth, &mut entries)?;
            Ok(json!({ "entries": entries }))
        }
        "desktop.fs_read" => {
            let file = within(root, rel, true)?;
            let bytes = std::fs::read(&file)?;
            match String::from_utf8(bytes) {
                Ok(content) => Ok(json!({ "content": content })),
                Err(e) => Ok(json!({
                    "content": format!("[not text: {} bytes]", e.as_bytes().len()),
                    "binary": true,
                })),
            }
        }
        "desktop.fs_write" => {
            // The folder does not have to contain the file yet, so the check is
            // on where it would go, not on where it is.
            let file = within(root, rel, false)?;
            if let Some(expected) = args.get("old_content_sha256").and_then(|v| v.as_str()) {
                let actual = std::fs::read(&file).ok().map(|b| sha256(&b));
                if actual.as_deref() != Some(expected) {
                    bail!("the file changed since it was read");
                }
            }
            let content = args.get("new_content").and_then(|v| v.as_str()).unwrap_or_default();
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&file, content)?;
            Ok(json!({ "written": display(&file), "bytes": content.len() }))
        }
        "desktop.fs_delete" => {
            let target = within(root, rel, true)?;
            if target == root {
                bail!("the connected folder itself cannot be deleted");
            }
            if target.is_dir() {
                std::fs::remove_dir(&target)?;
            } else {
                std::fs::remove_file(&target)?;
            }
            Ok(json!({ "deleted": display(&target) }))
        }
        "desktop.terminal_run" => {
            let command = args
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("no command"))?;
            let secs = args.get("timeout_secs").and_then(|v| v.as_i64()).unwrap_or(60).max(1);
            terminal(root, command, secs as u64, call_id, progress).await
        }
        other => bail!("this client does not know how to {other}"),
    }
}

/// Resolve a path inside the folder, or refuse.
fn within(root: &Path, rel: &str, must_exist: bool) -> Result<PathBuf> {
    let joined = root.join(rel);
    let resolved = if must_exist {
        std::fs::canonicalize(&joined).with_context(|| format!("no such path: {rel}"))?
    } else {
        // A path that is not there yet is checked through the nearest ancestor
        // that is, so a link partway up cannot be used to land outside.
        let parent = joined.parent().ok_or_else(|| anyhow!("no such path: {rel}"))?;
        let base = std::fs::canonicalize(parent)
            .with_context(|| format!("no such folder for: {rel}"))?;
        base.join(joined.file_name().ok_or_else(|| anyhow!("no file name in {rel}"))?)
    };
    let root = std::fs::canonicalize(root)?;
    if !resolved.starts_with(&root) {
        bail!("that path is outside the connected folder");
    }
    Ok(resolved)
}

fn walk(root: &Path, dir: &Path, depth: i64, out: &mut Vec<Value>) -> Result<()> {
    if depth <= 0 || out.len() > 500 {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        let path = entry.path();
        let shown = path.strip_prefix(root).unwrap_or(&path);
        out.push(json!({
            "path": shown.to_string_lossy(),
            "kind": if meta.is_dir() { "folder" } else { "file" },
            "bytes": meta.len(),
        }));
        if meta.is_dir() {
            walk(root, &path, depth - 1, out)?;
        }
    }
    Ok(())
}

/// Run a command in the folder, sending its output on as it appears.
async fn terminal(
    root: &Path,
    command: &str,
    secs: u64,
    call_id: uuid::Uuid,
    progress: mpsc::Sender<String>,
) -> Result<Value> {
    use tokio::io::AsyncReadExt;

    let mut cmd = if cfg!(windows) {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(command);
        c
    } else {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    cmd.current_dir(root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Nothing this process was given to reach the instance with is passed on to
    // whatever it starts: a command run in a folder has no business inheriting a
    // credential for the account that asked for it.
    for (key, _) in std::env::vars() {
        if key.starts_with("FOSNIE_") || key.starts_with("PAI__") || key == "DATABASE_URL" {
            cmd.env_remove(key);
        }
    }

    let mut child = cmd.spawn()?;
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");

    let streamer = tokio::spawn(async move {
        let mut collected = String::new();
        let mut buf = [0u8; 4096];
        loop {
            match stdout.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                    let _ = progress
                        .send(
                            ClientFrame::DesktopToolProgress { call_id, chunk: chunk.clone() }
                                .to_json(),
                        )
                        .await;
                    collected.push_str(&chunk);
                }
            }
        }
        collected
    });

    let status = match tokio::time::timeout(
        std::time::Duration::from_secs(secs),
        child.wait(),
    )
    .await
    {
        Ok(status) => status?,
        Err(_) => {
            let _ = child.kill().await;
            bail!("the command ran past its {secs}s limit and was stopped");
        }
    };

    let out = streamer.await.unwrap_or_default();
    let mut err = String::new();
    let _ = stderr.read_to_string(&mut err).await;
    Ok(json!({
        "stdout": out,
        "stderr": err,
        "exit_code": status.code().unwrap_or(-1),
    }))
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn display(path: &Path) -> String {
    // Canonicalising on Windows yields the verbatim form (`\\?\C:\…`), which is
    // correct and unreadable. The instance stores what a person would type.
    path.to_string_lossy().trim_start_matches("\\\\?\\").to_string()
}

fn urlencoding(raw: &str) -> String {
    raw.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

async fn pair(http: &reqwest::Client, url: &str, code: &str) -> Result<String> {
    let platform = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let res = http
        .post(format!("{url}/api/device/pair"))
        .json(&json!({ "code": code, "name": "stand-in client", "platform": platform }))
        .send()
        .await?;
    if !res.status().is_success() {
        bail!("pairing was refused ({}): {}", res.status(), res.text().await.unwrap_or_default());
    }
    let body: HashMap<String, Value> = res.json().await?;
    Ok(body
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("no token in the pairing reply"))?
        .to_string())
}

async fn connect_folder(
    http: &reqwest::Client,
    url: &str,
    token: &str,
    root: &Path,
    tier: &str,
) -> Result<String> {
    let res = http
        .post(format!("{url}/api/me/workspaces"))
        .bearer_auth(token)
        .json(&json!({ "path": display(root), "label": "", "tier": tier }))
        .send()
        .await?;
    if !res.status().is_success() {
        bail!(
            "the folder was refused ({}): {}",
            res.status(),
            res.text().await.unwrap_or_default()
        );
    }
    let body: HashMap<String, Value> = res.json().await?;
    Ok(body.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string())
}

async fn ws_ticket(http: &reqwest::Client, url: &str, token: &str) -> Result<String> {
    let res = http.post(format!("{url}/api/ws-ticket")).bearer_auth(token).send().await?;
    if !res.status().is_success() {
        bail!("no socket ticket ({})", res.status());
    }
    let body: HashMap<String, Value> = res.json().await?;
    Ok(body
        .get("ticket")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("no ticket in the reply"))?
        .to_string())
}
