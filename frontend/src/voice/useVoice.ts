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
import { useCallback, useEffect, useRef, useState } from "react";
import { speakText, transcribeAudio } from "@/api/client";
import { wsStore } from "@/ws/store";
import { createCapture, type Capture } from "@/voice/audioCapture";
import type { ServerFrame } from "@/ws/protocol";

const PREFERRED_MIMES = [
  "audio/webm;codecs=opus",
  "audio/webm",
  "audio/ogg;codecs=opus",
  "audio/mp4",
];

function pickMime(): string | undefined {
  if (typeof MediaRecorder === "undefined") return undefined;
  return PREFERRED_MIMES.find((m) => MediaRecorder.isTypeSupported(m));
}

// ── Hands-free dictation (continuous VAD, segment-on-pause) ───────────────────
// One tap starts listening; speech is cut into phrases on each ~pause, each phrase
// is transcribed (batch engine) and handed to `onText` to append live. Tap again
// (or stop()) to finish. In-browser energy VAD — no deps, no egress. Pairs with
// the ffmpeg normalisation in ml/app/stt.py (any capture format transcodes to WAV).

const SILENCE_MS = 1100; // trailing quiet that ends a phrase
const MIN_SPEECH_MS = 250; // ignore sub-blips (a cough, a click)
const MAX_SEGMENT_MS = 14000; // force-cut a long monologue
const CALIBRATE_MS = 350; // ambient-noise sampling at start
const THRESHOLD_FACTOR = 1.8; // speech threshold = noise floor × this
const THRESHOLD_FLOOR = 0.012; // min RMS (0..1) that counts as speech
const POLL_MS = 50; // VAD sampling cadence

type DictationStatus = "idle" | "listening" | "transcribing";

function rmsOf(buf: Uint8Array): number {
  let sum = 0;
  for (let i = 0; i < buf.length; i++) {
    const v = (buf[i] - 128) / 128;
    sum += v * v;
  }
  return Math.sqrt(sum / buf.length);
}

