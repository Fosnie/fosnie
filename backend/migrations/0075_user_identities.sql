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

-- 0075_user_identities.sql — federated-identity linkage (Enterprise SSO/SCIM).
-- Historically `users.id == Keycloak sub`, so a user could only
-- ever have one identity and `email` (UNIQUE) was the sole cross-provider join key
-- — fragile once an IdP is brokered or a directory provisions a user BEFORE their
-- first login (the `sub` does not exist yet). This table decouples the two:
--
--   * `provider` — 'keycloak' for the local realm, 'scim' for a directory-created
--     user not yet linked to a login, or an IdP-broker alias.
--   * `subject`  — the provider's stable identifier (OIDC `sub`, SAML NameID, …).
--
-- The first SSO login LINKS an identity to an existing user (matched by verified
-- email) instead of the old email-parking dance; a directory-provisioned user gains
-- a 'keycloak' identity on their first login. Existing rows are backfilled with the
-- identity they implicitly had (`provider='keycloak', subject=id`) so nothing about
-- current Keycloak logins changes. This is Core schema (unified migrations); a
-- Core-only deploy simply never inserts non-keycloak rows.

CREATE TABLE user_identities (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider   TEXT        NOT NULL,   -- 'keycloak' | 'scim' | IdP-broker alias
    subject    TEXT        NOT NULL,   -- provider-stable subject (sub / NameID)
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (provider, subject)
);

-- Hot path: "which identities does this user hold?" (linking + deprovision).
CREATE INDEX user_identities_user_idx ON user_identities (user_id);

-- Backfill: every existing user implicitly authenticates via the local Keycloak
-- realm with subject == their id. Idempotent under the UNIQUE(provider, subject).
INSERT INTO user_identities (user_id, provider, subject)
SELECT id, 'keycloak', id::text FROM users
ON CONFLICT (provider, subject) DO NOTHING;
