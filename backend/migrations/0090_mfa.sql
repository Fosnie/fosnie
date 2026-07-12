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

-- 0090_mfa.sql — second-factor (TOTP, RFC 6238) for Core local-auth.
-- Opt-in per user, with an admin `auth.require_mfa` "mandatory for
-- everyone" policy (a runtime config_settings knob, not schema). Keycloak-mode 2FA
-- stays Keycloak's job — these columns are only ever written on the local path.
--
--   * `mfa_secret_enc` — the TOTP shared secret, base32, wrapped by the keyring
--     `encrypt_at_rest` (BYOK/rotation-compatible). NULL = not enrolled. Set (but
--     with `mfa_enabled_at` still NULL) during the pending setup→confirm window.
--   * `mfa_enabled_at` — non-NULL once the user has confirmed a code: MFA is live.
--   * `mfa_last_step`  — anti-replay: the last accepted TOTP time-step counter. A
--     candidate code is only accepted for a step strictly greater than this, then
--     CAS-advanced, so a code cannot be replayed within its ±1-step window.
--
-- Recovery codes are single-use, stored only as SHA-256 hashes (high-entropy, so a
-- fast hash is sufficient), shown once at generation; regenerate replaces the set.
-- Forward-only.
ALTER TABLE users
    ADD COLUMN mfa_secret_enc TEXT,
    ADD COLUMN mfa_enabled_at TIMESTAMPTZ,
    ADD COLUMN mfa_last_step  BIGINT;

CREATE TABLE mfa_recovery_codes (
    user_id    UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    code_hash  TEXT        NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    used_at    TIMESTAMPTZ,
    PRIMARY KEY (user_id, code_hash)
);
