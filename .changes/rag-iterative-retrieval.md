---
kind: added
bump: minor
roadmap_id: iterative-retrieval
---

# Iterative retrieval

## changelog

Retrieval now runs as many rounds as a question needs until the evidence is exhausted, and the answering model can search the library again itself when the first pass falls short.

## site

Ask a hard question and the assistant keeps digging. Retrieval now runs as many rounds as it takes against your knowledge bases, and when the answering model spots a gap it searches again on its own, so answers rest on the evidence that is actually there rather than the first passage found.

## detail

Retrieval used to make a single pass and answer from it. It now keeps searching in bounded rounds until the evidence is exhausted, and the answering model can search the library again itself when it spots a gap. Across the evaluation set this finds 51% more of the relevant material (mean section recall 0.57 to 0.86), and when the corpus genuinely lacks something the search stops and says so instead of inventing.
