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

-- 0083_seed_system_roles.sql — starter least-privilege roles (D2/§2). These are
-- `system = true`: their name is immutable and they cannot be deleted, but a firm
-- clones one to a normal (editable) role to customise. The permission strings are
-- catalogue names (backend/src/auth/permissions.rs). Idempotent: ON CONFLICT keeps
-- a re-run (or a manual rename) from failing. Inert in Core (no policy reads them).

INSERT INTO custom_roles (name, description, permissions, system) VALUES
    ('Auditor',
     'Read the audit log and usage analytics.',
     ARRAY['audit.view', 'analytics.view'],
     true),
    ('User Manager',
     'Manage users and group membership.',
     ARRAY['users.manage', 'users.view', 'groups.manage'],
     true),
    ('Provider Admin',
     'Configure inference providers and deployment settings.',
     ARRAY['providers.manage', 'config.manage'],
     true),
    ('Identity Admin',
     'Manage federated SSO, SCIM and identity settings.',
     ARRAY['identity.manage'],
     true),
    ('Compliance Officer',
     'Audit access, run exports and manage legal holds.',
     ARRAY['audit.view', 'export.run', 'holds.manage'],
     true)
ON CONFLICT (name) DO NOTHING;
