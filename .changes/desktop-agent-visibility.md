---
kind: added
bump: minor
---

# Seeing what the agent is doing

## changelog

Added a pinned plan above the stream, an end-of-turn summary of the files changed and commands run, an actions-only view, and made a waiting approval impossible to miss: a decision taken on one device settles the request on the others at once, the desktop app carries a taskbar count of approvals waiting, and a failed task raises a notification.

## site

You can now see what an agent is doing as it works. Its plan stays pinned above the conversation and ticks through step by step, and when a turn finishes there is a short summary of the files it changed and the commands it ran, each with how it exited and its output. A waiting approval is harder to miss: decide it on one device and the request settles everywhere you are signed in, the desktop app shows a count on its taskbar of how many are waiting, and a task that fails raises a notification. An actions-only toggle hides the prose so you can review a long turn by what it actually did.

## detail

An agent that is working should be legible without being watched. Several things now make it so, and they work in the browser and the desktop app alike unless noted.

The plan the agent sets for a multi-step turn is pinned above the conversation while it works, collapsed to a single line ("Step 3 of 7") that you can open into the full checklist. It ticks through one step at a time, so where the work has reached is always in view without scrolling. When the turn finishes, a summary takes its place: the files it changed, and the commands it ran with how each exited, how long it took, and the tail of its output to expand. On the desktop the files come with the existing offer to put them back; in a browser the same list shows, read-only, because the change is on somebody else's computer. A turn that changed nothing shows no summary at all.

A request for your approval no longer waits silently. When you approve or reject one, or it times out, the decision is sent to every device you have signed in, so a request you answered on your laptop stops asking on your phone rather than sitting there resolved-but-open. The desktop app keeps a count of approvals waiting on its taskbar and in its tray, cleared as they are dealt with, so one does not go unnoticed behind another window. And a task that fails while its window is in the background raises a notification, the same as a finished answer or a new request does, with clicking it bringing you to the chat it failed in.

Finally, an actions-only toggle in a chat's header hides the assistant's prose and leaves the plan, the tool steps, the approvals and the summary, for reviewing what a long agentic turn did without reading the whole of what it said.
