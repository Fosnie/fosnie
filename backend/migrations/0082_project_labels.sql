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

-- 0082_project_labels.sql — free-form labels on a Project, used as ABAC resource
-- attributes (D4: "senior associates may read projects labelled 'client-x'").
-- Distinct from the fixed `projects.sector` enum (general|legal) which stays as
-- is; labels are open-vocabulary (classification, practice area, client code).
-- Editable by the project owner or a `projects`-scoped admin; changes audited.

ALTER TABLE projects ADD COLUMN labels TEXT[] NOT NULL DEFAULT '{}';
