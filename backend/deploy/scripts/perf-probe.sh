#!/usr/bin/env bash
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
#
# Performance probe. Snapshots hardware, key service
# settings, and versions into ONE report for a sizing conversation (companion to the
# Enterprise perf-playbook). READ-ONLY — changes nothing. Every section degrades
# gracefully when a tool/service is absent.
#
# Usage: perf-probe.sh [OUTPUT_FILE]        (default: ./perf-probe-<host>-<date>.txt)
#   DATABASE_URL   optional — probe live Postgres settings
#   QDRANT_URL     optional — probe Qdrant (default http://localhost:6333)

set -u

HOST="$(hostname 2>/dev/null || echo host)"
OUT="${1:-./perf-probe-$HOST-$(date -u +%Y%m%d).txt}"
QDRANT_URL="${QDRANT_URL:-http://localhost:6333}"

have() { command -v "$1" >/dev/null 2>&1; }
sec()  { printf '\n===== %s =====\n' "$1"; }
try()  { if have "$1"; then shift; "$@" 2>&1; else echo "(skip: $1 not installed)"; fi; }

{
  echo "PAI perf-probe — $HOST — $(date -u +%Y-%m-%dT%H:%M:%SZ)"

  sec "OS / kernel"
  uname -a 2>/dev/null || echo "(uname unavailable)"
  [ -r /etc/os-release ] && cat /etc/os-release

  sec "CPU / NUMA"
  try lscpu lscpu
  echo "--- NUMA topology ---"
  try numactl numactl -H

  sec "Memory"
  if [ -r /proc/meminfo ]; then grep -E 'MemTotal|MemAvailable|Huge' /proc/meminfo; else try free free -h; fi

  sec "GPU / VRAM"
  if have nvidia-smi; then
    nvidia-smi --query-gpu=name,memory.total,memory.used,driver_version,pcie.link.gen.current,pcie.link.width.current --format=csv 2>&1
    echo "--- NVLink ---"; nvidia-smi nvlink -s 2>&1 | head -20
  else
    echo "(skip: nvidia-smi not installed — CPU-only or non-NVIDIA)"
  fi

  sec "Disks / block devices"
  try lsblk lsblk -o NAME,SIZE,TYPE,ROTA,MOUNTPOINT
  echo "--- mounts (data paths) ---"
  mount 2>/dev/null | grep -E 'pai|pg|postgres|qdrant' || echo "(no pai/pg/qdrant mounts matched)"

  sec "Tool versions"
  for t in psql redis-server qdrant vllm python uv cargo node nginx; do
    if have "$t"; then printf '%-10s ' "$t"; "$t" --version 2>&1 | head -1; else printf '%-10s (absent)\n' "$t"; fi
  done

  sec "Postgres settings (live)"
  if [ -n "${DATABASE_URL:-}" ] && have psql; then
    for k in server_version shared_buffers effective_cache_size work_mem maintenance_work_mem \
             max_wal_size checkpoint_timeout wal_compression synchronous_commit max_connections; do
      v=$(psql "$DATABASE_URL" -tAqc "SHOW $k;" 2>/dev/null || echo "?")
      printf '  %-24s = %s\n' "$k" "$v"
    done
  else
    echo "(skip: set DATABASE_URL + install psql to probe Postgres)"
  fi

  sec "Qdrant (live)"
  if have curl; then
    curl -fsS "$QDRANT_URL/telemetry" 2>/dev/null | head -c 2000 || echo "(no Qdrant at $QDRANT_URL)"
    echo; echo "--- collections ---"
    curl -fsS "$QDRANT_URL/collections" 2>/dev/null || echo "(collections unavailable)"
  else
    echo "(skip: curl not installed)"
  fi

  sec "systemd CPU/NUMA pinning (pai units)"
  if have systemctl; then
    for u in fosnie-backend pai-ml; do
      echo "--- $u ---"
      systemctl show "$u" -p CPUAffinity -p NUMAPolicy -p NUMAMask -p MemoryMax 2>/dev/null || echo "(no unit)"
    done
  else
    echo "(skip: systemctl not present)"
  fi
} | tee "$OUT"

echo
echo "wrote $OUT — attach it to the sizing analysis (see the Enterprise perf-playbook)."
