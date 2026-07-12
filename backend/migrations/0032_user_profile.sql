-- Self-service profile: let a user rename themselves and set a personal avatar.
--
-- `display_name_custom` flips true once the user edits their own name; the login
-- upsert (auth/provisioning.rs) then stops overwriting it from the Keycloak token
-- (email/role still sync). Avatar bytes live on disk under storage.avatars_dir;
-- the row only holds the pointer + mime + an updated-at used as a cache-buster.

ALTER TABLE users
    ADD COLUMN display_name_custom BOOLEAN     NOT NULL DEFAULT false,
    ADD COLUMN avatar_path         TEXT,
    ADD COLUMN avatar_mime         TEXT,
    ADD COLUMN avatar_updated_at   TIMESTAMPTZ;
