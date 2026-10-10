// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useNavigate, useSearchParams } from "react-router-dom";
import { currentModel, useRoutingModels } from "@/api/documentJobs";
import { answerResearchJob, cancelResearchJob, launchResearchJob, researchJobActive, researchJobKey, setResearchJobsEnabled,
  useResearchJob, useResearchJobCapabilities, useResearchJobScope, type ResearchJobCapabilities, type ResearchJobRequest,
  type ResearchSelection, type ResearchJob, useResearchLaunchReceipt, useResearchHistory } from "@/api/researchJobs";
import { Dropdown } from "./Dropdown";

export function ResearchJobsSetting({ configured, configReady }: { configured: boolean; configReady: boolean }) {
  const caps = useResearchJobCapabilities();
  const qc = useQueryClient();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return <div className="panel mb-4">
    <label className="flex items-center justify-between gap-4">
      <span>Model selection for Research
        <small className="block text-slate">Allow Default, Manual and Auto for queued file, web and hybrid research.</small>
      </span>
      <input type="checkbox" aria-label="Enable model selection for Research" checked={configured}
        disabled={busy || !configReady || !caps.data || caps.isError || caps.data.schema_version !== 1}
        onChange={async e => {
          setBusy(true); setError(null);
          try { await setResearchJobsEnabled(e.target.checked); await Promise.all([
            qc.invalidateQueries({ queryKey: ["research-job-capabilities"] }), qc.invalidateQueries({ queryKey: ["admin-config"] }),
          ]); } catch (e) { setError((e as Error).message); } finally { setBusy(false); }
        }} />
    </label>
    <p className="text-xs text-slate">Turning this off stops further work on these jobs. Web collection also requires the web-search connector. Chat model selection is separate.</p>
    {(error || caps.isError) && <p role="alert">{error ?? "Could not load research routing settings."}</p>}
  </div>;
}

