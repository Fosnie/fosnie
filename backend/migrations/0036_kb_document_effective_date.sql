-- The document's own effective date (agreement/report/letter date, or period
-- covered) — extracted at ingestion (ml/app/metadata.py). NULL when none could be
-- determined; retrieval then falls back to the ingestion timestamp. Lets a
-- "monthly report" agent scope by the document's date, not when it was uploaded.

ALTER TABLE kb_documents ADD COLUMN effective_date TIMESTAMPTZ;
