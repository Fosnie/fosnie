-- Incremental, persisted context compaction.
-- One rolling summary per chat plus a watermark of the highest message
-- sequence already folded into it. The summary is updated incrementally as
-- older turns age out of the verbatim window, so we never re-summarise the
-- whole history each turn, and it survives a reload. This is ephemeral *chat*
-- compression and is deliberately separate from the persistent memory store
-- (§A.3.2) — it is never written there.
CREATE TABLE chat_summaries (
    chat_id         UUID PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
    summary         TEXT        NOT NULL,
    up_to_sequence  INTEGER     NOT NULL,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
