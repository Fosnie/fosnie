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

-- 0024_message_content_encrypted.sql — at-rest encryption flag for DM messages.
-- When a message encryption key is configured, direct-message (kind='dm') bodies
-- are stored AES-256-GCM-encrypted in `content` with this flag set; everything
-- else stays plaintext. Old plaintext rows and new encrypted rows coexist. The
-- generated `content_tsv` then indexes ciphertext for encrypted rows (never
-- matches a plaintext query) — DMs are excluded from cross-message search anyway.
-- Forward-only; owned by sqlx-cli.

ALTER TABLE group_chat_messages
    ADD COLUMN content_encrypted BOOLEAN NOT NULL DEFAULT false;
