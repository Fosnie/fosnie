-- Optional sector tag so the workmode (general / legal) can suggest a default agent
-- in the picker. NULL = usable in any sector. Agents remain globally selectable across
-- modes; this only drives the default suggestion.

ALTER TABLE agents ADD COLUMN sector TEXT;