export function useDictation(opts: {
  onText: (text: string) => void;
  streaming?: boolean;
  /** Streaming only: replace the composer's live dictation region. */
  setComposer?: (text: string) => void;
  /** Streaming only: read the composer's current text (for the dictation base). */
  getComposer?: () => string;
}) {
  const [status, setStatus] = useState<DictationStatus>("idle");
  const [error, setError] = useState<string | null>(null);

  const onTextRef = useRef(opts.onText);
  onTextRef.current = opts.onText;
  const setComposerRef = useRef(opts.setComposer);
  setComposerRef.current = opts.setComposer;
  const getComposerRef = useRef(opts.getComposer);
  getComposerRef.current = opts.getComposer;
  const streamingRef = useRef(!!opts.streaming);
  streamingRef.current = !!opts.streaming;

  // ── Streaming dictation (realtime STT; text-while-speaking) ──────────────────
  const streamCaptureRef = useRef<Capture | null>(null);
  const streamUnsubRef = useRef<(() => void) | null>(null);
  const streamActiveRef = useRef(false);
  // Composer text BEFORE the current dictation (settled phrases land after it).
  const baseRef = useRef("");
  const finishTimerRef = useRef<number | null>(null);

  // base + live phrase, with a single separating space.
  const join = (b: string, t: string) => (b ? b.trimEnd() + " " + t : t);

  const teardownStream = useCallback(() => {
    streamActiveRef.current = false;
    streamCaptureRef.current?.stop();
    streamCaptureRef.current = null;
    streamUnsubRef.current?.();
    streamUnsubRef.current = null;
    if (finishTimerRef.current != null) {
      clearTimeout(finishTimerRef.current);
      finishTimerRef.current = null;
    }
    setStatus("idle");
  }, []);

  // Stop the mic + ask the server to commit, but KEEP the subscription so the
  // settled `voice.transcript` (emitted after commit) still lands; the composer
  // text is never cleared — the last live partial stays if the final times out.
  const stopStreaming = useCallback(() => {
    if (!streamActiveRef.current) return;
    streamCaptureRef.current?.stop();
    streamCaptureRef.current = null;
    try {
      wsStore.send({ type: "voice.dictate.stop" });
    } catch {
      /* socket gone */
    }
    setStatus("transcribing");
    if (finishTimerRef.current != null) clearTimeout(finishTimerRef.current);
    finishTimerRef.current = window.setTimeout(() => teardownStream(), 4000);
  }, [teardownStream]);

  const startStreaming = useCallback(async () => {
    if (streamActiveRef.current) return;
    streamActiveRef.current = true;
    setError(null);
    baseRef.current = getComposerRef.current?.() ?? "";
    setStatus("listening");
    wsStore.send({ type: "voice.dictate.start" });
    // Partials fill the composer live (cumulative — replace the dictation region);
    // the settled transcript promotes the base so a next phrase appends after it.
    streamUnsubRef.current = wsStore.onFrame((f: ServerFrame) => {
      if (!streamActiveRef.current) return;
      switch (f.type) {
        case "voice.partial":
          setComposerRef.current?.(join(baseRef.current, (f as { text?: string }).text ?? ""));
          break;
        case "voice.transcript": {
          const t = ((f as { text?: string }).text ?? "").trim();
          if (t) {
            const full = join(baseRef.current, t);
            setComposerRef.current?.(full);
            baseRef.current = full;
          }
          // The settled transcript only arrives after a stop-commit; finish then.
          if (finishTimerRef.current != null) teardownStream();
          break;
        }
        case "voice.error":
          setError((f as { message?: string }).message ?? "Dictation error.");
          teardownStream();
          break;
      }
    });
    try {
      streamCaptureRef.current = await createCapture({
        onFrame: (audio_base64, seq) => wsStore.send({ type: "voice.audio.chunk", audio_base64, seq }),
      });
      if (!streamActiveRef.current) streamCaptureRef.current.stop(); // stopped during getUserMedia
    } catch (e) {
      setError((e as Error).message || "Microphone unavailable.");
      teardownStream();
    }
  }, [teardownStream]);

  const streamRef = useRef<MediaStream | null>(null);
  const ctxRef = useRef<AudioContext | null>(null);
  const analyserRef = useRef<AnalyserNode | null>(null);
  const recRef = useRef<MediaRecorder | null>(null);
  const chunksRef = useRef<Blob[]>([]);
  const timerRef = useRef<number | null>(null);
  const mimeRef = useRef<string | undefined>(undefined);
  const activeRef = useRef(false);
  const thresholdRef = useRef(THRESHOLD_FLOOR);
  const queueRef = useRef<Promise<void>>(Promise.resolve());
  const pendingRef = useRef(0); // segments in transcription flight

  // per-segment bookkeeping
  const voicedMsRef = useRef(0);
  const lastVoiceTsRef = useRef(0);
  const segStartRef = useRef(0);

  function closeAudio() {
    if (timerRef.current != null) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    try {
      void ctxRef.current?.close();
    } catch {
      /* already closed */
    }
    ctxRef.current = null;
    analyserRef.current = null;
    streamRef.current?.getTracks().forEach((t) => t.stop());
    streamRef.current = null;
    recRef.current = null;
  }

  const cleanup = useCallback(() => {
    activeRef.current = false;
    closeAudio();
    chunksRef.current = [];
  }, []);

  useEffect(() => () => cleanup(), [cleanup]);

  function startRecorder() {
    const stream = streamRef.current;
    if (!stream) return;
    const mime = mimeRef.current;
    const rec = mime ? new MediaRecorder(stream, { mimeType: mime }) : new MediaRecorder(stream);
    chunksRef.current = [];
    rec.ondataavailable = (e) => {
      if (e.data.size > 0) chunksRef.current.push(e.data);
    };
    recRef.current = rec;
    rec.start();
    voicedMsRef.current = 0;
    const now = performance.now();
    segStartRef.current = now;
    lastVoiceTsRef.current = now;
  }

  /** Stop the current recorder and resolve its blob (or null). */
  function cutRecorder(): Promise<Blob | null> {
    const rec = recRef.current;
    recRef.current = null;
    if (!rec || rec.state === "inactive") return Promise.resolve(null);
    return new Promise((resolve) => {
      rec.onstop = () => {
        const blob = chunksRef.current.length
          ? new Blob(chunksRef.current, { type: rec.mimeType })
          : null;
        chunksRef.current = [];
        resolve(blob);
      };
      try {
        rec.stop();
      } catch {
        resolve(null);
      }
    });
  }

  /** Queue a finished segment for transcription, preserving phrase order. */
  function enqueueTranscribe(blob: Blob | null) {
    if (!blob) return;
    pendingRef.current += 1;
    setStatus("transcribing");
    queueRef.current = queueRef.current.then(async () => {
      try {
        const { text } = await transcribeAudio(blob);
        const t = text?.trim();
        if (t) onTextRef.current(t);
      } catch (e) {
        setError((e as Error).message);
      } finally {
        pendingRef.current -= 1;
        if (pendingRef.current === 0) setStatus(activeRef.current ? "listening" : "idle");
      }
    });
  }

  async function endSegment(restart: boolean) {
    const hadSpeech = voicedMsRef.current >= MIN_SPEECH_MS;
    const blob = await cutRecorder();
    if (hadSpeech) enqueueTranscribe(blob);
    if (restart && activeRef.current) {
      startRecorder();
      timerRef.current = window.setTimeout(poll, POLL_MS);
    }
  }

  function poll() {
    const analyser = analyserRef.current;
    if (!activeRef.current || !analyser) return;
    const buf = new Uint8Array(analyser.fftSize);
    analyser.getByteTimeDomainData(buf);
    const now = performance.now();
    if (rmsOf(buf) >= thresholdRef.current) {
      voicedMsRef.current += POLL_MS;
      lastVoiceTsRef.current = now;
    }
    const hadSpeech = voicedMsRef.current >= MIN_SPEECH_MS;
    const endByPause = hadSpeech && now - lastVoiceTsRef.current >= SILENCE_MS;
    const endByMax = hadSpeech && now - segStartRef.current >= MAX_SEGMENT_MS;
    if (endByPause || endByMax) {
      void endSegment(true); // reschedules poll after the new recorder starts
      return;
    }
    timerRef.current = window.setTimeout(poll, POLL_MS);
  }

  function calibrate(): Promise<void> {
    return new Promise((resolve) => {
      const analyser = analyserRef.current;
      if (!analyser) return resolve();
      const buf = new Uint8Array(analyser.fftSize);
      const t0 = performance.now();
      let peak = 0;
      const tick = () => {
        analyser.getByteTimeDomainData(buf);
        peak = Math.max(peak, rmsOf(buf));
        if (performance.now() - t0 < CALIBRATE_MS) {
          window.setTimeout(tick, 30);
        } else {
          thresholdRef.current = Math.max(peak * THRESHOLD_FACTOR, THRESHOLD_FLOOR);
          resolve();
        }
      };
      tick();
    });
  }

  const start = useCallback(async () => {
    if (streamingRef.current) return startStreaming();
    setError(null);
    if (typeof navigator === "undefined" || !navigator.mediaDevices?.getUserMedia) {
      setError("Microphone not available in this browser.");
      return;
    }
    if (typeof MediaRecorder === "undefined") {
      setError("Recording is not supported in this browser.");
      return;
    }
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      streamRef.current = stream;
      const Ctx: typeof AudioContext =
        window.AudioContext || (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext;
      const ctx = new Ctx();
      ctxRef.current = ctx;
      // Browsers create the context "suspended"; resume so the analyser delivers
      // real samples during calibration + the first segment (else the first
      // phrase can be missed on a cold start).
      if (ctx.state === "suspended") await ctx.resume();
      const analyser = ctx.createAnalyser();
      analyser.fftSize = 2048;
      ctx.createMediaStreamSource(stream).connect(analyser);
      analyserRef.current = analyser;
      mimeRef.current = pickMime();
      activeRef.current = true;
      await calibrate();
      if (!activeRef.current) return; // stopped during calibration
      startRecorder();
      setStatus("listening");
      timerRef.current = window.setTimeout(poll, POLL_MS);
    } catch (e) {
      setError((e as Error).message || "Microphone permission denied.");
      cleanup();
      setStatus("idle");
    }
  }, [cleanup, startStreaming]);

  /** Finish: cut + transcribe the final phrase, release the mic. */
  const stop = useCallback(async () => {
    if (streamingRef.current || streamActiveRef.current) return stopStreaming();
    if (!activeRef.current) return;
    activeRef.current = false;
    if (timerRef.current != null) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    const hadSpeech = voicedMsRef.current >= MIN_SPEECH_MS;
    const blob = await cutRecorder();
    if (hadSpeech) enqueueTranscribe(blob);
    closeAudio();
    if (pendingRef.current === 0) setStatus("idle");
  }, [stopStreaming]);

  // Hard teardown of the streaming session on unmount (batch path has its own cleanup).
  useEffect(() => () => teardownStream(), [teardownStream]);

  return { status, error, start, stop, clearError: () => setError(null) };
}

