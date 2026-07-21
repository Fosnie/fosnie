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

-- Platform API keys: bearer credentials that let an external application act as
-- a user against this instance.
--
-- token_hash — the SHA-256 (raw 32 bytes) of a 256-bit CSPRNG secret. The
-- plaintext is shown ONCE at creation and never stored; every lookup is by
-- hash, never by prefix. The index is UNIQUE rather than plain: a collision is
-- cryptographically impossible, but the constraint turns a hypothetical
-- auth-confusion bug into an insert error instead of a silent identity swap.
--
-- display_prefix — the first characters of the secret, kept in clear purely so
-- a key is identifiable in the UI ("which of my three keys is this?").
--
-- kind — 'api' today. 'device' is reserved for per-device tokens issued to a
-- desktop client: same hash mechanics, different issuance flow. Declaring the
-- value now means that work needs no second migration and no backfill.
--
-- expires_at NULL = non-expiring. revoked_at is a soft revocation; a revoked or
-- expired key authenticates nothing but stays listable so the owner can see
-- what was withdrawn and when.
CREATE TABLE api_keys (
    id             UUID        PRIMARY KEY,
    user_id        UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind           TEXT        NOT NULL DEFAULT 'api'
                               CHECK (kind IN ('api', 'device')),
    name           TEXT        NOT NULL DEFAULT '',
    token_hash     BYTEA       NOT NULL,
    display_prefix TEXT        NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at   TIMESTAMPTZ NULL,
    expires_at     TIMESTAMPTZ NULL,
    revoked_at     TIMESTAMPTZ NULL
);

CREATE UNIQUE INDEX api_keys_hash_uniq ON api_keys (token_hash);

-- Owner listing (Profile) and the admin per-user view read live keys first.
CREATE INDEX api_keys_user_idx ON api_keys (user_id, created_at DESC);
