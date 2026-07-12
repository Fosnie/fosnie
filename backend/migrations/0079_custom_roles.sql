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

-- 0079_custom_roles.sql — named, additive bundles of fine-grained permissions
-- (Enterprise custom-roles). A custom role is an ADDITIVE
-- set of catalogue permission strings (see backend/src/auth/permissions.rs); a
-- user's effective permissions are the union of their base platform_role and
-- every role assigned to them or their groups. A role never SUBTRACTS.
--
-- `permissions` holds catalogue strings validated against the code catalogue at
-- write time (the DB does not enforce catalogue membership — the API does).
-- `system = true` marks the seeded starter roles whose name is immutable and
-- which cannot be deleted; their permission set is a read-only reference a firm
-- clones to customise.
--
-- Enterprise-only in practice (only the private edition ships a policy that reads
-- these), but Core schema under the unified-migrations rule; empty and inert in a
-- Core deploy (the Core FlatRbacPolicy never consults it).

CREATE TABLE custom_roles (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT        NOT NULL UNIQUE,
    description TEXT        NOT NULL DEFAULT '',
    permissions TEXT[]      NOT NULL DEFAULT '{}',   -- catalogue permission strings
    system      BOOLEAN     NOT NULL DEFAULT false,  -- seeded, immutable name/deletion
    created_by  UUID        REFERENCES users(id),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
