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

"""clause_section_ref extraction. The old extractor captured
commencement dates and neighbouring numbers; the fix rejects dates, prefers the section
heading at the top of a line, and gates any inline match on a section keyword."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app.chunker import _clause_ref


def test_date_is_not_mistaken_for_section():
    chunk = (
        "The provisions came into force on the appointed day (1.10.2007) and apply generally.\n"
        "Under section 239 a member may apply to the court for relief."
    )
    assert _clause_ref(chunk) == "239"


def test_leading_heading_beats_inline_cross_reference():
    # A chunk that IS section 239 but cross-refers section 284 must be tagged 239.
    chunk = "239 Application to the court\nThis section applies notwithstanding section 284 of the Act."
    assert _clause_ref(chunk) == "239"


def test_section_letter_suffix_normalised():
    chunk = "443A Duty to keep records\nEvery company must comply with this section."
    assert _clause_ref(chunk) == "443A"
    assert _clause_ref("s 443a applies here as section 443a") == "443A"


def test_contract_dotted_clause_preserved():
    assert _clause_ref("2.3 Confidentiality\nEach party shall keep the terms confidential.") == "2.3"


def test_no_reference_returns_none():
    assert _clause_ref("A paragraph of ordinary prose with no clause or section heading.") is None


def test_bare_date_only_chunk_is_none():
    assert _clause_ref("Dated 1.10.2007 between the parties.") is None
