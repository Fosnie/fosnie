#!/usr/bin/env bash
# Build the read-only rootfs for the code-interpreter sandbox — BOTH backends
# from ONE image definition:
#   - Firecracker: an ext4 image  ($OUT_DIR/rootfs.ext4)   [rootfs_image]
#   - gVisor:      an OCI rootfs directory ($GVISOR_ROOTFS) [code_interpreter.gvisor_rootfs]
#
# Strategy: build a minimal Debian userland FROM python:3.12-slim-bookworm in
# Docker (glibc, so pandas/openpyxl/matplotlib/numpy install as manylinux wheels
# with no compilation), bake the fixed analysis libraries and the guest agent in,
# then `docker export` the filesystem once and (a) copy it out as the gVisor OCI
# rootfs directory and (b) pack it into an ext4 image with `mkfs.ext4 -d` (no loop
# mount / privileges beyond docker needed).
#
# The image is a VERSIONED, self-contained artefact: NO runtime pip, NO network
# (egress is closed in production). The orchestrator boots it read-only, so the
# in-guest agent needs a writable scratch: our /sbin/init mounts a tmpfs on /tmp
# and gives matplotlib a writable, pre-warmed MPLCONFIGDIR.
#
# Boot contract (must match backend/src/code_interpreter/firecracker.rs):
#   - boot args have no `init=`  -> kernel runs /sbin/init (we install it here)
#   - boot args have no `root=`  -> Firecracker auto-adds root=/dev/vda (ro drive)
#   - guest agent listens on AF_VSOCK PAI_AGENT_PORT (default 5005)
#
# Reproducible; pin the base image + library versions below.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"

: "${OUT_DIR:=/opt/pai/firecracker}"
# gVisor OCI rootfs directory (matches config default code_interpreter.gvisor_rootfs).
: "${GVISOR_ROOTFS:=$OUT_DIR/rootfs}"
# Set EXT4=0 to skip the Firecracker ext4 image (e.g. gVisor-only, KVM-less host).
: "${EXT4:=1}"
: "${BASE_IMAGE:=python:3.12-slim-bookworm}"
: "${ROOTFS_MB:=2560}"                          # ext4 size; ~2.5G fits numpy+pandas+matplotlib
: "${IMG_TAG:=pai-ci-rootfs:build}"
# Pinned analysis libraries baked into the image (no runtime pip in the VM).
: "${PY_LIBS:=numpy==2.1.3 pandas==2.2.3 openpyxl==3.1.5 matplotlib==3.9.2}"
: "${AGENT_SRC:=$HERE/guest_agent.py}"

log() { printf '[build-rootfs] %s\n' "$*" >&2; }

command -v docker >/dev/null || { log "docker required"; exit 1; }
if [ "$EXT4" = "1" ] && ! command -v mkfs.ext4 >/dev/null; then
  log "note: mkfs.ext4 (e2fsprogs) not found; will build the gVisor dir only (set EXT4=0 to silence)."
fi
[ -f "$AGENT_SRC" ] || { log "guest agent not found at $AGENT_SRC"; exit 1; }

mkdir -p "$OUT_DIR"
work="$(mktemp -d)"
trap 'rm -rf "$work"; docker rmi -f "$IMG_TAG" >/dev/null 2>&1 || true' EXIT

cp "$AGENT_SRC" "$work/guest_agent.py"

# --- /sbin/init: our PID 1 inside the microVM -------------------------------
# Mounts pseudo-filesystems + a writable tmpfs scratch, seeds a writable
# matplotlib cache from the baked one, then execs the agent. If the agent ever
# returns, PID 1 exits -> kernel panic=1 reboots -> the VM is torn down (fine: a
# fresh VM boots per execution).
cat >"$work/init" <<'INIT'
#!/bin/sh
mount -t proc     proc     /proc    2>/dev/null
mount -t sysfs    sysfs    /sys     2>/dev/null
mount -t devtmpfs devtmpfs /dev     2>/dev/null
mount -t tmpfs    tmpfs    /tmp     2>/dev/null
# Writable, pre-warmed matplotlib config/cache (rootfs is read-only).
mkdir -p /tmp/mpl
[ -d /opt/mplcache ] && cp -a /opt/mplcache/. /tmp/mpl/ 2>/dev/null
export PAI_AGENT_PORT=5005
export MPLBACKEND=Agg
export MPLCONFIGDIR=/tmp/mpl
export HOME=/tmp
export PYTHONDONTWRITEBYTECODE=1
exec python3 /opt/pai-agent/guest_agent.py
INIT
chmod 0755 "$work/init"

