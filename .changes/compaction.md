---
kind: fixed
bump: patch
---

# History compaction

## changelog

Fixed incremental history compaction silently stopping after the first summary on long conversations.

## site

Very long conversations now keep compacting their history correctly instead of stalling after the first summary.
