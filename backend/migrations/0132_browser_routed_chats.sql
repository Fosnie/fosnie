-- Copyright 2026 Private AI Ltd (SC881079)
-- SPDX-License-Identifier: Apache-2.0
-- Adoption is permanent; selecting Default still uses the routed contract.
CREATE TABLE browser_routed_chats (
    chat_id UUID PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
ALTER TABLE chats ADD COLUMN routing_options JSONB NOT NULL DEFAULT '{}'
    CHECK (jsonb_typeof(routing_options)='object');
ALTER TABLE messages ADD COLUMN routing_options JSONB
    CHECK (routing_options IS NULL OR jsonb_typeof(routing_options)='object');
