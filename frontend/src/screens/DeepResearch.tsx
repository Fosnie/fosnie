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

// Deep Research home: a centred input — source
// choice (web | files | hybrid), question and report template (with a structure
// preview; there is a single deep mode, no depth choice) — then the lightweight
// plan gate: ambiguity triage chips when the question is unclear against
// the scope, else a one-line scope summary. Corpus modes require an explicit
// library choice before Start. A dormant web-search connector
// surfaces as the honest refusal (the 403 from prepare) for web/hybrid; a
// files-only run is air-gap-safe and never gated. Recent runs listed below.

import { useEffect, useRef, useState } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import {
  prepareResearch,
  startResearch,
  useResearchChats,
  useResearchTemplate,
  useResearchTemplates,
  type ResearchPrepareOut,
  type ResearchRefineParams,
  type ResearchRequestBody,
  type TriageOption,
} from "@/api/client";
import { AnimatePresence, motion } from "motion/react";
import { Icon } from "@/components/icons";
import { Dropdown } from "@/components/Dropdown";
import { TemplatePreview } from "@/components/TemplatePreview";
import { NeuralBackground } from "@/components/NeuralBackground";
import { slideDownVariants, spring } from "@/app/motion";

// The report templates come from the catalogue (GET /api/research/templates): the
// four built-ins plus the user's own. Editing them lives in Studio; this screen
// only picks one and previews its shape.

type Source = ResearchRequestBody["source"];

// A user-defined template id is a UUID; a built-in is a short slug.
const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** What the preview needs about the chosen template, from whichever source. */
interface TemplatePreview {
  description: string;
  structure: string[];
  outline_mode: "constrained" | "free";
  archived?: boolean;
}

const SOURCES: { id: Source; label: string; icon: keyof typeof Icon }[] = [
  { id: "web", label: "Research the web", icon: "Globe" },
  { id: "files", label: "Research my files", icon: "Docs" },
  { id: "hybrid", label: "Files + web", icon: "Research" },
];

