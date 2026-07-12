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

-- 0074_kb_parent_child.sql — per-KB parent–child chunking toggle.
-- Parent–child chunking (embed small children, expand to the enclosing parent section
-- at retrieval) was a global ML env flag (default OFF), so "edge provisions" living in a
-- child chunk were never expanded for statute/contract Libraries. Promote it to a per-KB
-- setting chosen at creation. Existing KBs keep the previous behaviour (false); flip a
-- specific KB then re-ingest its documents to rebuild chunks with parents.
-- Forward-only; owned by sqlx-cli.

ALTER TABLE knowledge_bases
    ADD COLUMN parent_child BOOLEAN NOT NULL DEFAULT false;
