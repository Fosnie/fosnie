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

import { toast } from "@/components/dialogs";
import { useEffect, useMemo, useState } from "react";
import { renderPrompt, usePrompt, usePrompts } from "@/api/client";
import { PromptFillForm } from "@/components/PromptFillForm";

const SCOPE_ORDER = ["personal", "project", "global"] as const;

/** Modal: pick a prompt → fill placeholders → render → insert into composer.
 *  `initialId` pre-selects a prompt (the composer "/" picker opens straight on the
 *  fill form for a chosen prompt). */
export function PromptPicker({
  onInsert,
  onAgent,
  onClose,
  initialId,
}: {
  onInsert: (text: string) => void;
  onAgent: (agentId: string) => void;
  onClose: () => void;
  initialId?: string;
}) {
  const prompts = usePrompts();
  const [selected, setSelected] = useState<string | null>(initialId ?? null);
  const detail = usePrompt(selected ?? undefined);
  const [values, setValues] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    setValues({});
  }, [selected]);

  const groups = useMemo(() => {
    const m = new Map<string, typeof prompts.data>();
    (prompts.data ?? []).forEach((p) => {
      (m.get(p.scope) ?? m.set(p.scope, []).get(p.scope)!)!.push(p);
    });
    return SCOPE_ORDER.map((s) => [s, m.get(s) ?? []] as const).filter(([, l]) => l.length);
  }, [prompts.data]);

  async function insert() {
    if (!detail.data || busy) return;
    setBusy(true);
    try {
      const { content } = await renderPrompt(detail.data.id, values);
      onInsert(content);
      if (detail.data.agent_id) onAgent(detail.data.agent_id);
      onClose();
    } catch (e) {
      toast(`Render failed: ${(e as Error).message}`);
      setBusy(false);
    }
  }

  const focused = !!initialId;
  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center overflow-y-auto bg-navy/70 p-6" onClick={onClose}>
      <div className={"my-auto flex w-full gap-4 rounded-2xl border border-navy-lighter bg-navy-light p-6 shadow-xl " + (focused ? "max-w-lg" : "max-w-3xl")} onClick={(e) => e.stopPropagation()}>
        {/* List — hidden when opened focused on one prompt (the "/" flow). */}
        {!focused && (
          <div className="w-56 shrink-0">
            <div className="mb-2 flex items-center justify-between">
              <h2 className="text-sm uppercase tracking-[0.14em] text-slate">Prompts</h2>
              <button onClick={onClose} className="text-slate hover:text-slate-lightest">✕</button>
            </div>
            <div className="max-h-80 overflow-y-auto">
              {prompts.isLoading && <p className="text-xs text-slate">Loading…</p>}
              {prompts.data?.length === 0 && <p className="text-xs text-slate/70">No prompts. Create some in 📋 Prompts.</p>}
              {groups.map(([scope, list]) => (
                <div key={scope} className="mb-2">
                  <div className="px-1 text-[0.65rem] uppercase tracking-wide text-slate/60">{scope}</div>
                  {list!.map((p) => (
                    <button
                      key={p.id}
                      onClick={() => setSelected(p.id)}
                      className={"block w-full truncate rounded px-2 py-1 text-left text-sm " + (selected === p.id ? "bg-navy-lighter text-slate-lightest" : "text-slate hover:text-slate-lightest")}
                    >
                      {p.name}
                    </button>
                  ))}
                </div>
              ))}
            </div>
          </div>
        )}

        {/* Detail / fill */}
        <div className="min-w-0 flex-1">
          {!selected ? (
            <p className="text-sm text-slate/70">Select a prompt.</p>
          ) : detail.isError ? (
            <p className="text-sm text-red">Couldn't load this prompt. <button onClick={() => detail.refetch()} className="underline">Retry</button></p>
          ) : detail.isLoading || !detail.data ? (
            <p className="text-sm text-slate">Loading…</p>
          ) : (
            <>
              <div className="mb-3 flex items-center justify-between">
                <h3 className="text-slate-lightest">{detail.data.name}</h3>
                {focused && <button onClick={onClose} className="text-slate hover:text-slate-lightest">✕</button>}
              </div>
              <PromptFillForm
                placeholders={detail.data.placeholders}
                variables={detail.data.variables}
                values={values}
                onChange={(k, val) => setValues((v) => ({ ...v, [k]: val }))}
              />
              <button
                onClick={insert}
                disabled={busy}
                className="mt-2 rounded-lg bg-gold px-4 py-2 text-sm font-medium text-navy hover:bg-gold-light disabled:opacity-40"
              >
                {busy ? "Inserting…" : "Insert into message"}
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
