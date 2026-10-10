// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { changeDocumentJob, currentModel, jobActive, jobKey, launchDocumentJob,
  setDocumentJobsEnabled, useDocumentJob, useDocumentJobCapabilities, useRoutingModels,
  type DocumentJobCapabilities, type DocumentSelection } from "@/api/documentJobs";
import { VerificationReport } from "./verificationReport";
import { Dropdown } from "./Dropdown";

export function DocumentJobsSetting({ configured, configReady }: { configured: boolean; configReady: boolean }) {
  const caps = useDocumentJobCapabilities();
  const qc = useQueryClient();
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return <div className="panel mb-4">
    <label className="flex items-center justify-between gap-4">
      <span>Model selection for document verification
        <small className="block text-slate">Allow Default, Manual and Auto for document jobs. Requires a routing policy and configured model profiles.</small>
      </span>
      <input type="checkbox" aria-label="Enable model selection for document verification" checked={configured}
        disabled={saving || !configReady || !caps.data || caps.isError} onChange={async e => {
          setSaving(true); setError(null);
          try { await setDocumentJobsEnabled(e.target.checked); await Promise.all([
            qc.invalidateQueries({ queryKey: ["document-job-capabilities"] }), qc.invalidateQueries({ queryKey: ["admin-config"] }),
          ]); }
          catch (e) { setError((e as Error).message); } finally { setSaving(false); }
        }} />
    </label>
    <p className="text-xs text-slate">Turning this off stops further work on these jobs. Chat model selection is configured separately.</p>
    {(error || caps.isError) && <p role="alert">{error ?? "Could not load document routing settings."}</p>}
  </div>;
}

