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
# Dependency vulnerability scan — run in CI and before cutting a release. Fails on a
# known advisory in either the Rust or the frontend dependency tree. Dependencies are
# pinned (Cargo.lock + frontend/package-lock.json); this is the gate that catches a
# newly-disclosed CVE in something already pinned.
#
#   Tools (install once):
#     cargo install cargo-audit          # RustSec advisory DB
#     # npm ships `npm audit` built in
#
# Consider also wiring dependabot/renovate for automated bump PRs.
set -euo pipefail

root="$(cd "$(dirname "$0")/../../.." && pwd)"   # repo root (scripts → deploy → backend → root)

echo "== cargo audit (backend) =="
( cd "$root/backend" && cargo audit )

echo
echo "== npm audit (frontend, production deps) =="
( cd "$root/frontend" && npm audit --omit=dev )

echo
echo "Dependency scans passed."
