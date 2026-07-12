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

// Custom in-app dialogs + toasts, replacing native window.alert/confirm/prompt
// (which render off-brand "localhost says…" browser chrome). The API is imperative
// and callable from anywhere — components, hooks, and .then/.catch chains:
//   await confirmDialog({ title, body?, danger? }) -> boolean
//   await promptDialog({ title, defaultValue? })   -> string | null
//   toast("Save failed: …")                        -> error toast (default)
// A single <DialogHost/> + <ToastHost/> (mounted once in Shell) render the active
// modal and the toast stack. House-styled (modal-scrim/modal, btn-gold/btn-danger).

import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { AnimatePresence, motion } from "motion/react";
import { Icon } from "@/components/icons";
import { popVariants, scrimVariants, spring, toastVariants } from "@/app/motion";

type ConfirmReq = {
  kind: "confirm";
  id: number;
  title: string;
  body?: string;
  confirmLabel?: string;
  cancelLabel?: string;
  danger?: boolean;
  resolve: (v: boolean) => void;
};
type PromptReq = {
  kind: "prompt";
  id: number;
  title: string;
  label?: string;
  defaultValue?: string;
  placeholder?: string;
  confirmLabel?: string;
  resolve: (v: string | null) => void;
};
type DialogReq = ConfirmReq | PromptReq;

type ToastVariant = "error" | "info" | "success";
type ToastItem = { id: number; text: string; variant: ToastVariant };

// --- singleton store (mirrors ws/store.ts: module state + a Set of listeners) ----
let queue: DialogReq[] = [];
let toasts: ToastItem[] = [];
let seq = 1;
const listeners = new Set<() => void>();

function emit() {
  for (const l of listeners) l();
}
function subscribe(l: () => void): () => void {
  listeners.add(l);
  return () => listeners.delete(l);
}
function active(): DialogReq | null {
  return queue[0] ?? null;
}
function getToasts(): ToastItem[] {
  return toasts;
}
/** Resolve the active dialog and advance the queue. */
function settle(result: boolean | string | null) {
  const cur = queue.shift();
  if (cur) (cur.resolve as (v: unknown) => void)(result);
  emit();
}

export function confirmDialog(o: Omit<ConfirmReq, "kind" | "id" | "resolve">): Promise<boolean> {
  return new Promise<boolean>((resolve) => {
    queue.push({ kind: "confirm", id: seq++, resolve, ...o });
    emit();
  });
}
export function promptDialog(o: Omit<PromptReq, "kind" | "id" | "resolve">): Promise<string | null> {
  return new Promise<string | null>((resolve) => {
    queue.push({ kind: "prompt", id: seq++, resolve, ...o });
    emit();
  });
}
export function toast(text: string, o?: { variant?: ToastVariant; duration?: number }): void {
  const id = seq++;
  toasts = [...toasts, { id, text, variant: o?.variant ?? "error" }];
  emit();
  window.setTimeout(() => {
    toasts = toasts.filter((t) => t.id !== id);
    emit();
  }, o?.duration ?? 6000);
}
function dismissToast(id: number) {
  toasts = toasts.filter((t) => t.id !== id);
  emit();
}

// Read the store via useSyncExternalStore — React's external-store primitive.
// (A manual useReducer "tick" + reading the module global directly in render is
// invisible to the React Compiler, which then memoises the stale first value;
// useSyncExternalStore is a tracked reactive read the Compiler respects.)

// --- hosts (mounted once in Shell) ----------------------------------------------

export function DialogHost() {
  const cur = useSyncExternalStore(subscribe, active);
  // AnimatePresence keeps the active dialog mounted through its exit animation.
  return (
    <AnimatePresence>
      {cur && (cur.kind === "confirm"
        ? <ConfirmView key={cur.id} req={cur} />
        : <PromptView key={cur.id} req={cur} />)}
    </AnimatePresence>
  );
}

