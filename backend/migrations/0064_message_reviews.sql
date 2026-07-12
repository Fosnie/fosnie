-- Agent action audit view: reverse-link a message to its turn (so the review can pull
-- the agent run + audit trail) and record an explicit human sign-off per assistant
-- message. The audit hash-chain keeps the full decision history; this table holds the
-- latest queryable state for the badge + drawer.

ALTER TABLE messages ADD COLUMN turn_id UUID;

CREATE TYPE review_decision AS ENUM ('approved', 'changes_requested', 'rejected');

CREATE TABLE message_reviews (
    message_id        UUID            PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    chat_id           UUID            NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    decision          review_decision NOT NULL,
    note              TEXT,
    reviewer_user_id  UUID            REFERENCES users(id),
    reviewed_at       TIMESTAMPTZ     NOT NULL DEFAULT now()
);

CREATE INDEX message_reviews_chat_idx ON message_reviews (chat_id);
