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

-- 0077_scim_resources.sql — lossless SCIM shadow store (Enterprise SSO/SCIM).
-- A SCIM GET must round-trip exactly what the IdP last wrote:
-- if we returned only a lossy projection of our `users`/`groups`, an IdP such as
-- Entra reconciles the difference on every cycle and drifts. So every SCIM
-- resource is stored verbatim as JSON here, and the changeable fields are ALSO
-- projected into `users`/`groups` on each write (that projection is what the rest
-- of the platform reads).
--
--   * kind          — 'user' | 'group'.
--   * owner_id       — FK to users.id / groups.id (the projected row).
--   * resource       — the full SCIM JSON representation as last written by the IdP.
--   * revision       — monotonic per-resource version; rendered as the SCIM ETag
--                      `meta.version = W/"<revision>"` and checked on If-Match.
--
-- Enterprise-only in practice (the SCIM server lives in the private edition), but
-- the table is Core schema under the unified-migrations rule; a Core-only deploy
-- leaves it empty and inert.

CREATE TABLE scim_resources (
    id            UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    kind          TEXT        NOT NULL,
    owner_id      UUID        NOT NULL,
    resource      JSONB       NOT NULL,
    revision      BIGINT      NOT NULL DEFAULT 1,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_modified TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT scim_resources_kind_chk CHECK (kind IN ('user', 'group')),
    UNIQUE (kind, owner_id)
);

-- Fast lookups the SCIM filter fast-paths need on the shadow itself.
CREATE INDEX scim_resources_owner_idx    ON scim_resources (owner_id);
CREATE INDEX scim_resources_external_idx ON scim_resources ((resource ->> 'externalId'));
