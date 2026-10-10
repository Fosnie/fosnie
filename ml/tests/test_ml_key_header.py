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

"""The shared-secret guard on the ML service.

The header was renamed from `X-PAI-ML-Key` to `X-Fosnie-ML-Key`. Both spellings
authenticate, so a backend and an ML service on either side of the rename still
talk to each other; anything presenting neither (nor the Bearer form a Prometheus
scraper uses) is still rejected."""

from fastapi.testclient import TestClient

import app.main as main
from app.config import settings


def _client(monkeypatch, secret: str = "s3cret") -> TestClient:
    monkeypatch.setattr(settings, "shared_secret", secret)
    return TestClient(main.app)


def test_health_is_open_without_the_secret(monkeypatch):
    assert _client(monkeypatch).get("/health").status_code == 200


def test_new_header_authenticates(monkeypatch):
    r = _client(monkeypatch).get("/metrics", headers={"X-Fosnie-ML-Key": "s3cret"})
    assert r.status_code == 200


def test_pre_rename_header_still_authenticates(monkeypatch):
    r = _client(monkeypatch).get("/metrics", headers={"X-PAI-ML-Key": "s3cret"})
    assert r.status_code == 200


def test_bearer_form_authenticates(monkeypatch):
    r = _client(monkeypatch).get("/metrics", headers={"Authorization": "Bearer s3cret"})
    assert r.status_code == 200


def test_wrong_and_missing_secrets_are_rejected(monkeypatch):
    c = _client(monkeypatch)
    assert c.get("/metrics").status_code == 401
    assert c.get("/metrics", headers={"X-Fosnie-ML-Key": "nope"}).status_code == 401
    assert c.get("/metrics", headers={"X-PAI-ML-Key": "nope"}).status_code == 401
