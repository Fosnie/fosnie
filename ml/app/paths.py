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

"""Path confinement for request-supplied file paths. The Rust backend already
passes trusted paths under the storage tree, but the ML service must not honour
traversal/absolute escapes from any caller (defence-in-depth). When no storage
root is configured (dev), the check is skipped but null bytes are still rejected."""

from pathlib import Path

from fastapi import HTTPException

from .config import settings


def safe_path(candidate: str) -> str:
    """Return the resolved path if it lies within the configured storage root;
    raise 400 otherwise. Works for not-yet-existing write targets (resolve walks
    the parent)."""
    if not candidate or "\x00" in candidate:
        raise HTTPException(status_code=400, detail="invalid path")
    root = settings.storage_root.strip()
    if not root:
        return candidate  # confinement disabled (dev)
    base = Path(root).resolve()
    resolved = Path(candidate).resolve()
    if not resolved.is_relative_to(base):
        raise HTTPException(status_code=400, detail="path outside storage root")
    return str(resolved)
