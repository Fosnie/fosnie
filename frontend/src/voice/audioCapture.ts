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

// Microphone capture for live voice. The browser's
// MediaRecorder only yields Opus/WebM, so we use Web Audio to get raw PCM: an
// AudioWorklet (with a ScriptProcessor fallback) hands Float32 frames to the main
// thread, which resamples to 16 kHz mono, packs 20 ms PCM16 LE frames, base64-
// encodes them, and emits them as `voice.audio.chunk` payloads. Echo cancellation
// + noise suppression are requested so the assistant's own audio can't self-trigger
// barge-in over an open mic.

const TARGET_RATE = 16000;
const FRAME_SAMPLES = 320; // 20 ms @ 16 kHz

export interface Capture {
  /** Release the mic + audio graph. */
  stop(): void;
  /** Whether the browser granted echo cancellation (reported to the server). */
  aec: boolean;
}

export interface CaptureOptions {
  /** One 20 ms PCM16 frame, base64-encoded, with a monotonic sequence number. */
  onFrame: (audioBase64: string, seq: number) => void;
  /** Input level (RMS, 0..1) per frame, for the meter. */
  onLevel?: (level: number) => void;
}

/** Streaming linear resampler → fixed-size Int16 frames. Carries a fractional read
 *  position across pushes so there is no per-block drift. */
class FrameResampler {
  private buf: Float32Array = new Float32Array(0);
  private pos = 0; // fractional read index into `buf`
  private readonly ratio: number; // input samples per output sample
  private out: number[] = [];

  constructor(inRate: number) {
    this.ratio = inRate / TARGET_RATE;
  }

  push(input: Float32Array, emit: (frame: Int16Array) => void): void {
    // Append the new input to whatever tail remained.
    const merged = new Float32Array(this.buf.length + input.length);
    merged.set(this.buf);
    merged.set(input, this.buf.length);
    this.buf = merged;

    while (this.pos + 1 < this.buf.length) {
      const i = Math.floor(this.pos);
      const frac = this.pos - i;
      const s = this.buf[i] * (1 - frac) + this.buf[i + 1] * frac;
      this.out.push(s);
      this.pos += this.ratio;
      if (this.out.length >= FRAME_SAMPLES) {
        const frame = new Int16Array(FRAME_SAMPLES);
        for (let k = 0; k < FRAME_SAMPLES; k++) {
          const v = Math.max(-1, Math.min(1, this.out[k]));
          frame[k] = v < 0 ? v * 0x8000 : v * 0x7fff;
        }
        emit(frame);
        this.out = this.out.slice(FRAME_SAMPLES);
      }
    }
    // Drop the input samples we've already consumed.
    const drop = Math.floor(this.pos);
    if (drop > 0) {
      this.buf = this.buf.slice(drop);
      this.pos -= drop;
    }
  }
}

function bytesToBase64(bytes: Uint8Array): string {
  let bin = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    bin += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(bin);
}

function rms(frame: Int16Array): number {
  let sum = 0;
  for (let i = 0; i < frame.length; i++) {
    const v = frame[i] / 32768;
    sum += v * v;
  }
  return Math.sqrt(sum / Math.max(1, frame.length));
}

// PCM capture worklet, served as a static asset under CSP `script-src 'self'`
// (see frontend/public/pcm-capture-worklet.js). A Blob URL would be blocked by CSP.
const WORKLET_URL = "/pcm-capture-worklet.js";

export async function createCapture(opts: CaptureOptions): Promise<Capture> {
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: {
      echoCancellation: true,
      noiseSuppression: true,
      autoGainControl: true,
      channelCount: 1,
    },
  });
  const track = stream.getAudioTracks()[0];
  const aec = track?.getSettings?.().echoCancellation !== false;

  const Ctx: typeof AudioContext =
    window.AudioContext ||
    (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext;
  // Ask for 16 kHz so the browser resamples for us; Safari ignores the hint, hence
  // the JS resampler keyed on the actual context rate.
  let ctx: AudioContext;
  try {
    ctx = new Ctx({ sampleRate: TARGET_RATE });
  } catch {
    ctx = new Ctx();
  }
  if (ctx.state === "suspended") await ctx.resume();

  const resampler = new FrameResampler(ctx.sampleRate);
  let seq = 0;
  let stopped = false;
  const onChunk = (input: Float32Array) => {
    if (stopped) return;
    resampler.push(input, (frame) => {
      opts.onLevel?.(rms(frame));
      opts.onFrame(bytesToBase64(new Uint8Array(frame.buffer)), seq++);
    });
  };

  const source = ctx.createMediaStreamSource(stream);
  // A muted sink keeps the graph pulling without echoing the mic to the speakers.
  const mute = ctx.createGain();
  mute.gain.value = 0;

  let workletNode: AudioWorkletNode | null = null;
  let scriptNode: ScriptProcessorNode | null = null;
  // Prefer AudioWorklet, loading the processor from a static, CSP-allowed URL. Fall
  // back to the deprecated ScriptProcessorNode only for genuinely worklet-less
  // browsers (or an unexpected load failure) — no longer for a CSP-blocked Blob.
  let usedWorklet = false;
  if (ctx.audioWorklet) {
    try {
      await ctx.audioWorklet.addModule(WORKLET_URL);
      workletNode = new AudioWorkletNode(ctx, "pcm-capture");
      workletNode.port.onmessage = (e) => onChunk(e.data as Float32Array);
      source.connect(workletNode);
      workletNode.connect(mute);
      usedWorklet = true;
    } catch {
      workletNode = null;
    }
  }
  if (!usedWorklet) {
    scriptNode = ctx.createScriptProcessor(2048, 1, 1);
    scriptNode.onaudioprocess = (e) => onChunk(new Float32Array(e.inputBuffer.getChannelData(0)));
    source.connect(scriptNode);
    scriptNode.connect(mute);
  }
  mute.connect(ctx.destination);

  return {
    aec,
    stop() {
      if (stopped) return;
      stopped = true;
      try {
        if (workletNode) workletNode.port.onmessage = null;
        if (scriptNode) scriptNode.onaudioprocess = null;
        source.disconnect();
        workletNode?.disconnect();
        scriptNode?.disconnect();
        mute.disconnect();
        void ctx.close();
      } catch {
        /* already torn down */
      }
      stream.getTracks().forEach((t) => t.stop());
    },
  };
}
