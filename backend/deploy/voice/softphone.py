# Copyright 2026 Private AI Ltd (SC881079)
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""A telephone system, for the length of one call.

This stands in for the switchboard on a practice's own network. It does the three
things such a system does, in the order it does them: it asks what to do with a call
that is ringing, it opens a connection and presents the identifier it was given, and
it asks afterwards whether anybody is to be rung instead.

It is here so that a deployment can be dialled without a carrier account and without a
telephone number. Nothing in it is a test double: the identifier it presents is the
real single-use one, the audio it sends is what a narrowband line carries, and the
answer it gets back is the same audio a caller would hear.

    uv run --with sounddevice --with numpy --with requests \
        python softphone.py --to +441315550100 --key <the deployment's shared secret>

Speak when it says the line is up. Press Ctrl-C to hang up.

With no microphone, or with --wav, it plays a sound file down the line instead and
saves what comes back.
"""

from __future__ import annotations

import argparse
import queue
import socket
import struct
import sys
import threading
import time
import wave

import requests

# The wire. One type byte, then the length as two bytes most significant first.
TYPE_HANGUP = 0x00
TYPE_ID = 0x01
TYPE_DTMF = 0x03
TYPE_AUDIO = 0x10
TYPE_ERROR = 0xFF

RATE = 8_000  # what a telephone line carries
FRAME_SAMPLES = 160  # twenty milliseconds of it
FRAME_BYTES = FRAME_SAMPLES * 2  # signed sixteen bit, one channel
FRAME_SECONDS = FRAME_SAMPLES / RATE

SILENCE = b"\x00" * FRAME_BYTES


def encode(kind: int, payload: bytes = b"") -> bytes:
    return struct.pack(">BH", kind, len(payload)) + payload


def frames(buf: bytearray):
    """Every whole message at the front of `buf`, consuming what it reads."""
    while len(buf) >= 3:
        kind, length = struct.unpack(">BH", bytes(buf[:3]))
        if len(buf) < 3 + length:
            return
        payload = bytes(buf[3 : 3 + length])
        del buf[: 3 + length]
        yield kind, payload


# ---------------------------------------------------------------------------
# The three things a telephone system does
# ---------------------------------------------------------------------------


def ask_what_to_do(base: str, key: str, caller: str, called: str) -> str:
    r = requests.get(
        f"{base}/api/telephony/audiosocket/answer",
        params={"from": caller, "to": called},
        headers={"x-fosnie-telephony-key": key},
        timeout=10,
    )
    if r.status_code != 200:
        raise SystemExit(
            f"the deployment would not take this call ({r.status_code}). "
            "Check the number is registered and switched on, that the line is set to "
            "your own telephone system, and that the shared secret matches."
        )
    ticket = r.text.strip()
    if len(ticket) != 36:
        raise SystemExit(f"expected an identifier, got {ticket!r}")
    return ticket


def ask_who_to_ring(base: str, key: str, ticket: str) -> str:
    r = requests.get(
        f"{base}/api/telephony/audiosocket/continue",
        params={"call": ticket},
        headers={"x-fosnie-telephony-key": key},
        timeout=10,
    )
    return r.text.strip() if r.status_code == 200 else ""


# ---------------------------------------------------------------------------
# Audio in and out
# ---------------------------------------------------------------------------


def read_wav_as_line_audio(path: str) -> bytes:
    """A sound file as the samples a telephone line carries."""
    import numpy as np

    with wave.open(path, "rb") as w:
        if w.getsampwidth() != 2:
            raise SystemExit("the sound file has to be sixteen bit")
        raw = np.frombuffer(w.readframes(w.getnframes()), dtype="<i2")
        if w.getnchannels() > 1:
            raw = raw.reshape(-1, w.getnchannels()).mean(axis=1)
        rate = w.getframerate()
    if rate != RATE:
        # Straight interpolation. This is one side of a demonstration call, not a
        # signal chain: the deployment's own conversion is the one that matters.
        want = int(len(raw) * RATE / rate)
        raw = np.interp(
            np.linspace(0, len(raw) - 1, want), np.arange(len(raw)), raw.astype("f4")
        )
    return raw.astype("<i2").tobytes()


class Speaker:
    """What comes back, played or written down."""

    def __init__(self, save_to: str | None, live: bool):
        self.buffer = bytearray()
        self.save_to = save_to
        self.stream = None
        if live:
            import sounddevice as sd

            self.stream = sd.RawOutputStream(
                samplerate=RATE, channels=1, dtype="int16", blocksize=FRAME_SAMPLES
            )
            self.stream.start()

    def play(self, pcm: bytes) -> None:
        self.buffer.extend(pcm)
        if self.stream is not None:
            self.stream.write(pcm)

    def close(self) -> None:
        if self.stream is not None:
            self.stream.stop()
            self.stream.close()
        if self.save_to and self.buffer:
            with wave.open(self.save_to, "wb") as w:
                w.setnchannels(1)
                w.setsampwidth(2)
                w.setframerate(RATE)
                w.writeframes(bytes(self.buffer))
            print(f"  what the line said is in {self.save_to}")


# ---------------------------------------------------------------------------
# The call
# ---------------------------------------------------------------------------


def carry(sock: socket.socket, ticket: str, source, speaker: Speaker, stop: threading.Event):
    sock.sendall(encode(TYPE_ID, bytes.fromhex(ticket.replace("-", ""))))

    def receive():
        buf = bytearray()
        while not stop.is_set():
            try:
                chunk = sock.recv(8192)
            except OSError:
                break
            if not chunk:
                break
            buf.extend(chunk)
            for kind, payload in frames(buf):
                if kind == TYPE_AUDIO:
                    speaker.play(payload)
                elif kind == TYPE_HANGUP:
                    print("\n  the line hung up")
                    stop.set()
                    return
                elif kind == TYPE_ERROR:
                    print(f"\n  the line reported a fault: {payload!r}")
                    stop.set()
                    return
        stop.set()

    reader = threading.Thread(target=receive, daemon=True)
    reader.start()

    # A live line delivers a frame every twenty milliseconds from the moment it is
    # connected. Silence is a frame like any other: stop sending and the deployment
    # rightly concludes the telephone system has gone.
    next_at = time.monotonic()
    while not stop.is_set():
        try:
            sock.sendall(encode(TYPE_AUDIO, source()))
        except OSError:
            break
        next_at += FRAME_SECONDS
        delay = next_at - time.monotonic()
        if delay > 0:
            time.sleep(delay)
        else:
            next_at = time.monotonic()


def microphone_source(stop: threading.Event):
    """Twenty milliseconds of what is being said, or silence if nothing is."""
    import sounddevice as sd

    q: queue.Queue[bytes] = queue.Queue(maxsize=50)

    def on_audio(indata, _frames, _time, _status):
        try:
            q.put_nowait(bytes(indata))
        except queue.Full:
            pass

    stream = sd.RawInputStream(
        samplerate=RATE,
        channels=1,
        dtype="int16",
        blocksize=FRAME_SAMPLES,
        callback=on_audio,
    )
    stream.start()

    def take() -> bytes:
        try:
            return q.get(timeout=FRAME_SECONDS * 2)
        except queue.Empty:
            return SILENCE

    return take, stream


def file_source(pcm: bytes):
    """A sound file down the line, then silence for as long as the call lasts."""
    at = 0

    def take() -> bytes:
        nonlocal at
        frame = pcm[at : at + FRAME_BYTES]
        at += FRAME_BYTES
        if len(frame) < FRAME_BYTES:
            return frame + SILENCE[len(frame) :]
        return frame

    return take


def main() -> None:
    p = argparse.ArgumentParser(description="Place a call at a deployment's own telephone line.")
    p.add_argument("--base", default="http://127.0.0.1:8088", help="where the deployment answers")
    p.add_argument("--host", default="127.0.0.1", help="where it listens for a telephone system")
    p.add_argument("--port", type=int, default=9500)
    # Required, with no default: a shared secret printed in a public repository is
    # one somebody eventually leaves in place on a deployment that answers a real line.
    p.add_argument("--key", required=True, help="the shared secret the deployment expects")
    p.add_argument("--to", required=True, help="the number being rung")
    p.add_argument("--from", dest="caller", default="+447700900123", help="who is ringing")
    p.add_argument("--wav", help="play this sound file down the line instead of a microphone")
    p.add_argument("--save", default="call.wav", help="where to write what the line said")
    p.add_argument("--seconds", type=float, help="hang up after this long")
    args = p.parse_args()

    print(f"ringing {args.to} from {args.caller}")
    ticket = ask_what_to_do(args.base, args.key, args.caller, args.to)
    print(f"  the deployment will take it: {ticket}")

    live = args.wav is None
    stop = threading.Event()
    stream = None
    if live:
        try:
            source, stream = microphone_source(stop)
        except Exception as e:  # no microphone, no sound device, no drivers
            print(f"  no microphone ({e}); sending silence instead")
            source, live = (lambda: SILENCE), False
    else:
        source = file_source(read_wav_as_line_audio(args.wav))

    speaker = Speaker(args.save, live=live)

    sock = socket.create_connection((args.host, args.port), timeout=10)
    sock.settimeout(None)
    print("  the line is up. Speak, and press Ctrl-C to hang up.")

    if args.seconds:
        threading.Timer(args.seconds, stop.set).start()

    try:
        carry(sock, ticket, source, speaker, stop)
    except KeyboardInterrupt:
        print("\n  hanging up")
    finally:
        stop.set()
        try:
            sock.sendall(encode(TYPE_HANGUP))
        except OSError:
            pass
        time.sleep(0.2)
        sock.close()
        if stream is not None:
            stream.stop()
            stream.close()
        speaker.close()

    ring = ask_who_to_ring(args.base, args.key, ticket)
    print(f"  and now ring {ring}" if ring else "  the call is over")


if __name__ == "__main__":
    sys.exit(main())
