-- Persist intent independently of the selected/actual target. Historical rows
-- intentionally stay NULL: an actual model does not reveal Default/Manual/Auto.
ALTER TABLE messages ADD COLUMN routing_selection JSONB
    CHECK (routing_selection IS NULL OR jsonb_typeof(routing_selection)='object');
CREATE TABLE routed_chat_branches (
    chat_id UUID PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
    source_chat_id UUID REFERENCES chats(id) ON DELETE SET NULL,
    source_message_id UUID REFERENCES messages(id) ON DELETE SET NULL,
    source_scope JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