function relTime(iso: string): string {
  const d = Date.now() - new Date(iso).getTime();
  const m = Math.floor(d / 60_000);
  if (m < 60) return `${Math.max(m, 0)}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  return `${Math.floor(h / 24)}d`;
}

export function DeepResearch() {
  const nav = useNavigate();
  const location = useLocation();
  const runs = useResearchChats();
  const catalogue = useResearchTemplates();
  const [question, setQuestion] = useState("");
  const qRef = useRef<HTMLTextAreaElement | null>(null);
  function grow(el: HTMLTextAreaElement) {
    el.style.height = "auto";
    el.style.height = Math.min(el.scrollHeight, 220) + "px";
  }
  const [source, setSource] = useState<Source>("web");
  const [template, setTemplate] = useState<ResearchRequestBody["template"]>("exploration");
  const [kbIds, setKbIds] = useState<string[]>([]); // [] ⇒ the whole readable scope
  const [refinements, setRefinements] = useState<string[]>([]);
  const [prepared, setPrepared] = useState<ResearchPrepareOut | null>(null);
  const [refusal, setRefusal] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // 'Refine' from a finished/cancelled run: re-open prefilled (router state),
  // then clear it so a manual reload starts blank.
  useEffect(() => {
    const r = (location.state as { refine?: ResearchRefineParams } | null)?.refine;
    if (!r) return;
    setQuestion(r.question ?? "");
    if (r.source) setSource(r.source);
    if (r.template) setTemplate(r.template);
    setKbIds(r.kb_ids ?? []);
    setRefinements(r.refinements ?? []);
    nav(location.pathname, { replace: true, state: null });
  }, [location, nav]);

  const reset = () => {
    setPrepared(null);
    setRefusal(null);
  };

  const body = (overrides: Partial<ResearchRequestBody> = {}): ResearchRequestBody => ({
    question: question.trim(),
    source,
    template,
    kb_ids: kbIds,
    refinements,
    ...overrides,
  });

  async function runPrepare(over: Partial<ResearchRequestBody> = {}) {
    if (!question.trim() || busy) return;
    setBusy(true);
    setRefusal(null);
    try {
      setPrepared(await prepareResearch(body(over)));
    } catch (e) {
      const msg = (e as Error).message || "";
      setPrepared(null);
      setRefusal(
        /dormant|forbidden|not available|403/i.test(msg)
          ? "Web research isn't enabled on this deployment — the web-search connector is dormant (zero-egress default). Ask your administrator to enable it, or research your files instead."
          : msg,
      );
    } finally {
      setBusy(false);
    }
  }

  // Answer a triage chip: narrow the scope (kb_ids) or add a refinement, then
  // re-prepare with triage skipped → the confirm view.
  async function answerChip(opt: TriageOption) {
    let nextKb = kbIds;
    let nextRef = refinements;
    if (opt.kb_ids.length > 0) {
      nextKb = opt.kb_ids;
      setKbIds(nextKb);
    } else if (opt.refinement) {
      nextRef = [...refinements, opt.refinement];
      setRefinements(nextRef);
    }
    await runPrepare({ kb_ids: nextKb, refinements: nextRef, skip_triage: true });
  }

  async function onStart() {
    if (busy) return;
    setBusy(true);
    try {
      const { chat_id } = await startResearch(body());
      nav(`/c/${chat_id}`);
    } catch (e) {
      setRefusal((e as Error).message);
      setBusy(false);
    }
  }

  const showChips = prepared && prepared.questions.length > 0;
  const showConfirm = prepared && prepared.questions.length === 0;
  const corpus = source !== "web";

  // Resolve the picker options and the preview for the current selection. The
  // built-ins group above the user's own; the current template is looked up in
  // the catalogue, or — when it is an archived one an existing chat still points
  // at — fetched by id so it can still be shown (with a badge).
  const builtins = catalogue.data?.builtin ?? [];
  const customs = catalogue.data?.custom ?? [];
  const inCatalogue =
    builtins.find((t) => t.id === template) ?? customs.find((t) => t.id === template);
  const orphaned = !inCatalogue && UUID_RE.test(template);
  const orphanDetail = useResearchTemplate(orphaned ? template : undefined);

  let preview: TemplatePreview | undefined;
  if (inCatalogue) {
    preview = {
      description: inCatalogue.description,
      structure: inCatalogue.structure,
      outline_mode: inCatalogue.outline_mode,
    };
  } else if (orphanDetail.data) {
    preview = {
      description: orphanDetail.data.description,
      structure: orphanDetail.data.skeleton.map((s) => s.heading),
      outline_mode: orphanDetail.data.outline_mode,
      archived: orphanDetail.data.archived,
    };
  }

  const templateOptions = [
    ...builtins.map((t) => ({ value: t.id, label: t.label, group: "Built-in" })),
    ...customs.map((t) => ({ value: t.id, label: t.label, group: "Your templates" })),
    // Keep an archived-but-current template selectable so its report can be
    // re-run/refined; it is not offered to fresh runs otherwise.
    ...(orphaned && orphanDetail.data
      ? [{ value: template, label: `${orphanDetail.data.label} (archived)`, group: "Your templates" }]
      : []),
  ];

  // Preserve the typed question when stepping into the template editor.
  const openManageTemplates = () =>
    nav("/studio/research", {
      state: {
        returnTo: location.pathname,
        refine: { question, source, template, kb_ids: kbIds, refinements } as ResearchRefineParams,
      },
    });

  return (
    <div style={{ position: "relative", height: "100%", overflow: "hidden", isolation: "isolate" }}>
      {/* Same neural-net canvas as the General idle hero, fixed behind the scroll.
          isolation:isolate creates a stacking context so the -z-10 layer stays
          behind the content but in front of the app background (not hidden by it). */}
      <div className="absolute inset-0 -z-10 opacity-70 pointer-events-none"><NeuralBackground /></div>
      <div className="flex h-full flex-col items-center overflow-y-auto px-6 py-10">
      <div className="w-full max-w-2xl" style={{ marginTop: "8vh" }}>
        <div className="dr-hero dot-grid specular-border">
          <div className="text-center">
            <p className="mono text-[0.7rem] uppercase tracking-[0.2em] text-slate">Deep Research</p>
            <h1 className="serif mt-2 text-3xl text-slate-lightest">What should we research?</h1>
            <p className="mt-2 text-sm text-slate">
              A long-form, fully cited report — collected, synthesised and delivered to this workspace.
            </p>
            <p className="mt-2 text-xs text-slate" style={{ opacity: 0.75 }}>
              Works best with a powerful model (Claude, GPT or Gemini) configured for the LLM role.
            </p>
          </div>
        </div>

        {/* Source choice */}
        <div className="mt-8 flex justify-center gap-2">
          {SOURCES.map((s) => {
            const I = Icon[s.icon];
            const active = source === s.id;
            return (
              <button
                key={s.id}
                className={`btn sm ${active ? "btn-gold" : "btn-line"}`}
                onClick={() => {
                  setSource(s.id);
                  setKbIds([]);
                  setRefinements([]);
                  reset();
                }}
              >
                <I size={14} /> {s.label}
              </button>
            );
          })}
        </div>

        {/* Question + controls. marginTop inline because `.composer` sets
            `margin: 0 auto`, which would otherwise zero a Tailwind mt-* class. */}
        <div className="composer" style={{ marginTop: 22, flexDirection: "column", alignItems: "stretch", gap: 10, padding: 14 }}>
          <textarea
            ref={qRef}
            className="chat-in"
            rows={1}
            placeholder={
              corpus
                ? "e.g. What do our contracts say about termination for convenience, and where do they differ?"
                : "e.g. What are the main approaches to running LLMs fully on-premise, and their trade-offs?"
            }
            value={question}
            onChange={(e) => {
              setQuestion(e.target.value);
              grow(e.target);
              reset();
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                void runPrepare();
              }
            }}
          />
          <div className="flex items-center gap-2">
            <Dropdown
              ariaLabel="Report template"
              value={template}
              options={templateOptions}
              disabled={catalogue.isLoading}
              onChange={(v) => { setTemplate(v); reset(); }}
            />
            <button
              className="btn btn-ghost sm"
              onClick={openManageTemplates}
              title="Create, duplicate or edit report templates"
            >
              Manage templates
            </button>
            <button className="btn btn-gold sm" style={{ marginLeft: "auto" }} onClick={() => void runPrepare()} disabled={!question.trim() || busy}>
              {busy && !prepared ? "Checking…" : "Review scope"}
            </button>
          </div>
          {/* What the chosen template produces — the same preview the editor shows. */}
          {preview && (
            <TemplatePreview
              description={preview.description}
              structure={preview.structure}
              outlineMode={preview.outline_mode}
              archived={preview.archived}
            />
          )}
        </div>

        {/* Triage chips (corpus ambiguity) — one screen, tappable */}
        <AnimatePresence>
        {showChips && (
          <motion.div key="chips" className="approval-card mt-3" style={{ display: "flex", flexDirection: "column", gap: 12 }}
            variants={slideDownVariants} initial="initial" animate="animate" exit="exit" transition={spring}>
            <p className="text-sm text-slate-lightest">A couple of quick questions to scope this well:</p>
            {prepared!.questions.map((qn) => (
              <div key={qn.id}>
                <p className="text-sm text-slate-lightest" style={{ marginBottom: 6 }}>{qn.prompt}</p>
                <div className="flex flex-wrap gap-2">
                  {qn.options.map((o, i) => (
                    <button key={i} className="btn btn-line sm" onClick={() => void answerChip(o)} disabled={busy}>
                      {o.label}
                    </button>
                  ))}
                </div>
              </div>
            ))}
            <button
              className="btn btn-ghost sm"
              style={{ alignSelf: "flex-start" }}
              onClick={() => {
                // "Whole scope" = an explicit choice of every library, not a silent
                // empty default. Materialise the full set, then confirm.
                const all = prepared!.scope.map((s) => s.kb_id);
                if (all.length) setKbIds(all);
                void runPrepare({ kb_ids: all, skip_triage: true });
              }}
              disabled={busy}
            >
              Skip — research the whole scope
            </button>
          </motion.div>
        )}

        {/* The plan gate: scope summary, (corpus) library picker, one-tap Start */}
        {showConfirm && (
          <motion.div key="confirm" className="approval-card mt-3" style={{ display: "flex", flexDirection: "column", gap: 10 }}
            variants={slideDownVariants} initial="initial" animate="animate" exit="exit" transition={spring}>
            <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
              <Icon.Research size={16} />
              <span className="text-sm text-slate-lightest" style={{ flex: 1 }}>{prepared!.scope_summary}</span>
              <button
                className="btn btn-gold sm"
                onClick={() => void onStart()}
                disabled={busy || (corpus && kbIds.length === 0)}
                title={corpus && kbIds.length === 0 ? "Choose at least one library first" : undefined}
              >
                <Icon.Play size={14} /> Start
              </button>
            </div>
            {/* Corpus runs require an EXPLICIT library choice — no silent "whole
                corpus". The picker is always shown; an empty selection blocks Start. */}
            {corpus && (
              <div className="flex flex-col gap-1">
                <div className="flex items-center justify-between">
                  <span className="text-xs text-slate">
                    {kbIds.length === 0
                      ? "Choose the libraries to research:"
                      : `${kbIds.length} of ${prepared!.scope.length} ${prepared!.scope.length === 1 ? "library" : "libraries"} selected`}
                  </span>
                  <button
                    className="btn btn-line sm"
                    style={{ padding: "1px 8px", fontSize: "0.7rem" }}
                    onClick={() => {
                      const all = prepared!.scope.map((s) => s.kb_id);
                      setKbIds(kbIds.length === prepared!.scope.length ? [] : all);
                    }}
                  >
                    {kbIds.length === prepared!.scope.length ? "Clear" : `All ${prepared!.scope.length} libraries`}
                  </button>
                </div>
                {prepared!.scope.map((k) => {
                  const checked = kbIds.includes(k.kb_id);
                  return (
                    <label key={k.kb_id} className="flex items-center gap-2 text-sm text-slate-light" style={{ cursor: "pointer" }}>
                      <input
                        type="checkbox"
                        checked={checked}
                        onChange={(e) => {
                          setKbIds(
                            e.target.checked
                              ? Array.from(new Set([...kbIds, k.kb_id]))
                              : kbIds.filter((id) => id !== k.kb_id),
                          );
                        }}
                      />
                      <span style={{ flex: 1 }}>{k.name}</span>
                      <span className="mono text-[0.65rem] text-slate">
                        {k.kind === "project" ? "project · " : ""}{k.doc_count} docs
                      </span>
                    </label>
                  );
                })}
              </div>
            )}
          </motion.div>
        )}
        {refusal && (
          <motion.div key="refusal" className="approval-card mt-3" style={{ borderColor: "rgba(255,120,120,0.35)" }}
            variants={slideDownVariants} initial="initial" animate="animate" exit="exit" transition={spring}>
            <div className="flex items-center gap-2 text-sm text-slate-lightest">
              <Icon.Alert size={15} /> {refusal}
            </div>
          </motion.div>
        )}
        </AnimatePresence>

        {/* Recent runs */}
        <div className="mt-10">
          <p className="side-label mono">Recent research</p>
          {(runs.data ?? []).length === 0 && (
            <p className="mt-2 text-sm text-slate">No research runs yet.</p>
          )}
          <div className="mt-2">
            {(runs.data ?? []).slice(0, 12).map((c) => (
              <div key={c.id} style={{ display: "flex", alignItems: "center", gap: 6 }}>
                <button
                  className="chat-item"
                  style={{ flex: 1 }}
                  onClick={() => nav(`/c/${c.id}`)}
                >
                  <div className="chat-item-main">
                    <span className="chat-title" title={c.title}>{c.title}</span>
                    <span className="chat-meta mono">{relTime(c.created_at)}</span>
                  </div>
                </button>
                {c.research_params && (
                  <button
                    className="btn btn-line sm"
                    title="Re-run with the same scope, edit anything first"
                    onClick={() => nav("/research", { state: { refine: c.research_params } })}
                  >
                    Refine
                  </button>
                )}
              </div>
            ))}
          </div>
        </div>
      </div>
    </div>
    </div>
  );
}
