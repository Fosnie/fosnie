# One-off maintenance scripts

Not part of the service. Each script here repairs or builds a derived field on an
existing Qdrant collection after a fix landed in the chunker or the retrieval
pipeline; new deployments never need them, because ingest already writes the field.

All three are idempotent payload-only updates — nothing is re-embedded — so a
re-run on an already-corrected collection reports zero changes.

| Script | What it repairs | Needed when |
| --- | --- | --- |
| `backfill_clause_ref.py` | `clause_section_ref` on chunks written by the old extractor, which captured commencement dates instead of the statute section | the collection was ingested before that fix |
| `backfill_refs_out.py` | `refs_out` + `section_num`, the cross-reference graph deterministic retrieval expansion reads, plus the payload indexes that make the filters fast | as above |
| `backfill_toc.py` | Builds the topic→section TOC index from running headers into `pai_kb_toc` | the KB predates the TOC index, or its headers changed |

Run from `ml/` with Qdrant reachable, e.g. `python scripts/backfill_toc.py --dry-run`.
