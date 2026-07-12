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

-- 0065_local_auth.sql — Core local email/password login.
--
-- Adds an Argon2id password hash to the users cache so Core can authenticate
-- without Keycloak (AUTH_MODE=local, the default open-source build). Nullable:
-- Keycloak-sourced users have no local password. Sessions are opaque tokens in
-- Redis (no table). Forward-only; owned by sqlx-cli.

ALTER TABLE users ADD COLUMN password_hash TEXT;