/** This form never calls legacy prepare: triage inference would escape the job budget. */
export function ResearchJobLaunch({ request, onLibraries, capabilities, onBusy }: {
  request: ResearchJobRequest; onLibraries: (ids: string[]) => void; capabilities?: ResearchJobCapabilities;
  onBusy: (busy: boolean) => void;
}) {
  const nav = useNavigate();
  const [search, setSearch] = useSearchParams();
  const receiptId = search.get("research_request");
  const qc = useQueryClient();
  const corpus = request.source !== "web";
  const web = request.source !== "files";
  const libraries = useResearchJobScope(corpus);
  const [mode, setMode] = useState<ResearchSelection["mode"]>("default");
  const [provider, setProvider] = useState("");
  const [clarify, setClarify] = useState(false);
  const [verify, setVerify] = useState(request.verify ?? false);
  const [deepen, setDeepen] = useState(request.deepen ?? false);
  const [busy, setBusy] = useState(false);
  const receipt = useResearchLaunchReceipt(receiptId, !busy);
  const inFlight = useRef(false);
  const [error, setError] = useState<string | null>(null);
  const enabled = capabilities?.schema_version === 1 && capabilities.enabled && capabilities.sources.includes(request.source);
  const models = useRoutingModels(!!enabled && mode === "manual");
  const choices = (models.data ?? []).filter(currentModel);
  const customTemplate = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(request.template);
  const templateSupported = capabilities?.templates.includes(request.template) || (capabilities?.custom_templates && customTemplate);
  const bytes = (s: string) => new TextEncoder().encode(s).length;
  const invalidText = !request.question.trim() || bytes(request.question.trim()) > 4096
    || request.refinements.length > 8 || request.refinements.some(r => bytes(r) > 512);
  const selectedCount = (libraries.data ?? []).filter(k => request.kb_ids.includes(k.id)).reduce((n, k) => n + k.doc_count, 0);
  const invalidScope = corpus && (libraries.isError || !request.kb_ids.length || request.kb_ids.length > 4
    || selectedCount < 1 || selectedCount > (capabilities?.maximum_documents ?? 4)
    || request.kb_ids.some(id => !libraries.data?.some(k => k.id === id)));
  const invalidModel = !capabilities?.selection_modes.includes(mode)
    || (mode === "manual" && (models.isError || !choices.some(p => p.id === provider)));
  const disabled = busy || !!receiptId || !enabled || !templateSupported || invalidScope || invalidText || invalidModel || (web && !capabilities?.web_enabled)
    || (clarify && (!capabilities?.clarification || request.refinements.length > 5))
    || (verify && !capabilities?.verification) || (deepen && capabilities?.deepening !== "existing_sources_v1");
  async function start() {
    if (inFlight.current || disabled) return;
    inFlight.current = true; setBusy(true); onBusy(true); setError(null);
    try {
      const requestId = capabilities?.idempotent_launch ? crypto.randomUUID() : undefined;
      if (requestId) { const next = new URLSearchParams(search); next.set("research_request", requestId); setSearch(next, { replace: true }); }
      const selection: ResearchSelection = mode === "manual" ? { mode, provider_id: provider }
        : mode === "auto" ? { mode, objective: "balanced" } : { mode };
      const { verify: _verify, deepen: _deepen, ...baseRequest } = request;
      const result = await launchResearchJob({ ...baseRequest, question: request.question.trim(),
        ...(verify ? { verify: true } : {}), ...(deepen ? { deepen: true } : {}) }, selection, clarify, requestId);
      void qc.invalidateQueries({ queryKey: ["chats"] });
      void qc.invalidateQueries({ queryKey: ["research-history"] });
      nav(`/c/${result.chat_id}`);
    } catch (e) {
      setError(`${(e as Error).message} No automatic retry was sent. If the connection failed, the job may already have started. ${capabilities?.idempotent_launch ? "Check the saved launch below." : "Check Recent research before trying again."}`);
      inFlight.current = false; setBusy(false); onBusy(false);
    }
  }
  return <section className="document-job mt-3" aria-label="Research model selection">
    <h3 className="side-label mono">Research</h3>
    {receiptId && !busy && <div className="panel" aria-label="Research launch recovery">
      <p>Saved launch: {receipt.isError ? "status unavailable" : receipt.data?.status ?? "checking"}</p>
      <p className="gd-note">This page keeps the launch ID after reload. Checking it does not start research or call a model.</p>
      {receipt.isError && <p role="alert">Could not look up this launch. Retry the lookup or check Research job history before starting a separate request.</p>}
      {!receipt.isError && receipt.data?.status === "planning" && <p>Model planning is still pending. Wait for this launch to finish.</p>}
      {!receipt.isError && ["failed", "interrupted"].includes(receipt.data?.status ?? "") && <p>This launch stopped. Its ID will not restart planning. Some planning work may already have been charged.</p>}
      {!receipt.isError && receipt.data?.status === "succeeded" && receipt.data.chat_id && <button className="btn btn-gold sm"
        onClick={() => nav(`/c/${receipt.data!.chat_id}`)}>Open existing research</button>}
      <button className="btn btn-line sm" disabled={receipt.isFetching} onClick={() => void receipt.refetch()}>Check saved launch</button>
      {receipt.data?.status !== "planning" && !receipt.isLoading && <button className="btn btn-line sm" onClick={() => {
        const next = new URLSearchParams(search); next.delete("research_request"); setSearch(next, { replace: true }); setError(null);
      }}>Prepare a separate request</button>}
    </div>}
    {corpus && <p className="gd-note">Choose libraries containing 1–{capabilities?.maximum_documents ?? 4} ready documents in total, up to 4 MiB each.
      All selected documents must fit the report limits; larger collections are rejected, not sampled.
    </p>}
    <p className="gd-note">{capabilities?.custom_templates ? "Built-in and compatible custom templates are supported." : "Built-in templates only."}</p>
    {capabilities?.verification && <>
      <label className="flex items-center gap-2"><input type="checkbox" checked={verify} disabled={busy}
        onChange={e => setVerify(e.target.checked)} />Check report against source evidence</label>
      {verify && <p className="gd-note">Uses the configured verification service within this job's budget. The report retains flagged passages for your review.
        A service failure stops publication. This checks support in the collected evidence; it does not establish that the sources themselves are true.</p>}
    </>}
    {capabilities?.deepening === "existing_sources_v1" && <>
      <label className="flex items-center gap-2"><input type="checkbox" checked={deepen} disabled={busy}
        onChange={e => setDeepen(e.target.checked)} />Re-examine sources for each section</label>
      {deepen && <p className="gd-note">One additional planning pass per section looks for missed evidence and gaps in the selected sources.
        It does not search for more sources. The original budget and deadline apply.</p>}
    </>}
    {capabilities?.clarification && <>
      <label className="flex items-center gap-2"><input type="checkbox" checked={clarify} disabled={busy}
        onChange={e => setClarify(e.target.checked)} />Ask clarifying questions before research</label>
      {clarify && <p className="gd-note">One review may ask up to three questions before collecting evidence. It uses this job's selected planning model and budget.
        Answer on the research page before the job deadline; waiting time counts toward the ten-minute limit.</p>}
      {clarify && request.refinements.length > 5 && <p role="alert">Clarification requires at most five existing refinements.</p>}
    </>}
    {customTemplate && capabilities?.custom_templates && <p className="gd-note">Custom templates need 1–{capabilities.maximum_template_sections ?? 8} fixed sections with headings and briefs, or a free outline with no skeleton.
      Add writing instructions; expansion and executive-summary placeholders are not supported yet. The server checks size and access before launch.
      Editing a template during its run stops that job.</p>}
    {web && <p className="gd-note">One search using your question and up to {capabilities?.maximum_web_pages ?? 4} fetched pages, within the same job budget.
      File contents are never sent as search queries. Reports require usable web evidence; failed collection does not fall back to a files-only report.</p>}
    {web && !capabilities?.web_enabled && <p role="alert">Web research requires an enabled web-search connector. Ask your administrator to enable it.</p>}
    <p className="gd-note">The server checks file access and size before starting. You will receive the completed report; partial drafts are not shown.</p>
    {!enabled && <p role="alert">Research with model selection is unavailable. Refresh the routing settings before starting.</p>}
    {capabilities && !templateSupported && <p role="alert">Choose a supported built-in report template to start research.</p>}
    <Dropdown ariaLabel="Research model selection" value={mode} onChange={setMode} fullWidth disabled={busy || !enabled}
      options={([
        { value: "default", label: "Default — configured model" }, { value: "manual", label: "Manual — choose a model" },
        { value: "auto", label: "Auto — routing policy" },
      ] as const).filter(o => capabilities?.selection_modes.includes(o.value))} />
    {mode === "manual" && <Dropdown ariaLabel="Research model" value={provider} onChange={setProvider} fullWidth disabled={busy || !enabled}
      options={[{ value: "", label: models.isLoading ? "Loading models…" : "Choose a model", disabled: true },
        ...choices.map(p => ({ value: p.id, label: `${p.label || p.model}${p.label && p.model ? ` · ${p.model}` : ""}` }))]} />}
    {mode === "manual" && models.isError && <p role="alert">Could not load models. <button onClick={() => void models.refetch()}>Retry models</button></p>}
    {mode === "manual" && !models.isLoading && !models.isError && choices.length === 0 && <p>No models with current routing profiles are available.</p>}
    <p className="gd-note">{mode === "auto"
      ? "Uses the configured rules and judge, where enabled, based on task metadata. Question and document contents are not assessed for difficulty. Off and shadow policies use the default model."
      : "Uses one selected model for planning, writing, summary and title, without a routing judge. OCR uses its separately configured service."}</p>
    {corpus && <><fieldset disabled={busy} className="flex flex-col gap-2">
      <legend>Choose libraries</legend>
      {libraries.isError ? <p role="alert">Could not load libraries. <button onClick={() => void libraries.refetch()}>Retry libraries</button></p>
        : libraries.isLoading ? <p>Loading libraries…</p>
        : !libraries.data?.length ? <p>No readable libraries are available.</p>
        : libraries.data.map(k => <label key={k.id} className="flex items-center gap-2">
          <input type="checkbox" checked={request.kb_ids.includes(k.id)} onChange={e => onLibraries(e.target.checked
            ? [...request.kb_ids, k.id] : request.kb_ids.filter(id => id !== k.id))} />{k.name}
          <small className="text-slate">{k.kind === "project" ? "project · " : ""}{k.doc_count} ready documents</small>
        </label>)}
    </fieldset>
    {request.kb_ids.length > 0 && !libraries.isError && <p className="gd-note">{selectedCount} ready documents selected. The server rechecks the full inventory before starting.</p>}
    {selectedCount > (capabilities?.maximum_documents ?? 4) && <p role="alert">This selection exceeds the document limit. Choose smaller libraries.</p>}
    {libraries.data && request.kb_ids.some(id => !libraries.data?.some(k => k.id === id)) && <p role="alert">A previously selected library is no longer available. <button disabled={busy} onClick={() => onLibraries(request.kb_ids.filter(id => libraries.data?.some(k => k.id === id)))}>Remove unavailable libraries</button></p>}
    {request.kb_ids.length > 4 && <p role="alert">Choose at most four libraries, containing at most four documents in total.</p>}
    </>}
    {request.question.trim() && invalidText && <p role="alert">Shorten the question to 4096 UTF-8 bytes and use at most eight refinements of 512 bytes each.</p>}
    <button className="btn btn-gold sm" disabled={disabled} onClick={() => void start()}>{busy ? "Starting research…" : `Start ${request.source === "files" ? "file" : request.source} research`}</button>
    {error && <p role="alert">{error}</p>}
  </section>;
}

