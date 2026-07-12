-- Deactivate already-parked "ghost" users. When the dev Keycloak realm is recreated,
-- a returning person logs in under a new subject id and their old row was parked by
-- hand to a `stale-…` email but left active, so it leaked into the directory and the
-- analytics. Login now self-heals this (auth/provisioning.rs parks + deactivates the
-- colliding row); this one-off clears the rows parked before that landed. A no-op in
-- any environment without hand-parked rows (e.g. production).
UPDATE users
   SET deactivated_at = now()
 WHERE email LIKE 'stale-%' AND deactivated_at IS NULL;
