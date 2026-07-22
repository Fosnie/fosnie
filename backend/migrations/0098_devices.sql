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

-- A desktop installation paired to a user account. Identity lives here rather
-- than on the token, so revocation is a property of the machine: withdrawing a
-- device takes effect on the next request whatever token it holds, without
-- waiting for anything to expire.
CREATE TABLE devices (
    id           UUID        PRIMARY KEY,
    user_id      UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name         TEXT        NOT NULL DEFAULT '',
    platform     TEXT        NOT NULL CHECK (platform IN ('windows', 'macos', 'linux')),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at TIMESTAMPTZ NULL,
    revoked_at   TIMESTAMPTZ NULL
);

CREATE INDEX devices_user_idx ON devices (user_id, created_at DESC);

-- A device token is one row in api_keys, distinguished by kind='device' and
-- anchored to its device. The CHECK makes the two kinds structurally distinct
-- rather than a naming convention: a device token cannot exist without a device,
-- and an application key can never carry one, so a mis-set kind fails the insert
-- instead of minting a credential that authorises the wrong surface.
ALTER TABLE api_keys
    ADD COLUMN device_id UUID NULL REFERENCES devices(id) ON DELETE CASCADE;

ALTER TABLE api_keys
    ADD CONSTRAINT api_keys_device_kind_ck
    CHECK ((kind = 'device') = (device_id IS NOT NULL));

-- One live token per device: a bug in the pairing flow cannot leave a machine
-- holding two valid credentials. Revoked rows are exempt so a device can be
-- re-paired after being withdrawn.
CREATE UNIQUE INDEX api_keys_device_uniq ON api_keys (device_id)
    WHERE device_id IS NOT NULL AND revoked_at IS NULL;

-- Short-lived pairing codes. A code is a bearer secret for its lifetime, so only
-- its hash is stored: a leaked table must not let anyone pair. consumed_at makes
-- redemption single-use and leaves the spent attempt visible.
CREATE TABLE device_pairing_codes (
    code_hash   BYTEA       PRIMARY KEY,
    user_id     UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at  TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ NULL
);

CREATE INDEX device_pairing_codes_user_idx ON device_pairing_codes (user_id, created_at DESC);
