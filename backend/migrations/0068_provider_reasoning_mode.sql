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

-- 0067_provider_reasoning_mode.sql — operator override for capability-aware
-- reasoning control.
--
-- Auto-detection (provider-kind by host + model heuristic) is only a default;
-- model names churn and local engines are arbitrary, so an admin can force the
-- reasoning UI/translation mode per llm provider row. NULL = 'auto' (detect).
-- Values: auto | none | toggle | levels | budget | always_on. Forward-only.

ALTER TABLE provider_configs ADD COLUMN reasoning_mode TEXT;
