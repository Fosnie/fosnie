---
kind: security
bump: patch
---

# MCP grant enforcement

## changelog

Agents can now be granted individual MCP tools, and every MCP call is checked against one authorisation gate, closing grant-bypass gaps.

## site

You can now grant an agent just the MCP tools it needs, not the whole server, and every tool call is checked against a single authorisation gate. Denied attempts are recorded, so a model reaching for a tool it was never given is visible.

## detail

In version 0.1.0, assigning an agent an MCP server offered it every tool that server exposed, with no per-tool control: granting one tool effectively granted all of them, and the tool name the model emitted was passed to the server without being checked against what the agent was allowed to call, so a fabricated name could reach a different tool, or a server the agent was never assigned. Anyone running 0.1.0 with MCP servers is affected. Every MCP tool call now passes one authorisation check (the connector enabled, the server active and readable, the specific tool granted and present in the server's approved list) on both the live path and the resume of an interrupted call, so a revoked grant or a quarantined server refuses rather than running on stale state. Agents can now be granted individual tools instead of the whole server, existing agents keep their current access, and every denied call is recorded in the audit log.
