---
kind: added
bump: minor
---

# Scheduled tasks that run on your machine

## changelog

Added automations that run against a connected folder on your own machine, with missed runs surfaced and file changes paused for your approval.

## site

An automation can now run against a connected folder on one of your machines, not only on the server. It keeps the same schedule, records a run as missed rather than dropping it if the machine was offline, and pauses any step that changes files until you approve it, from any device you are signed in to.

## detail

A scheduled automation can now be pointed at a connected folder on one of your machines: the schedule stays on the server, but the folder work is carried out by the machine itself, under the trust you granted it. If the machine is offline when a run is due, the run is recorded as missed rather than quietly skipped, and can be made up automatically once the machine reconnects. A step that changes a file waits for your approval instead of failing and can be answered from any device you are signed in to, and for a folder you trust you can let writes go ahead without a prompt each time while deletion is always asked.
