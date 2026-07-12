-- Tier-2 #8: per-user-group feature flags (config + auth-rbac). A client-admin
-- can DISABLE a host feature (voice / code-interpreter) for the members of a
-- user group. Restrict-only: the global `features.*` flag is the ceiling — a
-- group flag can only turn a feature OFF for that group, never enable one the
-- deployment has turned off. Most-restrictive wins across a user's groups.
--
-- A row means "members of `group_id` have `feature` set to `enabled`". Absence =
-- inherit the global setting. Only `enabled = false` rows actually constrain
-- (under restrict-only), but we store the admin's explicit choice either way.
CREATE TABLE group_feature_flags (
    group_id   UUID        NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    feature    TEXT        NOT NULL,  -- 'voice' | 'code_interpreter'
    enabled    BOOLEAN     NOT NULL,
    updated_by UUID        REFERENCES users(id),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (group_id, feature)
);

CREATE INDEX group_feature_flags_feature_idx ON group_feature_flags (feature) WHERE enabled = false;
