-- Presentation artefacts: the `pptx-deck` skill
-- emits a JSON slide spec that the ML pptx engine (python-pptx) builds into a real
-- .pptx with native text, tables and charts. Download-only (no preview).
ALTER TYPE artefact_kind ADD VALUE IF NOT EXISTS 'pptx';
