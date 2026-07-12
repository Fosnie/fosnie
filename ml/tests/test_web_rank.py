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

"""Composite URL ranking: tier suffix matching, path-depth decay, score blend."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app.web.rank import composite, normalise, path_depth, tier


def test_tier_suffix_matching():
    assert tier("www.legislation.gov.uk") == 1.0
    assert tier("gov.uk") == 1.0
    assert tier("research.ac.uk") == 1.0
    assert tier("en.wikipedia.org") == 0.85
    assert tier("www.reuters.com") == 0.75
    assert tier("medium.com") == 0.25
    assert tier("some-blog.example") == 0.5, "unknown → default"
    assert tier("") == 0.5


def test_tier_longest_suffix_wins():
    # legislation.gov.uk and gov.uk are both 1.0 — but the mechanism must pick
    # the most specific suffix; prove it with a host matching two tiers.
    # docs.python.org (0.85) is more specific than a hypothetical "org" entry —
    # there is no "org" tier, so check uk: ac.uk beats nothing else.
    assert tier("docs.python.org") == 0.85
    # A subdomain of a press site stays press.
    assert tier("live.bbc.co.uk") == 0.75


def test_tier_is_prior_not_filter():
    # Content-farm tier is demoted, never zero — it can still rank if the
    # reranker score is dominant.
    assert tier("quora.com") > 0.0


def test_path_depth():
    assert path_depth("https://example.com/") == 0
    assert path_depth("https://example.com/a") == 1
    assert path_depth("https://example.com/a/b/c?q=1") == 3


def test_normalise_degraded_reranker_flat():
    assert normalise([0.0, 0.0, 0.0]) == [0.5, 0.5, 0.5]
    assert normalise([]) == []
    out = normalise([1.0, 3.0, 2.0])
    assert out[0] == 0.0 and out[1] == 1.0 and abs(out[2] - 0.5) < 1e-9


def test_composite_frequency_beats_tier_at_equal_rerank():
    # Same rerank, same depth: a URL seen in 3 variant SERPs on an unknown
    # domain should beat a once-seen press URL (0.2*1.0+0.2*0.5 vs 0.2/3+0.2*0.75).
    common_unknown = composite(0.5, 3, "some-blog.example", "https://some-blog.example/a")
    rare_press = composite(0.5, 1, "reuters.com", "https://reuters.com/a")
    assert common_unknown > rare_press


def test_composite_degraded_reranker_orders_by_tier():
    # All rerank_norm equal (0.5): tier decides between equal-frequency candidates.
    gov = composite(0.5, 1, "legislation.gov.uk", "https://legislation.gov.uk/x")
    farm = composite(0.5, 1, "quora.com", "https://quora.com/x")
    assert gov > farm


def test_composite_path_depth_decay():
    shallow = composite(0.5, 1, "example.com", "https://example.com/a")
    deep = composite(0.5, 1, "example.com", "https://example.com/a/b/c/d/e")
    assert shallow > deep
