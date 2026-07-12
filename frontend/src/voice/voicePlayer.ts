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

// Streaming playback for the spoken reply. The
// backend emits ONE `voice.tts.chunk` per clause — a complete, self-contained
// audio clip (the assistant speaks clause-by-clause as they synthesise). Each clip
// is decoded with Web Audio `decodeAudioData` and scheduled back-to-back on a
// running timeline for gapless playback in arrival order. A complete clip always
// decodes (unlike concatenated partial mp3 chunks), so this is robust across
// engines (OpenAI mp3, kokoro, the batch wav fallback). `stop()` cuts everything
// immediately for barge-in. Single-use: create a fresh player per turn.

// ArrayBuffer-backed (not SharedArrayBuffer) so the bytes satisfy `BufferSource`
// for decodeAudioData under TS's strict typed arrays.
function base64ToBytes(b64: string): Uint8Array<ArrayBuffer> {
  const bin = atob(b64);
  const buf = new ArrayBuffer(bin.length);
  const out = new Uint8Array(buf);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export class VoicePlayer {
  private stopped = false;
  private ctx: AudioContext | null = null;
  private nextStart = 0;
  private sources: AudioBufferSourceNode[] = [];
  // Serialise decode+schedule so the timeline stays in arrival (clause) order.
  private chain: Promise<void> = Promise.resolve();

  /** Queue one complete-clause clip (base64) for playback, in arrival order. */
  enqueue(audioBase64: string, _mime: string): void {
    if (this.stopped) return;
    if (!this.ctx) {
      const Ctx: typeof AudioContext =
        window.AudioContext ||
        (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext;
      this.ctx = new Ctx();
      this.nextStart = 0;
    }
    // A suspended context (autoplay policy) must not silently swallow the reply;
    // the session starts from a click, so resuming here is allowed.
    void this.ctx.resume().catch(() => {});
    const bytes = base64ToBytes(audioBase64);
    this.chain = this.chain.then(() => this.decodeAndSchedule(bytes));
  }

  /** No more clauses this turn — nothing to flush (each clip self-schedules). */
  end(): void {
    /* clips are scheduled as they arrive; nothing to finalise */
  }

  /** Barge-in / teardown: cut audio immediately and release everything. */
  stop(): void {
    if (this.stopped) return;
    this.stopped = true;
    this.sources.forEach((s) => {
      try {
        s.stop();
      } catch {
        /* already stopped */
      }
    });
    this.sources = [];
    if (this.ctx) {
      try {
        void this.ctx.close();
      } catch {
        /* ignore */
      }
      this.ctx = null;
    }
  }

  private async decodeAndSchedule(bytes: Uint8Array<ArrayBuffer>): Promise<void> {
    const ctx = this.ctx;
    if (!ctx || this.stopped) return;
    let buf: AudioBuffer;
    try {
      buf = await ctx.decodeAudioData(bytes.buffer.slice(0));
    } catch {
      return; // undecodable clip → skip (don't strand the queue)
    }
    if (this.stopped || !this.ctx) return;
    const src = ctx.createBufferSource();
    src.buffer = buf;
    src.connect(ctx.destination);
    const start = Math.max(ctx.currentTime, this.nextStart);
    src.start(start);
    this.nextStart = start + buf.duration;
    this.sources.push(src);
    src.onended = () => {
      this.sources = this.sources.filter((s) => s !== src);
    };
  }
}
