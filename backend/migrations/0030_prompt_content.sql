-- Prompt content moves into the DB. Previously the text lived in a file on disk
-- (`content_path`) and the row was a pointer; a missing file → 500 → the UI hung
-- on "Loading…". Postgres is the source of truth, audited and host-independent.

ALTER TABLE prompts ADD COLUMN content TEXT;
ALTER TABLE prompts ALTER COLUMN content_path DROP NOT NULL;

-- Drop dead test-junk prompts whose content files were written to temp dirs that
-- no longer exist (security_guards / e2e fixtures). Unrecoverable, so remove them
-- rather than leave permanently-broken rows.
DELETE FROM prompts
 WHERE content IS NULL
   AND (content_path LIKE '/tmp/%' OR content_path LIKE '%pai_test_prompts%');
