---
kind: security
bump: patch
---

# Native tool enforcement

## changelog

Built-in and custom tool calls now pass a single authorisation check before they run, an admin-disabled tool genuinely refuses, and custom tools can be granted to an agent.

## site

Every tool an assistant can call now passes the same authorisation check before it runs: the agent's grant, the administrator's on/off switch, and the caller's own permissions. A tool an admin has switched off is genuinely off, and a tool the agent was never given cannot be invoked, even by a model that tries to name it directly.

## detail

Version 0.1.0 shipped the built-in tools and admin-defined custom tools without a common authorisation check on the dispatch path. Two gaps affected anyone running 0.1.0: a built-in tool the agent was never granted could still run if a model named it directly (only the code interpreter was gated), and switching a tool off in the admin settings removed it from the list shown to the model but did not stop it running if the name reached execution another way. Both are now closed: every built-in and custom tool call passes one authorisation check before it runs, an admin-disabled tool genuinely refuses, and `edit_document` now enforces its project write permission on ordinary chat turns as well as inside an agent run. Denied calls are recorded in the audit log, custom tools can now be granted to an agent, and existing agent configurations are unaffected: the changes only tighten what was already permitted.
