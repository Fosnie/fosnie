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

"""Blue-green re-index pure-logic tests."""

from app import contextual, reindex


def test_collection_name_slug():
    assert reindex.collection_name(1536, "text-embedding-3-small") == "pai_kb__1536__text_embedding_3_small"
    assert reindex.collection_name(1024, "bge-m3:Q8_0") == "pai_kb__1024__bge_m3_q8_0"
    # Empty/odd model never yields an empty slug.
    assert reindex.collection_name(768, "") == "pai_kb__768__model"


def test_reembed_text_reconstructed_from_payload():
    # The re-index rebuilds the embedded text from the payload: context blurb (if
    # any) + verbatim chunk — exactly what ingest embedded (augment).
    chunk = "Article 28 requires the processor to act only on documented instructions."
    assert contextual.augment("", chunk) == chunk
    assert contextual.augment("This clause is about processor duties.", chunk) == (
        "This clause is about processor duties.\n\n" + chunk
    )
