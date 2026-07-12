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

"""The notes-cache contract (pure parts): schema-version gating (a payload at a
different version is a miss → rebuild) and the structured-note → writer-note
flattening. The Qdrant round-trip itself is exercised by the live e2e."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app.research import census as census_mod


def test_valid_cached_requires_current_schema_version():
    assert census_mod._valid_cached({"schema_version": census_mod.SCHEMA_VERSION, "note": {}})
    assert not census_mod._valid_cached(None)
    assert not census_mod._valid_cached({})
    assert not census_mod._valid_cached({"schema_version": census_mod.SCHEMA_VERSION + 1}), (
        "a payload at a different schema version is treated as a miss → rebuild"
    )


def test_note_from_struct_flattens_metadata():
    note = census_mod._note_from_struct({
        "doc_type": "contract",
        "themes": ["termination", "liability"],
        "claims": ["clause 7 allows termination for convenience"],
        "entities": ["Acme Ltd"],
        "dates": ["2026-01-01"],
        "open_questions": ["is notice period 30 or 60 days?"],
        "quotes": ["either party may terminate on 30 days' notice"],
    })
    text = note.text()
    assert "Document type: contract" in text
    assert "Themes: termination; liability" in text
    assert "clause 7 allows termination" in text
    assert "Entities: Acme Ltd" in text
    assert "Dates: 2026-01-01" in text
    assert "Open questions:" in text
    assert note.quotes == ["either party may terminate on 30 days' notice"]


def test_note_from_struct_tolerates_missing_fields():
    note = census_mod._note_from_struct({"claims": ["only a claim"]})
    assert note.claims == ["only a claim"]
    assert note.quotes == []


def test_stub_note_from_filename():
    note = census_mod._stub_note("report.docx", "the opening line of the document body")
    assert note.claims and "report.docx" in note.claims[0]
