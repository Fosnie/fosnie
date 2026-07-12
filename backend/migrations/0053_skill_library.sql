-- The built-in default skills move from a single Rust-embedded seed to an
-- in-repo `skills/<slug>/` library, seeded by dir-walk at boot (skills_seed.rs).
--
--   slug         — stable identifier of a library skill (the source directory
--                  name). NULL for user-authored skills. Lets the seeder reconcile
--                  shipped updates against the right row across restarts.
--   source_hash  — hash of the files last shipped for this skill. Drives
--                  edit-preserving updates: if the on-disk skill still matches the
--                  shipped hash it is rewritten on upgrade; if a client edited it
--                  (hash differs) the seeder leaves it untouched. NULL for
--                  user-authored skills and for the legacy pre-library seed.
ALTER TABLE skills ADD COLUMN slug        TEXT;
ALTER TABLE skills ADD COLUMN source_hash TEXT;

-- One row per library slug. Multiple NULLs are allowed (user-authored skills),
-- so this does not constrain non-library skills.
CREATE UNIQUE INDEX skills_slug_key ON skills (slug) WHERE slug IS NOT NULL;
