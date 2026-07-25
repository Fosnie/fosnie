-- Copyright 2026 Private AI Ltd (SC881079)
--
-- Licensed under the Apache License, Version 2.0 (the "License");
-- you may not use this file except in compliance with the License.
-- You may obtain a copy of the License at
--
--     http://www.apache.org/licenses/LICENSE-2.0
--
-- Unless required by applicable law or agreed to in writing, software
-- distributed under the License is distributed on an "AS IS" BASIS,
-- WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
-- See the License for the specific language governing permissions and
-- limitations under the License.

-- How strong a boundary a paired machine keeps for a command it runs, as the
-- machine itself reports on each connection. The instance reads this to decide
-- how much it can safely stop asking the person about before running a command.
--
-- The default is the weaker tier: a machine that has never said otherwise, or a
-- client too old to say anything, is treated as keeping only the process-lifetime
-- line, so the person is still asked. A stronger value is only ever written from
-- a genuine device connection, never from a self-description a browser could make.
ALTER TABLE devices
    ADD COLUMN sandbox_tier TEXT NOT NULL DEFAULT 'lifecycle'
    CHECK (sandbox_tier IN ('full', 'lifecycle'));
