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
-- Announcement banners: an admin-managed, ordered list of notices shown to all
-- users in every section (a top-right corner stack), persistent until dismissed.
--
-- This is the banner LIST only. The welcome message is a separate singleton kept
-- in `config_settings` under the `welcome.*` keys (welcome.enabled/title/body),
-- written via `config::runtime` so it inherits validation + the atomic
-- `config.changed` audit row — the same pattern as `branding.*`.
--
-- Severity is a CHECK'd TEXT (not a new enum) so adding a level later is a
-- constraint swap, not an ALTER TYPE. Per-user dismissal lives client-side
-- (localStorage), so there is no dismissal table here.

CREATE TABLE announcements (
    id           UUID        PRIMARY KEY,
    content      TEXT        NOT NULL,                    -- markdown, rendered escaped
    severity     TEXT        NOT NULL DEFAULT 'info',     -- info|success|warning|error
    dismissible  BOOLEAN     NOT NULL DEFAULT true,
    active       BOOLEAN     NOT NULL DEFAULT true,
    sort_order   INTEGER     NOT NULL DEFAULT 0,
    created_by   UUID        REFERENCES users(id),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT announcements_severity_chk
        CHECK (severity IN ('info', 'success', 'warning', 'error')),
    CONSTRAINT announcements_content_len_chk
        CHECK (char_length(content) BETWEEN 1 AND 1000)
);

CREATE INDEX announcements_active_order_idx
    ON announcements (active, sort_order, created_at);
