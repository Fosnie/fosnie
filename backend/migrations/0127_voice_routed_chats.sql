-- Copyright 2026 Private AI Ltd (SC881079)
-- SPDX-License-Identifier: Apache-2.0
CREATE TABLE voice_routed_chats (
    chat_id UUID PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
    source_scope JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
