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

-- 0080_role_assignments.sql — binds a custom role to a principal (a user OR a
-- group), optionally NARROWED to a scope (delegated administration, D3/§4).
--
--   * principal_type — 'user' assigns to one person; 'group' assigns to every
--     member (synergy with SCIM/IdP groups: an IdP group can carry a role).
--   * scope_type / scope_ids — NULL scope = a GLOBAL holding (admin over that
--     area). A non-NULL scope narrows the role's scoped-capable permissions to a
--     set of groups ('group') or projects ('project'). The per-permission
--     semantics live in code (users.manage@group, grants.manage@project, …); the
--     API rejects a scoped assignment whose role carries a permission with no
--     scoped semantics.
--
-- Membership resolution is 1–2 indexed lookups (by user, and by the user's
-- groups); the Enterprise policy caches the resolved set with epoch invalidation.
--
-- Enterprise-only in practice; Core schema under unified migrations, inert in Core.

CREATE TABLE role_assignments (
    id             UUID           PRIMARY KEY DEFAULT gen_random_uuid(),
    role_id        UUID           NOT NULL REFERENCES custom_roles(id) ON DELETE CASCADE,
    principal_type principal_type NOT NULL,             -- reuse the 0002 enum (user|group)
    principal_id   UUID           NOT NULL,
    scope_type     TEXT           CHECK (scope_type IN ('group', 'project')),  -- NULL = global
    scope_ids      UUID[],                              -- NULL/empty when scope_type IS NULL
    created_by     UUID           REFERENCES users(id),
    created_at     TIMESTAMPTZ    NOT NULL DEFAULT now(),
    -- One assignment per (role, principal, scope shape). COALESCE the nullable
    -- scope columns so duplicate global assignments collide on a stable key.
    UNIQUE (role_id, principal_type, principal_id, scope_type, scope_ids)
);

-- Hot path: "which roles does this principal (user or one of their groups) hold?"
CREATE INDEX role_assignments_principal_idx ON role_assignments (principal_type, principal_id);
-- Hot path: "who holds this role?" (assignment count + cache invalidation).
CREATE INDEX role_assignments_role_idx ON role_assignments (role_id);