export function ResearchJobHistory() {
  const [pages, setPages] = useState<string[]>([]);
  const history = useResearchHistory(pages.at(-1));
  const nav = useNavigate(); const qc = useQueryClient();
  const [cancelling, setCancelling] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  return <section className="mt-10" aria-label="Research job history">
    <p className="side-label mono">Research job history</p>
    <button className="btn btn-line sm" disabled={history.isFetching} onClick={() => void history.refetch()}>Refresh research history</button>
    {history.isLoading && <p>Loading research jobs…</p>}
    {history.isError && <p role="alert">Could not load research history. Refresh to try again.</p>}
    {!history.isError && history.data && <>
      {history.data.items.length === 0 && <p>No research jobs on this page.</p>}
      <ul>{history.data.items.map(item => <li key={item.job_id} className="panel mt-2">
        <p>{item.accessible ? item.title : "Research details unavailable"} · {item.status}</p>
        <p className="gd-note">{new Date(item.created_at).toLocaleString()}{item.selection ? ` · ${item.selection.mode}` : ""}</p>
        {item.accessible && item.chat_id && <button className="btn btn-line sm" onClick={() => nav(`/c/${item.chat_id}`)}>Open research</button>}
        {["queued", "running", "awaiting_input"].includes(item.status) && <button className="btn btn-line sm" disabled={!!cancelling}
          onClick={async () => {
            if (cancelling) return; setCancelling(item.job_id); setError(null);
            try { await cancelResearchJob(item.job_id); await qc.invalidateQueries({ queryKey: ["research-history"] });
              void qc.invalidateQueries({ queryKey: researchJobKey(item.job_id) });
            } catch (e) { setError((e as Error).message); } finally { setCancelling(null); }
          }}>{cancelling === item.job_id ? "Cancelling…" : "Cancel job"}</button>}
      </li>)}</ul>
      <div className="flex gap-2">
        {pages.length > 0 && <button className="btn btn-line sm" onClick={() => setPages(p => p.slice(0, -1))}>Newer jobs</button>}
        {history.data.next_cursor && <button className="btn btn-line sm" onClick={() => setPages(p => [...p, history.data!.next_cursor!])}>Older jobs</button>}
      </div>
    </>}
    {error && <p role="alert">{error}</p>}
  </section>;
}

