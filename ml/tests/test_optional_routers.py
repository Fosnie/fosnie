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

"""Optional-router registration hook.

The Enterprise overlay (`app/enterprise/`, e.g. the `/classify-prompt` moderation
classifier) lives in the fosnie-enterprise edition and is ABSENT from Core. In Core,
`register_optional_routers` is a no-op: the app still boots and
the overlay routes 404, while every Core route is untouched. (The "overlay present"
case is covered by the Enterprise repo's own tests.)"""

from fastapi import FastAPI

import app.main as main
from app.main import register_optional_routers


def _paths(app) -> set[str]:
    return {getattr(r, "path", None) for r in app.routes}


def test_absent_overlay_is_noop():
    bare = FastAPI()
    included = register_optional_routers(bare, subpackage="enterprise_absent")
    assert included == []
    assert "/classify-prompt" not in _paths(bare)


def test_core_routes_independent_of_overlay():
    # Core routes live on the app regardless of the overlay (a Core image keeps them).
    assert "/generate" in _paths(main.app)
    assert "/health" in _paths(main.app)
