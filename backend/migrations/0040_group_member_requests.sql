-- Matter-owner approval for adding members to access-bearing groups.
-- Group membership resolves to project access live (access_grants principal_type
-- 'group'), so adding someone to a group that grants a project hands them that
-- project's documents. When the requester does not own every project the group
-- grants, the add is held as a request that each affected project owner (the data
-- owner) must approve. Pure team rosters (no grants) and admins are unaffected.

CREATE TABLE group_member_requests (
    id             UUID        PRIMARY KEY,
    group_id       UUID        NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    target_user_id UUID        NOT NULL REFERENCES users(id),
    requested_by   UUID        NOT NULL REFERENCES users(id),
    -- pending | approved | rejected
    status         TEXT        NOT NULL DEFAULT 'pending',
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    decided_at     TIMESTAMPTZ
);

-- One row per project at stake; the request is approved only when every row is
-- approved (an admin may approve on behalf of all). Any rejection rejects the whole.
CREATE TABLE group_member_request_approvals (
    request_id    UUID        NOT NULL REFERENCES group_member_requests(id) ON DELETE CASCADE,
    project_id    UUID        NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    owner_user_id UUID        NOT NULL REFERENCES users(id),
    -- NULL = awaiting | approve | reject
    decision      TEXT,
    decided_by    UUID        REFERENCES users(id),
    decided_at    TIMESTAMPTZ,
    PRIMARY KEY (request_id, project_id)
);

-- Only one open request per (group, target).
CREATE UNIQUE INDEX group_member_requests_open_idx
    ON group_member_requests (group_id, target_user_id)
    WHERE status = 'pending';
CREATE INDEX group_member_request_approvals_owner_idx
    ON group_member_request_approvals (owner_user_id) WHERE decision IS NULL;
