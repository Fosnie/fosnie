-- Self-contained HTML artefacts: the
-- `dashboard` skill emits offline-portable HTML pages, and the "Create page"
-- button turns a Deep Research report into one. Vendored libraries (ECharts) are
-- inlined ML-side so the artefact is a true single file with no external URLs.
ALTER TYPE artefact_kind ADD VALUE IF NOT EXISTS 'html';
