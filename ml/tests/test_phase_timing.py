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

"""The per-phase wall-clock timer accumulates a row per `_timed`
block and renders a labelled summary with a TOTAL row. It is a strict no-op when no
accumulator is installed (the production/non-streaming path)."""

import asyncio
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app import retrieve


def test_timed_accumulates_rows_and_summary():
    ph = retrieve._Phases()
    token = retrieve._phases.set(ph)
    try:
        async def scenario():
            with retrieve._timed("decompose"):
                await asyncio.sleep(0)
            with retrieve._timed("assemble+parents"):
                await asyncio.sleep(0)

        asyncio.new_event_loop().run_until_complete(scenario())
    finally:
        retrieve._phases.reset(token)

    assert len(ph._rows) == 2
    out = ph.summary()
    assert "decompose" in out
    assert "assemble+parents" in out
    assert "TOTAL" in out


def test_timed_is_noop_without_accumulator():
    # No accumulator installed → _timed must not raise and must record nothing.
    retrieve._phases.set(None)

    async def scenario():
        with retrieve._timed("decompose"):
            await asyncio.sleep(0)

    asyncio.new_event_loop().run_until_complete(scenario())
    assert retrieve._phases.get() is None
