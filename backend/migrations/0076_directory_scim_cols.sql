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

-- 0076_directory_scim_cols.sql — directory-provenance columns for SCIM 2.0 and
-- just-in-time (JIT) group sync (Enterprise SSO/SCIM).
--
-- users:
--   * scim_username — the SCIM `userName` (Okta/Entra send a UPN/login that is
--     frequently NOT the email). UNIQUE for the SCIM uniqueness contract.
--   * managed_by    — 'local' (created/edited here) or 'scim' (owned by the IdP's
--     provisioning engine; the admin UI locks direct editing of such rows).
--   * profile       — projected SCIM/Enterprise-User attributes (title, department,
--     manager, phone, …) kept as a JSONB projection for later ABAC.
--
-- groups:
--   * external_id — the SCIM group `externalId` (IdP's stable group key). UNIQUE.
--   * managed_by  — 'local' | 'scim' | 'idp' (a group minted by JIT sync from a
--     login `groups` claim). SCIM/IdP-managed groups are locked in the UI; grants
--     on them work exactly as for local groups.
--
-- group_members:
--   * source — provenance of the membership: 'manual' | 'scim' | 'idp'. JIT and
--     SCIM removal only ever retract memberships they created; manual grants are
--     never touched by directory sync.
--
-- Core schema (unified migrations). A Core-only deploy leaves every value at its
-- default, so behaviour is unchanged.

ALTER TABLE users
    ADD COLUMN scim_username TEXT UNIQUE,
    ADD COLUMN managed_by    TEXT  NOT NULL DEFAULT 'local',
    ADD COLUMN profile       JSONB;

ALTER TABLE users
    ADD CONSTRAINT users_managed_by_chk CHECK (managed_by IN ('local', 'scim'));

ALTER TABLE groups
    ADD COLUMN external_id TEXT UNIQUE,
    ADD COLUMN managed_by  TEXT NOT NULL DEFAULT 'local';

ALTER TABLE groups
    ADD CONSTRAINT groups_managed_by_chk CHECK (managed_by IN ('local', 'scim', 'idp'));

ALTER TABLE group_members
    ADD COLUMN source TEXT NOT NULL DEFAULT 'manual';

ALTER TABLE group_members
    ADD CONSTRAINT group_members_source_chk CHECK (source IN ('manual', 'scim', 'idp'));
