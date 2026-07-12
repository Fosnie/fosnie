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

// Live-voice session hook. Owns the mic capture, the
// streaming player, and a WebSocket subscription for the voice.* frames; exposes a
// small control surface for the call-mode overlay. The transcript + answer ride the
// relayed chat.* frames (rendered by Chat.tsx), so this hook only drives the live
// state machine, the spoken reply, push-to-talk, and barge-in.

import { useCallback, useEffect, useRef, useState } from "react";
import { wsStore } from "@/ws/store";
import { createCapture, type Capture } from "@/voice/audioCapture";
import { VoicePlayer } from "@/voice/voicePlayer";
import type { ServerFrame } from "@/ws/protocol";

export type VoiceState =
  | "idle"
  | "connecting"
  | "listening"
  | "capturing"
  | "thinking"
  | "speaking"
  | "interrupted"
  | "error";

export type VoiceMode = "ptt" | "vad";

export interface UseLiveVoiceOpts {
  chatId: string | null;
  agentId: string | null;
  projectId?: string | null;
  /** Called with the settled user transcript so Chat can show the optimistic bubbles. */
  onUserFinal?: (text: string) => void;
  /** From whoami `voice_live_opts`. */
  pttDefault?: boolean;
  silenceMs?: number;
}

export interface LiveVoice {
  active: boolean;
  state: VoiceState;
  partial: string;
  level: number;
  mode: VoiceMode;
  talking: boolean;
  error: string | null;
  start: () => void;
  end: () => void;
  pressTalk: () => void;
  releaseTalk: () => void;
  bargeIn: () => void;
  setMode: (m: VoiceMode) => void;
  clearError: () => void;
}

