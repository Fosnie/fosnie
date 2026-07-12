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

-- 0067_self_archive.sql — self-serve account deletion (soft-archive).
--
-- A member can delete their own account via `DELETE /api/me/account`: the row is
-- deactivated (login already refuses deactivated users) and the PII anonymised
-- (display name + email tombstoned). Core keeps the row for referential integrity
-- (audit/events/grants FK it) and emits `account.archived` so an Enterprise
-- edition can crypto-shred on top.
--
-- `self_archived_at` distinguishes a GDPR self-delete from an admin suspend (both
-- set `deactivated_at`): self-archived rows are hidden from the admin user-list
-- (no reactivation), whereas admin-suspended rows stay listed for reactivation.
-- Forward-only.

ALTER TABLE users ADD COLUMN self_archived_at TIMESTAMPTZ;
