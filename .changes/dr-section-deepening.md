---
kind: added
bump: minor
roadmap_id: deeper-dr
---

# Deep Research deepens thin sections before writing

## changelog

Deep Research now checks each report section's evidence before writing and runs a targeted search to fill the gaps, so under-supported sections get real sources instead of padding.

## site

Deep Research got more agentic. Before it writes, it now checks each section of the report for whether it has enough evidence, and for any section that comes up short it runs a focused round of extra research to fill the gap, on the web or across your own documents. Thin, padded sections become properly sourced ones, and a files-only run still makes no outbound web calls.

## detail

Deep Research used to gather evidence once and then write every section in a single pass from that shared pool, so a section the outline left short on evidence tended to read thin, with little the writer could actually cite. It now examines each section before writing: the least-supported ones are taken first, a judge decides whether the evidence bound to them is enough to write them well, and a focused search runs for whatever is missing, binding what it finds to that section alone. The web is searched for a web or hybrid run and your own document libraries for a files or hybrid run, so a files-only run stays fully air-gapped throughout. The step is time-boxed and capped on how much a section may gain, it never changes the number or order of sections, and if anything goes wrong it leaves the section as it was and carries on.
