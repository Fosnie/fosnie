#!/usr/bin/env python3
"""Reference in-guest agent for the code-interpreter sandbox. Runs INSIDE the
sandbox and executes one model-authored Python job in a scratch directory,
returning stdout/stderr/exit-code plus any files the code produced.

Two transports, ONE execution path (`_run_job`):
  - Firecracker microVM: `--serve` / no args -> listen on AF_VSOCK, one job per
    connection (length-prefixed JSON framing).
  - gVisor (runsc): `--oneshot <job.json> <result.json>` -> read the job from a
    file in the writable /work mount, execute, write the result back. No sockets.

Wire contract (matches backend/src/code_interpreter/{firecracker,gvisor}.rs):
  job    = {language, code, inputs:[{name, b64}], wall_secs, max_output_bytes}
  result = {stdout, stderr, exit_code, files:[{name, b64, mime}]}
  vsock framing: [u32 LE length][JSON] each way.

Zero egress is enforced by the sandbox having no network device (FC: no NIC;
gVisor: `--network=none`); this agent opens no outbound sockets. The rootfs is
fixed (python + pandas/openpyxl/matplotlib/numpy, no runtime pip).
"""

import base64
import json
import mimetypes
import os
import shutil
import socket
import struct
import subprocess
import sys
import tempfile

PORT = int(os.environ.get("PAI_AGENT_PORT", "5005"))


def _prepare_matplotlib_env():
    """Give matplotlib a WRITABLE, pre-warmed config dir. The rootfs is read-only,
    so a baked cache at /opt/mplcache is seeded into MPLCONFIGDIR (a tmpfs path)
    once; child code inherits these via os.environ. Idempotent — the FC init unit
    may already have done this, and gVisor relies on it entirely."""
    os.environ.setdefault("MPLBACKEND", "Agg")
    cfg = os.environ.get("MPLCONFIGDIR", "/tmp/mpl")
    try:
        os.makedirs(cfg, exist_ok=True)
        baked = "/opt/mplcache"
        if os.path.isdir(baked):
            for name in os.listdir(baked):
                src, dst = os.path.join(baked, name), os.path.join(cfg, name)
                if os.path.isfile(src) and not os.path.exists(dst):
                    shutil.copyfile(src, dst)
        os.environ["MPLCONFIGDIR"] = cfg
    except OSError:
        pass  # non-fatal: matplotlib will just rebuild its cache


def _read_exact(conn, n):
    buf = b""
    while len(buf) < n:
        chunk = conn.recv(n - len(buf))
        if not chunk:
            raise ConnectionError("short read")
        buf += chunk
    return buf


def _run_job(job):
    scratch = tempfile.mkdtemp(prefix="pai_ci_")
    before = set(os.listdir(scratch))
    for f in job.get("inputs", []):
        with open(os.path.join(scratch, f["name"]), "wb") as fh:
            fh.write(base64.b64decode(f["b64"]))

    try:
        proc = subprocess.run(
            [sys.executable, "-c", job["code"]],
            cwd=scratch,
            capture_output=True,
            text=True,
            timeout=int(job.get("wall_secs", 30)),
        )
        stdout, stderr, code = proc.stdout, proc.stderr, proc.returncode
    except subprocess.TimeoutExpired:
        return {"stdout": "", "stderr": "execution timed out", "exit_code": 124, "files": []}

    # Collect newly-created files, bounded by max_output_bytes.
    budget = int(job.get("max_output_bytes", 32 * 1024 * 1024))
    files = []
    for name in sorted(set(os.listdir(scratch)) - before):
        path = os.path.join(scratch, name)
        if not os.path.isfile(path):
            continue
        data = open(path, "rb").read()
        if len(data) > budget:
            continue
        budget -= len(data)
        mime = mimetypes.guess_type(name)[0] or "application/octet-stream"
        files.append({"name": name, "b64": base64.b64encode(data).decode(), "mime": mime})

    return {"stdout": stdout, "stderr": stderr, "exit_code": code, "files": files}


def _run_oneshot(job_path, result_path):
    """gVisor path: read one job from a file, execute, write the result. Always
    writes result.json (even on failure) so the host never sees 'no result'."""
    try:
        with open(job_path) as fh:
            job = json.load(fh)
        result = _run_job(job)
    except Exception as e:  # noqa: BLE001 - report, never crash silently
        result = {"stdout": "", "stderr": f"agent error: {e}", "exit_code": 1, "files": []}
    with open(result_path, "w") as fh:
        json.dump(result, fh)


def _serve_vsock():
    """Firecracker path: one job per AF_VSOCK connection, length-prefixed JSON."""
    s = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
    s.bind((socket.VMADDR_CID_ANY, PORT))
    s.listen(1)
    while True:
        conn, _ = s.accept()
        try:
            (length,) = struct.unpack("<I", _read_exact(conn, 4))
            job = json.loads(_read_exact(conn, length))
            result = _run_job(job)
            body = json.dumps(result).encode()
            conn.sendall(struct.pack("<I", len(body)) + body)
        except Exception as e:  # never hang the host; report the failure
            body = json.dumps(
                {"stdout": "", "stderr": f"agent error: {e}", "exit_code": 1, "files": []}
            ).encode()
            try:
                conn.sendall(struct.pack("<I", len(body)) + body)
            except OSError:
                pass
        finally:
            conn.close()


def main():
    _prepare_matplotlib_env()
    args = sys.argv[1:]
    if args and args[0] == "--oneshot":
        if len(args) != 3:
            sys.stderr.write("usage: guest_agent.py --oneshot <job.json> <result.json>\n")
            sys.exit(2)
        _run_oneshot(args[1], args[2])
        return
    _serve_vsock()


if __name__ == "__main__":
    main()
