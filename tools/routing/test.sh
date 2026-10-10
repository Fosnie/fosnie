#!/usr/bin/env bash
set -euo pipefail
task_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
task_python="${FOSNIE_ROUTING_PYTHON:-$task_root/ml/.venv/bin/python}"
if [[ ! -x "$task_python" ]]; then
  printf '%s\n' 'Existing ML Python environment required, or set FOSNIE_ROUTING_PYTHON' >&2
  exit 2
fi
task_lane="${1:-fast}"
if (($#)); then shift; fi
exec "$task_python" "$task_root/tools/routing/checks.py" --lane "$task_lane" "$@"