export function useLiveVoice(opts: UseLiveVoiceOpts): LiveVoice {
  const [active, setActive] = useState(false);
  const [state, setStateRaw] = useState<VoiceState>("idle");
  const [partial, setPartial] = useState("");
  const [level, setLevel] = useState(0);
  const [mode, setModeState] = useState<VoiceMode>(opts.pttDefault === false ? "vad" : "ptt");
  const [talking, setTalking] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const optsRef = useRef(opts);
  optsRef.current = opts;
  const activeRef = useRef(false);
  const stateRef = useRef<VoiceState>("idle");
  const talkingRef = useRef(false);
  const modeRef = useRef<VoiceMode>(mode);
  const captureRef = useRef<Capture | null>(null);
  const playerRef = useRef<VoicePlayer | null>(null);
  const tailTimer = useRef<number | null>(null);
  const lastLevelTs = useRef(0);
  const tailMsRef = useRef(850);

  tailMsRef.current = (opts.silenceMs ?? 600) + 250;

  const setVState = useCallback((s: VoiceState) => {
    stateRef.current = s;
    setStateRaw(s);
  }, []);

  // Sync the PTT default from whoami while idle (it can arrive after first render).
  useEffect(() => {
    if (!activeRef.current) {
      const m: VoiceMode = opts.pttDefault === false ? "vad" : "ptt";
      modeRef.current = m;
      setModeState(m);
    }
  }, [opts.pttDefault]);

  const closeCapture = useCallback(() => {
    captureRef.current?.stop();
    captureRef.current = null;
    setLevel(0);
  }, []);

  const openCapture = useCallback(async () => {
    if (captureRef.current) return;
    try {
      captureRef.current = await createCapture({
        onFrame: (audio_base64, seq) => wsStore.send({ type: "voice.audio.chunk", audio_base64, seq }),
        onLevel: (lv) => {
          const now = performance.now();
          if (now - lastLevelTs.current >= 80) {
            lastLevelTs.current = now;
            setLevel(lv);
          }
        },
      });
      if (!activeRef.current) closeCapture(); // session ended during getUserMedia
    } catch (e) {
      setError((e as Error).message || "Microphone unavailable.");
      setVState("error");
    }
  }, [closeCapture, setVState]);

  const bargeIn = useCallback(() => {
    wsStore.send({ type: "voice.barge_in" });
    playerRef.current?.stop();
    playerRef.current = null;
    setVState("listening");
  }, [setVState]);

  const start = useCallback(() => {
    if (activeRef.current) return;
    activeRef.current = true;
    setActive(true);
    setError(null);
    setPartial("");
    setVState("connecting");
    const o = optsRef.current;
    wsStore.send({
      type: "voice.stream.start",
      chat_id: o.chatId ?? null,
      project_id: o.projectId ?? null,
      agent_id: o.agentId ?? null,
      mode: modeRef.current,
      aec: true,
    });
    if (modeRef.current === "vad") void openCapture();
    setVState("listening"); // optimistic; the server confirms via voice.state
  }, [openCapture, setVState]);

  const end = useCallback(() => {
    if (!activeRef.current) return;
    activeRef.current = false;
    if (tailTimer.current) {
      clearTimeout(tailTimer.current);
      tailTimer.current = null;
    }
    wsStore.send({ type: "voice.stream.end" });
    closeCapture();
    playerRef.current?.stop();
    playerRef.current = null;
    talkingRef.current = false;
    setTalking(false);
    setActive(false);
    setPartial("");
    setLevel(0);
    setVState("idle");
  }, [closeCapture, setVState]);

  const pressTalk = useCallback(() => {
    if (!activeRef.current) return;
    if (tailTimer.current) {
      clearTimeout(tailTimer.current);
      tailTimer.current = null;
    }
    if (stateRef.current === "speaking") bargeIn(); // talk over the assistant
    talkingRef.current = true;
    setTalking(true);
    void openCapture();
  }, [bargeIn, openCapture]);

  const releaseTalk = useCallback(() => {
    if (!activeRef.current) return;
    talkingRef.current = false;
    setTalking(false);
    // PTT: keep capturing a short trailing tail so the server's silence gate ends
    // the turn (no per-utterance end frame is needed).
    if (modeRef.current === "ptt") {
      if (tailTimer.current) clearTimeout(tailTimer.current);
      tailTimer.current = window.setTimeout(() => {
        tailTimer.current = null;
        if (!talkingRef.current) closeCapture();
      }, tailMsRef.current);
    }
  }, [closeCapture]);

  const setMode = useCallback(
    (m: VoiceMode) => {
      modeRef.current = m;
      setModeState(m);
      if (!activeRef.current) return;
      if (m === "vad") void openCapture();
      else if (!talkingRef.current) closeCapture();
    },
    [openCapture, closeCapture],
  );

  // Voice.* frame handling (+ chat.interrupted to cut audio on server-side barge-in).
  useEffect(() => {
    return wsStore.onFrame((f: ServerFrame) => {
      if (!activeRef.current) return;
      switch (f.type) {
        case "voice.state":
          setVState((f as { state?: string }).state as VoiceState);
          break;
        case "voice.partial":
          setPartial((f as { text?: string }).text ?? "");
          break;
        case "voice.final": {
          const text = ((f as { text?: string }).text ?? "").trim();
          playerRef.current?.stop(); // a new utterance → cut any prior playback
          playerRef.current = null;
          setPartial("");
          if (text) optsRef.current.onUserFinal?.(text);
          break;
        }
        case "voice.tts.chunk": {
          const c = f as { audio_base64?: string; mime?: string };
          if (!c.audio_base64) break;
          if (!playerRef.current) playerRef.current = new VoicePlayer();
          playerRef.current.enqueue(c.audio_base64, c.mime ?? "audio/wav");
          break;
        }
        case "voice.tts.end":
          playerRef.current?.end();
          break;
        case "voice.error":
          setError((f as { message?: string }).message ?? "Voice error.");
          setVState("error");
          break;
        case "chat.interrupted":
          playerRef.current?.stop();
          playerRef.current = null;
          break;
      }
    });
  }, [setVState]);

  // Teardown on unmount.
  useEffect(
    () => () => {
      activeRef.current = false;
      if (tailTimer.current) clearTimeout(tailTimer.current);
      captureRef.current?.stop();
      captureRef.current = null;
      playerRef.current?.stop();
      playerRef.current = null;
    },
    [],
  );

  return {
    active,
    state,
    partial,
    level,
    mode,
    talking,
    error,
    start,
    end,
    pressTalk,
    releaseTalk,
    bargeIn,
    setMode,
    clearError: () => setError(null),
  };
}
