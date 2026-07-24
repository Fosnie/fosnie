---
kind: changed
bump: patch
---

# Desktop commands stop cleanly together

## changelog

A command run by the desktop agent that overruns its time limit is now stopped together with any programs it started, rather than leaving them running.
