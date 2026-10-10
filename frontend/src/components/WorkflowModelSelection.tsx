// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
import { Dropdown } from "./Dropdown";
import { useWorkflowRouting, type WorkflowObjective } from "@/api/workflowRouting";

export function WorkflowModelSelection({ value, onChange, routing, changed, disabled }: {
  value: unknown; onChange: (value: unknown) => void;
  routing: ReturnType<typeof useWorkflowRouting>; changed: boolean; disabled: boolean;
}) {
  const { selection, capabilities, models, policies, availableModels, availablePolicies, policy, supported, problem } = routing;
  const mode = value === undefined ? "legacy" : selection?.mode ?? "saved";
  const judge = policy?.policy.classifier_provider_id;
  const judgeModel = models.isError ? undefined : models.data?.find(m => m.id === judge);
  const strategy = policy?.policy.strategy ?? "rules";
  const rollout = policy?.policy.rollout ?? "off";
  const retry = () => { void capabilities.refetch(); if (!routing.ownerCatalog) void routing.viewer.refetch(); if (mode === "manual" || mode === "auto") void models.refetch(); if (mode === "auto") void policies.refetch(); };
  return <section aria-label="Workflow model routing" className="ed-section" style={{ padding: "16px 0" }}>
    <label className="form-label">Model selection</label>
    <Dropdown ariaLabel="Workflow model selection" fullWidth disabled={disabled} value={mode}
      onChange={m => onChange(m === "legacy" ? undefined : m === "manual" ? { mode: m, provider_id: "" }
        : m === "auto" ? { mode: m, objective: "balanced" } : { mode: "default" })}
      options={[
        { value: "legacy", label: "Legacy — existing workflow behavior" },
        ...["default", "manual", "auto"].map((m, i) => ({ value: m,
          label: ["Default — configured default model", "Manual — choose a model", "Auto — routing policy"][i],
          disabled: !supported || !capabilities.data?.selection_modes.includes(m) })),
        ...(mode === "saved" ? [{ value: "saved", label: "Saved configuration — preserved", disabled: true }] : []),
      ]} />
    {mode === "legacy" && <p className="ed-hint">Uses the existing workflow execution path. Selecting Default, Manual or Auto opts future firings into model routing.</p>}
    {mode === "default" && <p className="ed-hint">Uses the configured default model for the answer and its LLM helpers. No judge chooses another model.</p>}
    {selection?.mode === "manual" && <>
      <label className="form-label">Model</label>
      <Dropdown ariaLabel="Workflow model" fullWidth disabled={disabled || models.isPending || models.isError}
        value={selection.provider_id} onChange={provider_id => onChange({ mode: "manual", provider_id })}
        options={[{ value: "", label: "Choose a model", disabled: true },
          ...availableModels.map(m => ({ value: m.id, label: `${m.label ?? m.model ?? m.id} · ${m.model ?? ""}` })),
          ...(selection.provider_id && !availableModels.some(m => m.id === selection.provider_id)
            ? [{ value: selection.provider_id, label: `Saved model unavailable · ${selection.provider_id}`, disabled: true }] : [])]} />
      <p className="ed-hint">Pins the answer and LLM helpers to this model. An incompatible or unavailable model stops the run; the workflow does not silently switch to Auto.</p>
    </>}
    {selection?.mode === "auto" && <>
      <label className="form-label">Objective</label>
      <Dropdown ariaLabel="Workflow routing objective" fullWidth disabled={disabled} value={selection.objective ?? "balanced"}
        onChange={objective => onChange({ ...selection, objective: objective as WorkflowObjective })}
        options={[{ value: "economy", label: "Economy — prefer lower cost" }, { value: "balanced", label: "Balanced — quality and cost" }, { value: "quality", label: "Quality — prefer stronger capability" }]} />
      <label className="form-label">Routing policy</label>
      <Dropdown ariaLabel="Workflow routing policy" fullWidth disabled={disabled || policies.isPending || policies.isError} value={selection.policy_id ?? ""}
        onChange={policy_id => onChange({ mode: "auto", objective: selection.objective ?? "balanced", ...(policy_id ? { policy_id } : {}) })}
        options={[{ value: "", label: "Owner's default policy" }, ...availablePolicies.map(p => ({ value: p.id, label: `${p.label} · ${p.scope === "user" ? "Personal" : "Deployment"}` })),
          ...(selection.policy_id && !availablePolicies.some(p => p.id === selection.policy_id)
            ? [{ value: selection.policy_id, label: `Saved policy unavailable · ${selection.policy_id}`, disabled: true }] : [])]} />
      <p className="ed-hint">Auto uses the policy's rules, model profiles and access limits. The objective is a preference within those limits, not a guarantee of the best answer.</p>
      {policy && <div className="ed-hint" role="status">
        <p>Policy: {policy.label}</p>
        <p>{strategy === "rules" ? "Rules: matches configured conditions; no model judge is called. Without a model match, the checked default is used."
          : strategy === "hybrid" ? "Hybrid: rules run first; a judge assesses the request when no rule selects a model."
          : "Judge: a model assesses the request, then routing selects an eligible model from its profiles."}</p>
        {strategy !== "rules" && <p>Assigned judge: {judge ? judgeModel ? `${judgeModel.label ?? judgeModel.model} · ${judgeModel.model}` : `unavailable in the current catalog (${judge})` : "not configured"}. If the judge fails, routing attempts the checked default within the same budget.</p>}
        <p>{rollout === "active" ? "Active: the routing decision determines the model used."
          : rollout === "shadow" ? "Shadow: routing evaluates a proposal, but the answer uses the default model. Judge evaluation can consume tokens."
          : "Off: the default model answers; rules and judge evaluation are skipped."}</p>
      </div>}
      <p className="ed-hint">Policies shown belong to your account or deployment. Workflows execute as their original owner, whose policy access is checked again at execution.</p>
      {!routing.ownerCatalog && <p className="ed-hint">The owner's personal catalog is unavailable here. Only deployment choices are offered; the owner's default policy is resolved at execution and cannot be previewed here.</p>}
    </>}
    {supported && value !== undefined && !capabilities.data?.enabled && <p role="status" className="ed-hint">Workflow model routing is disabled on this deployment. You can save this configuration, but routed runs will stop until an administrator enables it.</p>}
    {problem && <p role="alert" className="ed-hint">{problem}{!changed && value !== undefined ? " Saving other fields keeps the saved selection unchanged." : ""}</p>}
    {mode === "legacy" && capabilities.isError && <p role="alert" className="ed-hint">Routing settings are unavailable. Legacy behavior stays selected; retry to configure model routing.</p>}
    {(capabilities.isError || (value !== undefined && !supported && !capabilities.isPending) || models.isError || policies.isError)
      && <button type="button" className="btn btn-ghost sm" disabled={disabled} onClick={retry}>Retry workflow routing settings</button>}
    {value !== undefined && <p className="ed-hint">Supports general-mode agents, web search, document reading, projects and libraries. Artefacts and tools that change data remain unsupported. Group delivery shares a guarded link with up to {capabilities.data?.max_recipients ?? 128} captured members who have source access. Queued runs keep their saved configuration; changed agents, sources or membership need a new firing.</p>}
  </section>;
}
