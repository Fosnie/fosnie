-- User-defined Deep Research report templates.
--
-- The four built-in templates (exploration | formal | freeform | literature) do
-- NOT live here: they stay as code constants in the research service
-- (ml/app/research/templates.py) so their carefully tuned writing instructions
-- never drift through a transcription into SQL. This table holds only the
-- templates users create, personal by default.
CREATE TABLE research_templates (
    id                   UUID        PRIMARY KEY,
    label                TEXT        NOT NULL,
    description          TEXT        NOT NULL DEFAULT '',
    -- [{"heading","brief","expandable":bool,"exec_summary":bool}, ...]; array order
    -- is the report order. The flags are per-section; the shape the research
    -- service actually consumes (a tuple of expandable headings / a placeholder
    -- sentinel) is derived at serialisation, so renaming a heading can never
    -- orphan a flag.
    skeleton             JSONB       NOT NULL DEFAULT '[]'::jsonb,
    writing_instructions TEXT        NOT NULL DEFAULT '',
    outline_mode         TEXT        NOT NULL DEFAULT 'constrained'
                                     CHECK (outline_mode IN ('constrained', 'free')),
    scope                TEXT        NOT NULL DEFAULT 'personal'
                                     CHECK (scope IN ('personal', 'global')),
    created_by           UUID        REFERENCES users(id),
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    archived_at          TIMESTAMPTZ NULL
);

CREATE INDEX research_templates_owner_idx
    ON research_templates (created_by, created_at DESC) WHERE archived_at IS NULL;
