-- Keep the guard for the lifetime of the output, even after deleting its
-- workflow or destination. Only the output chat may cascade this record.
CREATE TABLE workflow_output_scopes (
    chat_id UUID PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
    run_id UUID NOT NULL UNIQUE,
    group_chat_id UUID NOT NULL,
    recipients UUID[] NOT NULL CHECK (cardinality(recipients) BETWEEN 1 AND 128),
    source_scope JSONB NOT NULL,
    message_id UUID UNIQUE,
    delivered_at TIMESTAMPTZ,
    CHECK ((message_id IS NULL) = (delivered_at IS NULL))
);
