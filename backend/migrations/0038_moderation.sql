-- Moderation & accountability.
-- NEUTRAL terminology only — this store is litigation-discoverable, so it records
-- neutral structural facts + a "review priority", never a risk/suspicion verdict.
-- Feature is OFF by default (gated by runtime config_settings `moderation.*`).

CREATE TYPE moderation_status AS ENUM ('open', 'reviewed', 'dismissed');

-- One row per prompt that crossed the inclusion threshold. team_id scopes which
-- moderator may see it (= the chat's project; NULL = no team → firm-wide/break-glass
-- only). All the *_uplift/*_in_* fields are neutral structural facts computed in code.
CREATE TABLE moderation_flags (
    id                       UUID              PRIMARY KEY,
    user_id                  UUID              NOT NULL REFERENCES users(id),
    chat_id                  UUID              NOT NULL REFERENCES chats(id),
    message_id               UUID              NOT NULL REFERENCES messages(id),
    project_id               UUID              REFERENCES projects(id),
    team_id                  UUID              REFERENCES projects(id),
    prompt_excerpt           TEXT              NOT NULL,
    category                 TEXT              NOT NULL,   -- neutral domain label
    out_of_hours             BOOLEAN           NOT NULL,
    project_attached         BOOLEAN           NOT NULL,
    topic_in_user_practice   BOOLEAN           NOT NULL,
    related_matter_in_access BOOLEAN           NOT NULL,
    operational_uplift       BOOLEAN           NOT NULL,
    review_priority          INTEGER           NOT NULL,   -- computed neutral score
    status                   moderation_status NOT NULL DEFAULT 'open',
    created_at               TIMESTAMPTZ       NOT NULL DEFAULT now(),
    reviewed_by              UUID              REFERENCES users(id),
    reviewed_at              TIMESTAMPTZ
);

-- The team's open queue, highest review priority first.
CREATE INDEX moderation_flags_queue_idx
    ON moderation_flags (team_id, status, review_priority DESC, created_at DESC);
-- Retention sweeps scan by age.
CREATE INDEX moderation_flags_created_idx ON moderation_flags (created_at);

-- Which teams a Moderator oversees. This binding — NOT the platform role and NOT
-- access_grants — is the sole gate on flag visibility: an admin who self-adds to a
-- team's grants still has no row here, so sees no flags. Every grant is audited
-- (granted_by + moderation.assignment.created).
CREATE TABLE moderator_assignments (
    moderator_user_id UUID        NOT NULL REFERENCES users(id),
    team_id           UUID        NOT NULL REFERENCES projects(id),
    granted_by        UUID        REFERENCES users(id),
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (moderator_user_id, team_id)
);

CREATE INDEX moderator_assignments_team_idx ON moderator_assignments (team_id);
