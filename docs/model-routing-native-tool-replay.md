# Native tool replay in routed chat

Responses and Gemini use native tool history rather than the Chat Completions
wire representation. A routed tool step preserves bounded opaque provider
state on its internal ToolCall. The next step validates the binding to the
model/provider and the original call ID/name/arguments before replay.
Tool arguments remain unchanged; metadata is not tool input or UI text.

Responses messages translate to function_call/function_call_output items.
Object tool results become JSON strings. Native function IDs and associated
encrypted reasoning items are preserved. Reasoning summaries are removed from
opaque replay; plaintext thoughts are never included. Gemini retains the exact
functionCall part, thoughtSignature and native ID for the functionResponse.
Metadata for another model/provider is not forwarded. Data above64KiB and
inconsistent calls are rejected. Null replay fields are omitted from ordinary
Chat Completions history. This supports the non-streaming agent tool-loop path;
it does not introduce a general client-side native-state API.

Internal retrieval helpers cap their configured output maximum at the issued
purpose-specific grant. Native reasoning-based binary graders use generation
room from that grant because a four-token visible answer is not a four-token
reasoning response. No helper can increase the grant or change its effort.
Explicit chat/generation sampling still fails before HTTP when it exceeds its
grant. Legacy non-routed helpers retain their existing defaults.

The private flow example delegates active/desired embedding resolution to the
normal Core registry and uses its own database schema, chats, public library
and Qdrant collection. Synthetic protocol replay and real operational replay
are separate. Actual V20 completed10 authenticated WebSocket messages covering
web search, vector retrieval, citations and saved follow-up intent/context.
The library index was seeded with real bge-m3 vectors; document ingestion was
not exercised. Separate Enterprise acceptance tests keep real RBAC/licence/ACL
contracts but do not establish live Enterprise answer-quality results.

The native Glasgow weather probe read weather data but the local model's final
JSON response was rejected for a Markdown fence. No trimming/repair or silent
retry was introduced. This remains a declared-capability/format limitation,
not a successful strict structured-output result.

Workspace report.v20.md/report.v20-summary.json retain the initial V19 native
400 failures and the separate V20 allocation. Operational inputs were reused
for regression, not a new independent quality cohort. P12 promotion remains
disabled pending independent reviews/comparable families and broader workload
coverage. Final SQL count was unavailable because the parent test PostgreSQL
had already stopped; optional test services and ports were removed.

Protocol references: [OpenAI reasoning items](https://developers.openai.com/cookbook/examples/responses_api/reasoning_items),
[Gemini thought signatures](https://ai.google.dev/gemini-api/docs/generate-content/thought-signatures),
[Open-Meteo parameter reference](https://open-meteo.com/en/docs).
