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

// Which artefact the panel is showing, and how it is placed.
//
// The selection lives in the URL (`?a=<artefact id>`) rather than in component
// state so a panel can be linked to and survives a reload. It is resolved against
// the chat's already-loaded artefact list — the id always belongs to the open
// chat, so no extra request is needed.

import { useCallback, useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router-dom";

import type { Artefact } from "@/api/client";
import type { PanelMode } from "@/components/artefacts/ArtefactPanel";

/** Above this width the panel is a column beside the thread; below it, a drawer.
 *  The CSS keys off the class this decides, never off its own media query, so the
 *  two cannot drift apart. */
export const ARTEFACT_DOCK_QUERY = "(min-width: 1280px)";

const PARAM = "a";

/** How long after a turn finishes we are still willing to open its artefact. The
 *  artefact list is refetched again shortly after the turn completes (some
 *  artefacts are written server-side after the message), so the window has to
 *  outlast that, while staying short enough that an unrelated later refetch —
 *  a verification run, a conversion — cannot trigger an auto-open. */
const AUTO_OPEN_WINDOW_MS = 6000;

export function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState(() =>
    typeof window !== "undefined" ? window.matchMedia(query).matches : false,
  );
  useEffect(() => {
    const mq = window.matchMedia(query);
    const onChange = () => setMatches(mq.matches);
    onChange();
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, [query]);
  return matches;
}

export function useArtefactPanel(chatId: string | undefined, artefactList: Artefact[], listLoading: boolean) {
  const [sp, setSp] = useSearchParams();
  const selectedId = sp.get(PARAM);
  const selected = artefactList.find((a) => a.id === selectedId) ?? null;
  const docked = useMediaQuery(ARTEFACT_DOCK_QUERY);
  const mode: PanelMode = docked ? "docked" : "overlay";

  // True while the entry we pushed is still the top of the history stack, so
  // closing can go back rather than stacking another entry.
  const pushed = useRef(false);
  // The artefacts that already existed when the current turn was sent, and
  // whether the user has touched the panel since — an auto-open must not steal a
  // panel someone is reading.
  const preTurnIds = useRef<Set<string>>(new Set());
  const touched = useRef(false);
  const armed = useRef<{ at: number; chatId: string } | null>(null);

  const setParam = useCallback(
    (id: string | null, opts: { replace: boolean }) => {
      setSp(
        (prev) => {
          const next = new URLSearchParams(prev);
          if (id) next.set(PARAM, id);
          else next.delete(PARAM);
          return next;
        },
        { replace: opts.replace },
      );
    },
    [setSp],
  );

  const open = useCallback(
    (a: Artefact) => {
      touched.current = true;
      const wasOpen = !!selectedId;
      setParam(a.id, { replace: wasOpen });
      if (!wasOpen) pushed.current = true;
    },
    [selectedId, setParam],
  );

  const close = useCallback(() => {
    touched.current = true;
    if (pushed.current) {
      pushed.current = false;
      window.history.back();
      return;
    }
    setParam(null, { replace: true });
  }, [setParam]);

  const markInteracted = useCallback(() => {
    touched.current = true;
  }, []);

  /** Called when a turn is sent: remember what existed before it, and let the
   *  turn's own artefact claim the panel again. */
  const beginTurn = useCallback(() => {
    preTurnIds.current = new Set(artefactList.map((a) => a.id));
    touched.current = false;
  }, [artefactList]);

  /** Called when a turn completes: arm the auto-open. The list is not up to date
   *  at this point — only its invalidation has been queued — so the opening
   *  happens in the effect below, once something new actually arrives. */
  const armAutoOpen = useCallback((cid: string) => {
    armed.current = { at: Date.now(), chatId: cid };
  }, []);

  // Close and reset when the chat changes. Not on the first run: arriving with a
  // link that already names an artefact must open it, not strip it.
  const lastChatId = useRef<string | undefined>(chatId);
  useEffect(() => {
    if (lastChatId.current === chatId) return;
    lastChatId.current = chatId;
    armed.current = null;
    pushed.current = false;
    touched.current = false;
    preTurnIds.current = new Set();
    if (selectedId) setParam(null, { replace: true });
    // Only on a chat switch — re-running when the selection changes would close
    // the panel the moment it opened.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [chatId]);

  // Open the artefact a live turn just produced.
  useEffect(() => {
    const arm = armed.current;
    if (!arm || !chatId) return;
    if (arm.chatId !== chatId) { armed.current = null; return; }
    if (Date.now() - arm.at > AUTO_OPEN_WINDOW_MS) { armed.current = null; return; }
    if (touched.current) { armed.current = null; return; }
    // The list is newest-first, so the first entry we have not seen before is the
    // newest one this turn produced.
    const fresh = artefactList.find((a) => !preTurnIds.current.has(a.id));
    if (!fresh) return;
    armed.current = null;
    preTurnIds.current.add(fresh.id);
    setParam(fresh.id, { replace: true });
  }, [artefactList, chatId, setParam]);

  return {
    selectedId,
    selected,
    /** The id is in the URL but not (yet) in the list. */
    pending: !!selectedId && !selected && listLoading,
    missing: !!selectedId && !selected && !listLoading,
    isOpen: !!selectedId,
    mode,
    docked,
    open,
    close,
    markInteracted,
    beginTurn,
    armAutoOpen,
  };
}
