-- A "default" skill is applied to every Agent (existing + future) without an
-- explicit agent_skills binding — used for the platform's built-in document
-- drafting rules. The skill row + its SKILL.md are seeded idempotently at boot
-- (skills_seed.rs), because the on-disk path depends on the runtime storage dir.
ALTER TABLE skills ADD COLUMN is_default BOOLEAN NOT NULL DEFAULT false;
