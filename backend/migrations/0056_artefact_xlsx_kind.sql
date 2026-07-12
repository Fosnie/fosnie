-- Spreadsheet artefacts: the `xlsx-tables` skill
-- emits a JSON workbook spec that the ML xlsx engine (openpyxl) builds into a real
-- .xlsx. Download-only (no preview).
ALTER TYPE artefact_kind ADD VALUE IF NOT EXISTS 'xlsx';
