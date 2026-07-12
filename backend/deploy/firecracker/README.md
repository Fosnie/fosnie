# Code-interpreter (Firecracker) — deployment artefacts

The `code_interpreter` tool runs the model's Python in a **zero-egress
Firecracker microVM**, orchestrated by the Rust backend
([backend/src/code_interpreter/firecracker.rs](../../src/code_interpreter/firecracker.rs)).
It is a **Linux + KVM** capability only — enable it with
`features.code_interpreter = true` and the `[code_interpreter_vm]` config on a
bare-metal Linux host. **WSL2 and Docker are not supported KVM hosts.**

The platform code is shipped; what a deployment must build (Pass-2 per spec) is
the **rootfs image + kernel + this guest agent**. The Rust orchestrator boots a
fresh, network-less microVM per execution (boot → run job over vsock → destroy).

## What you build

1. **Kernel** — an uncompressed `vmlinux` Firecracker can boot. Point
   `code_interpreter_vm.kernel_image` at it.
2. **Rootfs** (`rootfs.ext4`, read-only) — a minimal Linux + Python with the
   fixed analysis libraries **pandas, openpyxl, matplotlib, numpy**. **No runtime
   `pip`** (egress is closed; libraries are baked in during servicing). The image
   is a **versioned platform artefact**. Point `code_interpreter_vm.rootfs_image`
   at it.
3. **Guest agent** — install [`guest_agent.py`](guest_agent.py) in the rootfs and
   start it at boot (an init service) so it is already listening on AF_VSOCK
   (`PAI_AGENT_PORT`, default 5005) when the VM starts. It executes one job per
   connection in a scratch dir and returns stdout/stderr/exit + new files.

## Wire protocol (host ↔ guest, vsock)

Firecracker bridges the guest's vsock port to a host Unix socket; the backend
does the `CONNECT <port>` handshake, then:

```
request  : [u32 LE length][JSON job]
response : [u32 LE length][JSON result]
job      = {language, code, inputs:[{name,b64}], wall_secs, max_output_bytes}
result   = {stdout, stderr, exit_code, files:[{name,b64,mime}]}
```

## Zero egress

The orchestrator configures **no network interface** — there is no TAP/bridge and
no route out, so injected code cannot exfiltrate or fetch even in principle
(§A.6.4). Do not add a network device.

## Config (`[code_interpreter_vm]`)

`firecracker_bin`, `kernel_image`, `rootfs_image`, `snapshot_dir`, `socket_dir`,
`vsock_cid`, `vsock_port`, `vcpus`, `mem_mb`, `wall_secs`, `pool_size`,
`max_output_bytes`. See `deploy/config.linux.example.toml`.

## Verify on the deploy box

```
features.code_interpreter = true   # in the boot config
PAI_FIRECRACKER=1 cargo test --test firecracker   # gated integration test
```

Then a live chat: ask the Agent to compute or plot something → confirm a
downloadable artefact appears and the VM has no network.

## Pass-2 / deferred

Snapshot **warm-pool** (restore ~30–200ms) + queue/supervisor — v1 boots per
execution (equally stateless). Exact resource-cap defaults and the formal
threat-model review (DoS exhaustion, kernel-escape probability, side channels)
are spec Pass-2. Image signing / build pipeline (Alpine vs minimal Debian).
