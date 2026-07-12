-- 0048_web_citations.sql — Web-search citations.
--
-- Web citations are URL-shaped, not document-anchored, so they live in their own
-- table rather than overloading `citations` (resolved decision 2). The dispatcher
-- inserts rows keyed by `turn_id` with `message_id` NULL (it does not know the
-- assistant message id); the chat orchestrator links them post-stream, mirroring
-- the `generated_artefacts` pattern.

CREATE TABLE web_citations (
    id              uuid PRIMARY KEY,
    message_id      uuid REFERENCES messages(id) ON DELETE CASCADE,
    turn_id         uuid NOT NULL,
    url             text NOT NULL,
    title           text,
    domain          text NOT NULL,
    published_date  date,
    fetched_at      timestamptz NOT NULL,
    quote_text      text NOT NULL,
    -- True when the page itself was not fetched (or failed) and the evidence is
    -- the search-engine snippet only.
    snippet_only    boolean NOT NULL DEFAULT false,
    created_at      timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX web_citations_message_idx ON web_citations (message_id);
CREATE INDEX web_citations_turn_idx ON web_citations (turn_id);
