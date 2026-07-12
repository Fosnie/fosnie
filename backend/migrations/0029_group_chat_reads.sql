-- Tier-3 #12: unread indicators. Per-member read watermark — the highest message
-- sequence the member has seen in a chat. Unread = messages with a higher
-- sequence. Advanced when the member opens the chat (list_messages). Default 0
-- so every existing membership starts with all current messages counted unread
-- until first opened.
ALTER TABLE group_chat_members ADD COLUMN last_read_seq INT NOT NULL DEFAULT 0;