export function DocumentVerificationJob({ documentId, targetType = "document", jobId, capabilities, docx, repairEnabled, disabled, onJob, onResult }: {
  documentId: string; jobId: string | null; capabilities?: DocumentJobCapabilities;
  targetType?: "document" | "draft";
  docx: boolean; repairEnabled: boolean; disabled: boolean;
  onJob: (id: string) => void; onResult: (id: string | null) => void;
}) {
  const qc = useQueryClient();
  const job = useDocumentJob(jobId);
  const enabled = capabilities?.schema_version === 1 && capabilities.enabled && capabilities.target_types.includes(targetType);
  const models = useRoutingModels(!!enabled);
  const [mode, setMode] = useState<DocumentSelection["mode"]>("default");
  const [provider, setProvider] = useState("");
  const [repair, setRepair] = useState(false);
  const [busy, setBusy] = useState(false);
  const inFlight = useRef(false);
  const [error, setError] = useState<string | null>(null);
  const matchesTarget = job.data && (job.data.target_type ?? "document") === targetType && (job.data.target_id ?? job.data.document_id) === documentId;
  const data = !job.isError && matchesTarget ? job.data : undefined;
  const active = jobActive(data);
  const result = data?.result_id ?? null;
  const refreshed = useRef("");
  useEffect(() => { onResult(result); }, [result, onResult]);
  useEffect(() => {
    if (!data || jobActive(data)) return;
    const revision = `${data.id}:${data.phase}:${data.status}:${data.updated_at}`;
    if (refreshed.current === revision) return;
    refreshed.current = revision;
    const targets = targetType === "document" ? [["document", documentId], ["doc-edits", documentId]] : [["artefacts"]];
    for (const key of [...targets, ["verification-latest", targetType, documentId], ["verification-run", data.result_id]]) {
      void qc.invalidateQueries({ queryKey: key });
    }
  }, [data, documentId, targetType, qc]);
  async function act(action: "launch" | "cancel" | "repair") {
    if (inFlight.current) return;
    inFlight.current = true; setBusy(true); setError(null);
    try {
      if (action === "launch") {
        const selection: DocumentSelection = mode === "manual" ? { mode, provider_id: provider }
          : mode === "auto" ? { mode, objective: "balanced" } : { mode };
        const reply = await launchDocumentJob(documentId, selection, repair && docx && repairEnabled, targetType);
        onResult(null); onJob(reply.job_id);
      } else if (jobId) {
        await changeDocumentJob(jobId, action);
        await qc.invalidateQueries({ queryKey: jobKey(jobId) });
      }
    } catch (e) {
      setError(`${(e as Error).message}${action === "launch" ? " No automatic retry was sent. If the connection failed, the job may already have started." : ""}`);
    } finally { inFlight.current = false; setBusy(false); }
  }
  const choices = (models.data ?? []).filter(currentModel);
  const invalidManual = mode === "manual" && (models.isError || !choices.some(p => p.id === provider));
  return <section className="document-job" aria-label="Document verification">
    <h3 className="side-label mono">{targetType === "draft" ? "Verify generated artefact" : "Verify current document"}</h3>
    {enabled ? <>
      <Dropdown ariaLabel="Verification model selection" value={mode} onChange={setMode} disabled={busy || active}
        fullWidth options={([
          { value: "default", label: "Default — configured model" }, { value: "manual", label: "Manual — choose a model" },
          { value: "auto", label: "Auto — routing policy" },
        ] as const).filter(o => capabilities.selection_modes.includes(o.value))} />
      {mode === "manual" && <Dropdown ariaLabel="Verification model" value={provider} onChange={setProvider} fullWidth disabled={busy || active}
        options={[{ value: "", label: models.isLoading ? "Loading models…" : "Choose a model", disabled: true },
          ...choices.map(p => ({ value: p.id, label: `${p.label || p.model}${p.label && p.model ? ` · ${p.model}` : ""}` }))]} />}
      {mode === "manual" && models.isError && <p role="alert">Could not load available models. <button onClick={() => void models.refetch()}>Retry</button></p>}
      {mode === "manual" && !models.isLoading && !models.isError && choices.length === 0 && <p>No models with current routing profiles are available.</p>}
      <p className="gd-note">{mode === "auto" ? "Uses your configured rules and judge, where enabled, based on the verification task. Document contents are not assessed for difficulty. Off and shadow policies use the default model."
        : mode === "manual" ? "Uses this model for document work. Evidence verification uses its separately configured model."
        : "Uses the default model from your routing policy. No routing judge is called."}</p>
      {targetType === "document" && docx && repairEnabled && <label className="document-job-check"><input type="checkbox" checked={repair} disabled={busy || active}
        onChange={e => setRepair(e.target.checked)} /> Also propose tracked repairs</label>}
      <button className="btn btn-line" onClick={() => void act("launch")}
        disabled={disabled || busy || active || !!jobId && (job.isPending || job.isError) || invalidManual || !capabilities.selection_modes.includes(mode)}>
        {busy ? "Working…" : "Verify draft"}</button>
    </> : <p className="gd-note">Document model selection is currently unavailable. Existing job status and cancellation remain accessible.</p>}
    {error && <p role="alert">{error}</p>}
    {jobId && <div aria-live="polite">
      {job.isPending && <p>Loading verification job…</p>}
      {(job.isError || job.data && !matchesTarget) && <p role="alert">Could not access this document job.
        <button className="btn btn-ghost" onClick={() => void job.refetch()}>Refresh status</button></p>}
      {data && <>
        <p><strong>{data.phase === "repair" ? "Repair" : "Verification"}: {data.status}</strong></p>
        <p className="gd-note">Selection: {data.selection.mode}. {data.planned_document_target ? `Planned model: ${data.planned_document_target.model}${data.planned_document_target.effort ? ` (${data.planned_document_target.effort})` : ""}.` : "Model details unavailable."}</p>
        {data.reason_codes.includes("routing_off") && <p className="gd-note">Auto is off in this policy; using the default model.</p>}
        {data.reason_codes.includes("shadow_default_planned") && <p className="gd-note">Shadow mode: the routing proposal does not change the model used.</p>}
        <p className="gd-note">Accounted calls: {data.usage.calls} (includes reservations). {data.usage.unknown_cost_calls > 0 ? "Some costs are unknown." : ""}</p>
        {["failed", "interrupted", "cancelled"].includes(data.status) && <p className="gd-note">This job has stopped. {result ? "Its verification report is still available below. " : ""}Start a new verification to try again.</p>}
        {targetType === "document" && enabled && capabilities.delayed_repair && repairEnabled && docx && data.status === "succeeded" && data.phase === "verify" && !data.repair && result &&
          <><button className="btn btn-line" disabled={busy || disabled} onClick={() => void act("repair")}>Propose tracked repairs</button>
          <p className="gd-note">Uses the original models and remaining budget. The document must be unchanged and the job must not have expired.</p></>}
      </>}
      {(active || job.isError) && <button className="btn btn-ghost" disabled={busy} onClick={() => void act("cancel")}>Cancel job</button>}
    </div>}
    {result && <VerificationReport key={result} runId={result} />}
  </section>;
}