# --- Dockerfile: bake libs + agent + init + warm the mpl font cache ---------
cat >"$work/Dockerfile" <<DOCKER
FROM ${BASE_IMAGE}
ENV DEBIAN_FRONTEND=noninteractive PYTHONDONTWRITEBYTECODE=1
# fontconfig + a font so matplotlib Agg has glyphs; util-linux for mount(8) in init.
RUN apt-get update \\
 && apt-get install -y --no-install-recommends fontconfig fonts-dejavu-core util-linux \\
 && rm -rf /var/lib/apt/lists/*
RUN pip install --no-cache-dir ${PY_LIBS}
COPY guest_agent.py /opt/pai-agent/guest_agent.py
COPY init /sbin/init
# Pre-warm the matplotlib font cache into a baked dir /sbin/init copies to tmpfs.
ENV MPLBACKEND=Agg MPLCONFIGDIR=/opt/mplcache
RUN mkdir -p /opt/mplcache \\
 && python3 -c "import matplotlib; import matplotlib.pyplot as plt; plt.figure(); plt.plot([0,1],[0,1]); plt.savefig('/tmp/_warm.png'); print('mpl', matplotlib.__version__, 'cache ok')" \\
 && chmod -R a+rX /opt/mplcache
DOCKER

log "building image ${IMG_TAG} from ${BASE_IMAGE} (libs: ${PY_LIBS})"
docker build -t "$IMG_TAG" "$work"

# --- export the container fs to a directory ---------------------------------
rootdir="$work/root"
mkdir -p "$rootdir"
cid="$(docker create "$IMG_TAG")"
log "exporting container filesystem ${cid}"
docker export "$cid" | tar -C "$rootdir" -xf -
docker rm "$cid" >/dev/null

# Ensure the kernel finds an init and the agent is where both backends expect it.
[ -x "$rootdir/sbin/init" ] || { log "FATAL: /sbin/init missing in rootfs"; exit 1; }
[ -f "$rootdir/opt/pai-agent/guest_agent.py" ] || { log "FATAL: guest agent missing in rootfs"; exit 1; }

# --- gVisor OCI rootfs directory (used read-only, shared across executions) --
log "materialising gVisor OCI rootfs -> ${GVISOR_ROOTFS}"
rm -rf "$GVISOR_ROOTFS"
mkdir -p "$GVISOR_ROOTFS"
cp -a "$rootdir/." "$GVISOR_ROOTFS/"
log "built gVisor rootfs dir at ${GVISOR_ROOTFS} ($(du -sh "$GVISOR_ROOTFS" | awk '{print $1}'))"

# --- Firecracker ext4 (populate directly from the dir; no loop mount) --------
if [ "$EXT4" = "1" ]; then
  if command -v mkfs.ext4 >/dev/null; then
    img="$OUT_DIR/rootfs.ext4"
    log "packing ${ROOTFS_MB}M ext4 -> ${img}"
    rm -f "$img"
    # -d populates from a directory; slack inodes for the many wheel files.
    mkfs.ext4 -q -F -L pai-ci-root -d "$rootdir" "$img" "${ROOTFS_MB}M"
    log "built $(du -h "$img" | awk '{print $1}') ext4 rootfs at ${img}"
  else
    log "WARNING: mkfs.ext4 not found; skipping the Firecracker ext4 image (gVisor dir built)."
  fi
fi

log "next:"
log "  gVisor : set [code_interpreter] gvisor_rootfs=${GVISOR_ROOTFS}; verify PAI_GVISOR=1 cargo test --test gvisor"
log "  FC     : set [code_interpreter_vm] rootfs_image=${OUT_DIR}/rootfs.ext4; verify PAI_FIRECRACKER=1 cargo test --test firecracker"
