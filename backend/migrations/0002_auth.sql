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

-- 0002_auth.sql — identity & access.
--
-- Adds the users cache-from-Keycloak, groups, and the flat OWUI-style
-- AccessGrants matrix (no inheritance). Backfills the FKs the skeleton
-- migration deferred. Forward-only; owned by sqlx-cli.

-- ---------------------------------------------------------------------------
-- Enums
-- ---------------------------------------------------------------------------

-- Four principals the architecture must tell apart (schema §3.1, 08 §A.7).
-- super_admin is the ephemeral break-glass principal and lives OUTSIDE
-- Keycloak; it appears here for audit/traceability typing, never as a standing
-- Keycloak-sourced account.
CREATE TYPE platform_role AS ENUM ('super_admin', 'client_admin', 'power_user', 'user');

-- AccessGrant principal + resource + permission vocabulary (§3.3).
CREATE TYPE principal_type AS ENUM ('user', 'group');

CREATE TYPE grant_resource_type AS ENUM (
    'project', 'project_knowledge', 'agent', 'skill', 'prompt',
    'chat', 'group_chat', 'project_chat', 'tabular_review', 'automation', 'document'
);

CREATE TYPE permission AS ENUM ('read', 'write', 'share', 'delete');

-- ---------------------------------------------------------------------------
-- users — local cache of Keycloak identities (schema §3.1)
-- id = Keycloak `sub`. Keycloak is authoritative; this is for read-side joins.
-- ---------------------------------------------------------------------------
CREATE TABLE users (
    id             UUID          PRIMARY KEY,
    display_name   TEXT          NOT NULL,
    email          TEXT          NOT NULL UNIQUE,
    role           platform_role NOT NULL,
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT now(),
    last_seen_at   TIMESTAMPTZ,
    deactivated_at TIMESTAMPTZ                        -- soft-delete
);

-- ---------------------------------------------------------------------------
-- groups + membership (schema §3.2). A power_user creates groups and places
-- their team into them.
-- ---------------------------------------------------------------------------
CREATE TABLE groups (
    id         UUID        PRIMARY KEY,
    name       TEXT        NOT NULL,
    created_by UUID        REFERENCES users(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE group_members (
    group_id UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    user_id  UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    PRIMARY KEY (group_id, user_id)
);

-- ---------------------------------------------------------------------------
-- access_grants — flat OWUI-style matrix, NO inheritance (§3.3, §B.13).
-- Projects are flat, so grants never cascade from a parent; document grants
-- are explicit at the resource level.
-- ---------------------------------------------------------------------------
CREATE TABLE access_grants (
    id             UUID                PRIMARY KEY,
    resource_type  grant_resource_type NOT NULL,
    resource_id    UUID                NOT NULL,
    principal_type principal_type      NOT NULL,
    principal_id   UUID                NOT NULL,
    permission     permission          NOT NULL,
    created_by     UUID                REFERENCES users(id),
    created_at     TIMESTAMPTZ         NOT NULL DEFAULT now(),
    UNIQUE (resource_type, resource_id, principal_type, principal_id, permission)
);

-- Hot path: "what may this principal access?"
CREATE INDEX access_grants_principal_idx ON access_grants (principal_type, principal_id);
-- Hot path: "who may touch this resource?"
CREATE INDEX access_grants_resource_idx ON access_grants (resource_type, resource_id);

-- ---------------------------------------------------------------------------
-- Backfill the FKs the skeleton (0001) deferred until users existed.
-- audit_events.actor_user_id stays FK-free on purpose: the audit log must not
-- be constrained by user deletion (separation of duties).
-- ---------------------------------------------------------------------------
ALTER TABLE config_settings
    ADD CONSTRAINT config_settings_updated_by_fkey
    FOREIGN KEY (updated_by) REFERENCES users(id);

ALTER TABLE branding_assets
    ADD CONSTRAINT branding_assets_updated_by_fkey
    FOREIGN KEY (updated_by) REFERENCES users(id);
