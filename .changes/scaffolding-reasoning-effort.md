---
kind: fixed
bump: patch
---

# Scaffolding reasoning effort

## changelog

Internal scaffolding calls (history compaction, skill dry-run, report-to-page rendering) no longer inherit the model's default reasoning effort, which on reasoning-heavy models wasted the token budget, inflated cost and latency, and could return nothing.

## site

On reasoning-capable models, background steps such as history compaction now run at minimal reasoning effort instead of the model's default, so they stay fast and inexpensive (a pair of routine requests could otherwise run to several dollars) and no longer occasionally come back empty.
