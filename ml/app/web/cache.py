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

"""In-memory TTL caches for the web pipeline: repeated
queries within a session shouldn't re-hit the search engines, and repeated
fetches of the same page shouldn't re-download it — both for speed and for
politeness (the pacing budget is finite). Single ML process, single event
loop — a plain OrderedDict with monotonic timestamps suffices; no Redis."""

import time
from collections import OrderedDict
from typing import Any


class TTLCache:
    """TTL + LRU-capped cache. `ttl <= 0` disables the cache entirely (every
    get misses, every set is a no-op) — the runtime off-switch."""

    def __init__(self, ttl: float, max_entries: int = 256) -> None:
        self.ttl = ttl
        self.max_entries = max_entries
        self._data: OrderedDict[Any, tuple[float, Any]] = OrderedDict()

    def get(self, key: Any) -> Any | None:
        if self.ttl <= 0:
            return None
        hit = self._data.get(key)
        if hit is None:
            return None
        stored_at, value = hit
        if time.monotonic() - stored_at >= self.ttl:
            del self._data[key]
            return None
        self._data.move_to_end(key)  # LRU touch
        return value

    def set(self, key: Any, value: Any) -> None:
        if self.ttl <= 0:
            return
        self._data[key] = (time.monotonic(), value)
        self._data.move_to_end(key)
        while len(self._data) > self.max_entries:
            self._data.popitem(last=False)

    def clear(self) -> None:
        self._data.clear()
