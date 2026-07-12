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

"""SERP provider interface — swappable exactly like the reranker/OCR/STT
engines. v1 ships SearXNG only
(zero recurring cost); the interface remains so a paid tier could be added
per-client one day without touching the pipeline."""

from dataclasses import dataclass
from typing import Protocol

from ..config import settings


@dataclass
class SerpResult:
    url: str
    title: str
    snippet: str
    published_date: str | None  # ISO YYYY-MM-DD when known
    engine: str


class SearchProvider(Protocol):
    async def search(self, query: str, recency: str, limit: int) -> list[SerpResult]: ...


def get_provider() -> SearchProvider:
    """The configured primary provider. Unknown values fail loudly — a silent
    fallback would mask a misconfigured deployment."""
    name = settings.web_search_provider.strip().lower()
    if name == "searxng":
        from . import searxng

        return searxng
    raise ValueError(f"unknown web_search_provider: {name!r}")