export function ResearchJobStatus({ jobId, chatId }: { jobId: string; chatId: string }) {
  const job = useResearchJob(jobId);
  const qc = useQueryClient();
  const [busy, setBusy] = useState(false);
  const inFlight = useRef(false);
  const [error, setError] = useState<string | null>(null);
  const [cancelled, setCancelled] = useState(false);
  const matches = job.data?.chat_id === chatId && job.data?.job_id === jobId;
  const data = !job.isError && matches ? job.data : undefined;
  const refreshed = useRef("");
  useEffect(() => {
    if (!data || researchJobActive(data)) return;
    const revision = `${data.job_id}:${data.status}:${data.message_id}`;
    if (refreshed.current === revision) return;
    refreshed.current = revision;
    for (const key of [["chat-messages", chatId], ["artefacts", chatId], ["chats"]]) void qc.invalidateQueries({ queryKey: key });
  }, [data, chatId, qc]);
  async function cancel() {
    if (inFlight.current) return;
    inFlight.current = true; setBusy(true); setError(null);
    try {
      const result = await cancelResearchJob(jobId);
      setCancelled(result.status === "cancelled");
      await qc.invalidateQueries({ queryKey: researchJobKey(jobId) });
    } catch (e) { setError((e as Error).message); }
    finally { inFlight.current = false; setBusy(false); }
  }
  return <section className="document-job" aria-label="Research job status" aria-live="polite">
    {job.isLoading && <p>Loading research status…</p>}
    {(job.isError || (job.data && !matches)) && <p role="alert">Could not access this research job. <button onClick={() => void job.refetch()}>Retry status</button></p>}
    {data && <>
      <p>Research: {data.status}</p>
      {data.options?.deepen && <p className="gd-note">Includes a section-by-section review of the selected evidence.</p>}
      {data.verification && <p className="gd-note">Source grounding checked: {data.verification.flagged} flagged passages for review.
        The report is checked against collected evidence, not independently established facts.</p>}
      {data.options?.verify && !data.verification && researchJobActive(data) && <p className="gd-note">Source grounding check required before publication.</p>}
      <p className="gd-note">Selection: {data.selection.mode}. Calls including reservations: {data.usage.calls}.
        {data.usage.unknown_cost_calls > 0 && " Some call costs are unknown."}
        {data.usage.exhausted && " The job budget is exhausted."}</p>
      {data.reason_codes.includes("shadow_default_planned") && <p className="gd-note">Shadow mode: the default model executes this research.</p>}
      {data.reason_codes.includes("routing_off") && <p className="gd-note">Routing is off: the default model executes this research.</p>}
      {researchJobActive(data) && data.status !== "awaiting_input" && <p className="gd-note">The completed report will appear here. You can leave this page and return later.</p>}
      {data.status === "awaiting_input" && (data.clarification
        ? <ResearchClarification key={`${jobId}:${data.clarification.id}`} job={data} />
        : <p role="alert">Clarification is unavailable. Refresh the job status.</p>)}
      {["failed", "interrupted", "cancelled"].includes(data.status) && <p className="gd-note">No completed report was published. This job will not restart automatically.</p>}
    </>}
    {cancelled && !data && <p>Research cancellation confirmed.</p>}
    {!cancelled && (researchJobActive(data) || job.isError) && <button className="btn btn-line sm" disabled={busy} onClick={() => void cancel()}>
      {busy ? "Cancelling…" : "Cancel research"}
    </button>}
    {error && <p role="alert">{error}</p>}
  </section>;
}

