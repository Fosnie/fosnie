-- Field metadata for prompts: each `{{key}}` in the template can carry a friendly
-- label, an input type (short | long | date | select), optional help text, and
-- options (for select). The template content stays `{{key}}` (render/parse
-- unchanged); this annotates the fields so the UI never shows raw `{{ }}` and can
-- render typed inputs. NULL = a legacy prompt with no metadata (UI derives labels).

ALTER TABLE prompts ADD COLUMN variables JSONB;
