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

"""Round-trip cache for Anthropic extended thinking.

During tool use Anthropic REQUIRES the assistant message that made a `tool_use` to
replay its `thinking`/`redacted_thinking` blocks verbatim (with `signature`) on the
next request, else it 400s. The backend tool-loop only persists the OpenAI shape
(`tool_calls[]`) and never sees thinking blocks, so the ML service caches the raw
Anthropic assistant content blocks keyed by each `tool_use_id` and rehydrates them when
that turn is replayed.

In-process (the ML service is a single process) TTL dict with a size bound — entries are
short-lived: a tool loop completes in seconds, and the blocks are only needed until the
turn's final answer is generated. Not security-sensitive; no Redis needed.
"""

import threading
import time
from typing import Any

_TTL_SECONDS = 3600.0  # generous; a tool loop finishes in seconds
_MAX_ENTRIES = 2048  # cap memory; drop oldest beyond this

# tool_use_id -> (expiry_ts, raw_content_blocks). Insertion order = age (dict is ordered).
_store: dict[str, tuple[float, list[dict[str, Any]]]] = {}
_lock = threading.Lock()


def put(tool_use_ids: list[str], blocks: list[dict[str, Any]]) -> None:
    """Map every `tool_use_id` produced in one response to the SAME raw content-block
    list (thinking + redacted_thinking + tool_use, signatures intact)."""
    if not tool_use_ids or not blocks:
        return
    now = time.monotonic()
    exp = now + _TTL_SECONDS
    with _lock:
        for tid in tool_use_ids:
            if tid:
                # Re-insert at the end so refreshed ids count as youngest.
                _store.pop(tid, None)
                _store[tid] = (exp, blocks)
        _evict_locked(now)


def get(tool_use_id: str) -> list[dict[str, Any]] | None:
    """Return the cached raw blocks for a `tool_use_id`, or None if absent/expired."""
    if not tool_use_id:
        return None
    now = time.monotonic()
    with _lock:
        entry = _store.get(tool_use_id)
        if entry is None:
            return None
        exp, blocks = entry
        if exp <= now:
            _store.pop(tool_use_id, None)
            return None
        return blocks


def clear() -> None:
    """Test hook — drop everything."""
    with _lock:
        _store.clear()


def _evict_locked(now: float) -> None:
    """Drop expired entries, then oldest-first until under the size cap. Caller holds
    the lock."""
    expired = [k for k, (exp, _) in _store.items() if exp <= now]
    for k in expired:
        _store.pop(k, None)
    while len(_store) > _MAX_ENTRIES:
        # dict preserves insertion order → first key is oldest.
        oldest = next(iter(_store))
        _store.pop(oldest, None)