function ConfirmView({ req }: { req: ConfirmReq }) {
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") { e.preventDefault(); settle(false); }
      else if (e.key === "Enter") { e.preventDefault(); settle(true); }
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [req.id]);
  return (
    <motion.div className="modal-scrim" onClick={() => settle(false)}
      variants={scrimVariants} initial="initial" animate="animate" exit="exit">
      <motion.div className="modal dialog-modal glass glass--modal glass-noise" onClick={(e) => e.stopPropagation()}
        role="dialog" aria-modal="true" aria-label={req.title}
        variants={popVariants} initial="initial" animate="animate" exit="exit" transition={spring}>
        <div className="modal-head">
          <div>
            <div className="eyebrow">{req.danger ? "Confirm" : "Please confirm"}</div>
            <h2 className="serif modal-title">{req.title}</h2>
          </div>
          <button className="icon-btn" onClick={() => settle(false)}><Icon.Close size={18} /></button>
        </div>
        {req.body && <div className="modal-body"><p className="dialog-body">{req.body}</p></div>}
        <div className="modal-foot">
          <button className="btn btn-ghost" onClick={() => settle(false)}>{req.cancelLabel ?? "Cancel"}</button>
          <button className={"btn " + (req.danger ? "btn-danger" : "btn-gold")} onClick={() => settle(true)} autoFocus>
            {req.confirmLabel ?? (req.danger ? "Delete" : "Confirm")}
          </button>
        </div>
      </motion.div>
    </motion.div>
  );
}

function PromptView({ req }: { req: PromptReq }) {
  const [val, setVal] = useState(req.defaultValue ?? "");
  const inputRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") { e.preventDefault(); settle(null); }
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [req.id]);
  return (
    <motion.div className="modal-scrim" onClick={() => settle(null)}
      variants={scrimVariants} initial="initial" animate="animate" exit="exit">
      <motion.div className="modal dialog-modal glass glass--modal glass-noise" onClick={(e) => e.stopPropagation()}
        role="dialog" aria-modal="true" aria-label={req.title}
        variants={popVariants} initial="initial" animate="animate" exit="exit" transition={spring}>
        <div className="modal-head">
          <div>
            <div className="eyebrow">Input</div>
            <h2 className="serif modal-title">{req.title}</h2>
          </div>
          <button className="icon-btn" onClick={() => settle(null)}><Icon.Close size={18} /></button>
        </div>
        <div className="modal-body">
          {req.label && <label className="ed-hint mono" style={{ display: "block", marginBottom: 6 }}>{req.label}</label>}
          <input
            ref={inputRef}
            className="field"
            style={{ width: "100%" }}
            value={val}
            placeholder={req.placeholder}
            onChange={(e) => setVal(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); settle(val); } }}
          />
        </div>
        <div className="modal-foot">
          <button className="btn btn-ghost" onClick={() => settle(null)}>Cancel</button>
          <button className="btn btn-gold" onClick={() => settle(val)}>{req.confirmLabel ?? "OK"}</button>
        </div>
      </motion.div>
    </motion.div>
  );
}

export function ToastHost() {
  const items = useSyncExternalStore(subscribe, getToasts);
  if (items.length === 0) return null;
  const glyph = (v: ToastVariant) => (v === "error" ? <Icon.Alert size={16} /> : v === "success" ? <Icon.Check size={16} /> : <Icon.Bell size={16} />);
  return (
    <div className="toast-stack">
      <AnimatePresence>
        {items.map((t) => (
          <motion.div key={t.id} className={"toast glass glass--hud toast-" + t.variant} role="status" layout
            variants={toastVariants} initial="initial" animate="animate" exit="exit" transition={spring}>
            <span className="toast-ic">{glyph(t.variant)}</span>
            <span className="toast-text">{t.text}</span>
            <button className="toast-x" onClick={() => dismissToast(t.id)} aria-label="Dismiss"><Icon.Close size={13} /></button>
          </motion.div>
        ))}
      </AnimatePresence>
    </div>
  );
}