function ResearchClarification({ job }: { job: ResearchJob }) {
  const clarification = job.clarification!;
  const qc = useQueryClient();
  const [answers, setAnswers] = useState(() => clarification.questions.map(() => ""));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inFlight = useRef(false);
  const invalid = answers.some(a => !a.trim() || new TextEncoder().encode(a).length > 256 || /[\u0000-\u001f\u007f-\u009f]/.test(a));
  const expired = !!job.deadline && Date.parse(job.deadline) <= Date.now();
  function answer(index: number, value: string) {
    setAnswers(current => current.map((a, i) => i === index ? value : a));
  }
  return <form aria-label="Research clarification" onSubmit={async e => {
    e.preventDefault(); if (inFlight.current || invalid || expired) return;
    inFlight.current = true; setBusy(true); setError(null);
    try {
      await answerResearchJob(job.job_id, clarification.id, answers);
      await qc.invalidateQueries({ queryKey: researchJobKey(job.job_id) });
    } catch (e) {
      setError(`${(e as Error).message} Refresh status before changing answers. Sending the same answers again is safe.`);
    } finally { inFlight.current = false; setBusy(false); }
  }}>
    <p>Clarify your request to continue this research.</p>
    {job.deadline && <p className="gd-note">Answer before {new Date(job.deadline).toLocaleString()}. The original budget and deadline apply.</p>}
    <fieldset disabled={busy || expired}>
      {clarification.questions.map((q, index) => <div key={index} className="mb-3">
        <label className="block">{q.prompt}<input className="input w-full" value={answers[index]}
          onChange={e => answer(index, e.target.value)} /></label>
        {q.options.map(option => <button type="button" className="btn btn-line sm" key={option}
          aria-pressed={answers[index] === option} onClick={() => answer(index, option)}>{option}</button>)}
      </div>)}
      <p className="gd-note">Answer every question in up to 256 UTF-8 bytes. These answers refine the report within your selected sources.</p>
      <button className="btn btn-gold sm" type="submit" disabled={busy || invalid || expired}>{busy ? "Continuing…" : "Continue research"}</button>
    </fieldset>
    {expired && <p role="alert">The research deadline has passed. This job cannot continue.</p>}
    {error && <p role="alert">{error}</p>}
  </form>;
}
