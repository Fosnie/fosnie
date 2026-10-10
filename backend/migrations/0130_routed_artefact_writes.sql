-- A render attempt is consumed before contacting the deterministic renderer.
-- An uncertain/failed attempt is never replayed after reconnect or restart.
CREATE TABLE routed_artefact_writes (
    turn_id UUID PRIMARY KEY,
    chat_id UUID NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    message_id UUID NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    actor_user_id UUID NOT NULL REFERENCES users(id),
    artefact_id UUID NOT NULL UNIQUE,
    input_hash TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('started','published','failed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ
);