/** On-demand read-aloud. Decodes the engine's clip with Web Audio rather than an
 *  <audio> element: a complete WAV/mp3 decodes reliably via `decodeAudioData`,
 *  whereas `<audio>` rejects some valid WAV with "no supported source". */
export function useReadAloud() {
  const [speakingId, setSpeakingId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const ctxRef = useRef<AudioContext | null>(null);
  const srcRef = useRef<AudioBufferSourceNode | null>(null);

  const stop = useCallback(() => {
    try {
      srcRef.current?.stop();
    } catch {
      /* already stopped */
    }
    srcRef.current = null;
    if (ctxRef.current) {
      try {
        void ctxRef.current.close();
      } catch {
        /* ignore */
      }
      ctxRef.current = null;
    }
    setSpeakingId(null);
  }, []);

  useEffect(() => () => stop(), [stop]);

  const play = useCallback(
    async (id: string, text: string) => {
      stop();
      setBusy(true);
      try {
        const blob = await speakText(text);
        const bytes = await blob.arrayBuffer();
        const Ctx: typeof AudioContext =
          window.AudioContext ||
          (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext;
        const ctx = new Ctx();
        ctxRef.current = ctx;
        if (ctx.state === "suspended") await ctx.resume();
        const buf = await ctx.decodeAudioData(bytes);
        if (ctxRef.current !== ctx) return; // stop() raced the await
        const src = ctx.createBufferSource();
        src.buffer = buf;
        src.connect(ctx.destination);
        src.onended = () => stop();
        src.start();
        srcRef.current = src;
        setSpeakingId(id);
      } catch (e) {
        stop();
        toast(`Read-aloud failed: ${(e as Error).message}`);
      } finally {
        setBusy(false);
      }
    },
    [stop],
  );

  return { speakingId, busy, play, stop };
}
