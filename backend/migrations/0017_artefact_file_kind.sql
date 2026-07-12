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

-- 0017_artefact_file_kind.sql — Generic 'file' artefact kind for code-interpreter
-- outputs (results become chat-scoped generated
-- artefacts). Charts/CSVs/etc. are stored in generated_artefacts with kind 'file',
-- their real MIME in `mime` and the filename in `title`. Forward-only; owned by
-- sqlx-cli. (ALTER TYPE ADD VALUE is permitted in a tx on PG 12+; the value is
-- not used within this migration.)

ALTER TYPE artefact_kind ADD VALUE IF NOT EXISTS 'file';
