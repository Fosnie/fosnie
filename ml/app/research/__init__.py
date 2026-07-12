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

"""Deep Research synthesis engine: a
memory bank of structured evidence notes → an outline whose nodes bind to
note IDs → a per-section writer (rolling summary + no-repeat register,
citations emitted ONLY as source IDs) → an edit-only coherence pass →
deterministic structure checks. Budgets derive from the runtime
`max_model_len` — one pipeline shape at every context size (32k → 1M)."""
