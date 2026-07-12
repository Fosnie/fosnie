#!/usr/bin/env bash
# Build/fetch the Firecracker guest kernel (vmlinux) for the code-interpreter microVM.
#
# The code-interpreter boots each microVM with FIXED boot args (see
# backend/src/code_interpreter/firecracker.rs): no `root=`, no `init=`, no network.
# Firecracker auto-adds `root=/dev/vda` for the read-only rootfs drive, and the
# kernel runs the default `/sbin/init` we bake into the rootfs (build-rootfs.sh).
#
# We fetch a PINNED, known-good uncompressed `vmlinux` from the Firecracker CI
# artefact bucket. That kernel is built with the microVM config Firecracker needs,
# crucially INCLUDING virtio-vsock (CONFIG_VSOCKS_VIRTIO / virtio transport built
# in) which the in-guest agent uses. `pip`/egress is irrelevant here: this is a
# static kernel image.
#
# Reproducible: pin the version + sha256 below. To bump, update KERNEL_VERSION and
# KERNEL_SHA256 together (the script fails closed on a checksum mismatch).
#
# Zero-egress note: fetching the kernel needs outbound access on the BUILD host.
# The resulting microVM still has no network device at runtime (that is enforced by
# the orchestrator, not the kernel). Run this on a connected build/deploy host.
#
# ---------------------------------------------------------------------------
# Source-build fallback (if the pinned CI kernel is unavailable or lacks vsock):
#   git clone --depth 1 -b v6.1 https://github.com/torvalds/linux
#   cd linux
#   curl -fsSL -o .config \
#     https://raw.githubusercontent.com/firecracker-microvm/firecracker/main/resources/guest_configs/microvm-kernel-ci-x86_64-6.1.config
#   # ensure vsock is in: CONFIG_VSOCKS=y, CONFIG_VSOCKS_VIRTIO=y,
#   #                     CONFIG_VIRTIO_VSOCKETS=y, CONFIG_VIRTIO_MMIO=y
#   make olddefconfig && make -j"$(nproc)" vmlinux
#   cp vmlinux "$OUT_DIR/vmlinux"
# ---------------------------------------------------------------------------
set -euo pipefail

# --- pinned inputs (bump version + sha together) ---------------------------
: "${ARCH:=x86_64}"
: "${OUT_DIR:=/opt/pai/firecracker}"
: "${FC_VERSION:=v1.10.1}"                 # firecracker binary release to install
: "${KERNEL_VERSION:=5.10.225}"            # Firecracker CI guest kernel (has vsock)
: "${KERNEL_CI_CHANNEL:=v1.11}"            # spec.ccfc.min firecracker-ci channel
# sha256 of the fetched vmlinux; leave empty to RECORD-only on first run, then
# paste the printed value back here to lock it (fail-closed thereafter).
# Recorded + pinned from the 2026-07-12 CI-metrics deploy run (vmlinux-5.10.225).
: "${KERNEL_SHA256:=23b3047df7dada3500d06c8012cc030b921da01e213735f7717d2166cfcf5f06}"

KERNEL_URL="https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/${KERNEL_CI_CHANNEL}/${ARCH}/vmlinux-${KERNEL_VERSION}"
FC_URL="https://github.com/firecracker-microvm/firecracker/releases/download/${FC_VERSION}/firecracker-${FC_VERSION}-${ARCH}.tgz"

log() { printf '[build-kernel] %s\n' "$*" >&2; }

command -v curl >/dev/null || { log "curl required"; exit 1; }
command -v sha256sum >/dev/null || { log "sha256sum required"; exit 1; }

mkdir -p "$OUT_DIR"

# --- 1. firecracker binary --------------------------------------------------
if command -v firecracker >/dev/null 2>&1; then
  log "firecracker already installed: $(command -v firecracker) ($(firecracker --version 2>/dev/null | head -1))"
else
  log "installing firecracker ${FC_VERSION} from ${FC_URL}"
  tmp="$(mktemp -d)"
  curl -fsSL "$FC_URL" -o "$tmp/fc.tgz"
  tar -C "$tmp" -xzf "$tmp/fc.tgz"
  install -m 0755 "$tmp/release-${FC_VERSION}-${ARCH}/firecracker-${FC_VERSION}-${ARCH}" /usr/local/bin/firecracker
  rm -rf "$tmp"
  log "installed $(firecracker --version 2>/dev/null | head -1) -> /usr/local/bin/firecracker"
fi
log "point code_interpreter_vm.firecracker_bin at 'firecracker' (on PATH) or /usr/local/bin/firecracker"

# --- 2. kernel (vmlinux) ----------------------------------------------------
dst="$OUT_DIR/vmlinux"
log "fetching guest kernel ${KERNEL_VERSION} -> ${dst}"
curl -fsSL "$KERNEL_URL" -o "$dst"

got="$(sha256sum "$dst" | awk '{print $1}')"
if [ -z "$KERNEL_SHA256" ]; then
  log "NO KERNEL_SHA256 pinned. Recorded sha256 = ${got}"
  log "Paste that value into KERNEL_SHA256 above to lock the build."
else
  if [ "$got" != "$KERNEL_SHA256" ]; then
    log "CHECKSUM MISMATCH: got ${got}, expected ${KERNEL_SHA256}"; rm -f "$dst"; exit 1
  fi
  log "kernel sha256 verified: ${got}"
fi

# --- 3. sanity: is vsock present in this kernel? ----------------------------
# Best-effort: the CI kernel bundles no separate config, so we grep the image for
# the virtio-vsock driver signature. Absence here is a WARNING, not fatal (the
# real proof is `PAI_FIRECRACKER=1 cargo test --test firecracker` booting a VM and
# the guest agent binding AF_VSOCK). If the boot hangs at "guest vsock not
# reachable", rebuild from source with the vsock configs (see fallback above).
if strings "$dst" 2>/dev/null | grep -qiE 'virtio.?vsock|vhost.?vsock|vmw_vsock'; then
  log "vsock driver signature found in kernel image (good)."
else
  log "WARNING: no obvious vsock signature in kernel image. If the microVM cannot"
  log "reach the guest agent, rebuild from source with CONFIG_VSOCKS_VIRTIO=y."
fi

log "done. vmlinux at ${dst}"
log "next: build-rootfs.sh, then set [code_interpreter_vm] kernel_image=${dst}"
