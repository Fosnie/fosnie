### Added

- Retrieval now runs as many rounds as a question needs until the evidence is exhausted, and the answering model can search the library again itself when the first pass falls short.
- Remote MCP servers can now authenticate each user individually through OAuth 2.1, so an admin adds a server by URL and every user connects once under their own identity.
- Deep Research report types are now user-definable: duplicate one of the four built-ins or start from scratch to set a report's section structure, per-section briefs, outline mode and writing style, personal by default or published deployment-wide under a permission.

### Fixed

- Internal scaffolding calls (history compaction, skill dry-run, report-to-page rendering) no longer inherit the model's default reasoning effort, which on reasoning-heavy models wasted the token budget, inflated cost and latency, and could return nothing.
- Fixed incremental history compaction silently stopping after the first summary on long conversations.

### Security

- Agents can now be granted individual MCP tools, and every MCP call is checked against one authorisation gate, closing grant-bypass gaps.
- Built-in and custom tool calls now pass a single authorisation check before they run, an admin-disabled tool genuinely refuses, and custom tools can be granted to an agent.

Full notes: https://docs.fosnie.dev/changelog/v0.2.0
