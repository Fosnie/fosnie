# Mixed routing and lightweight quick web search

Routing supports local, cloud and mixed models through the same profile/policy
contracts. A cloud judge is permitted in a cloud-only installation; it describes
task requirements while the server ranks the allowed current profiles. Manual
selection and explicitly selected effort remain pinned.

## Optional search-result excerpts

The administrator setting `web_search.quick_snippets_only` is a boolean, default
false, available in Admin configuration as **Web search: use snippets for Quick
searches**. With this setting enabled, effective Quick requests use bounded
search-engine excerpts, preserving domain/egress restrictions, source URLs/dates
and citation persistence. They do not fetch pages or call OCR, reranking or a web
planner/grade model. The answer model still makes its normal, accounted calls.
Empty results explicitly say that current facts cannot be established. Every
digest marks its partial, snippet-only coverage; a forecast is not an observation.

Ordinary Quick, Standard and Deep retain their existing pipeline by default.
Snippet mode cannot be requested through an untrusted tool argument: Core reads
the administrator value and supplies the ML flag. To omit unused fixed services
when planning, the agent must also have a maximum web depth of Quick. If an agent
permits Standard/Deep, preparation keeps the existing fixed dependencies.

The snippet request retains one tiny bounded WebPlan lease for existing transport
and completion attestation. No WebPlan inference executes; a valid complete empty
report releases unused reservations. Missing/malformed accounting does not make
work free. Current policy, actor/source access, cancellation and shared run limits
remain authoritative. A config change to full search requires already pinned
dependencies or fails before inference. Tool turns retain their pinned model and
do not gain the plain-text automatic retry introduced in V45.

## Operational pilot

The isolated V47 pilot configured installed Qwen4B/Llama3B, OpenAI gpt-5.4-mini and
Gemini gemini-3.1-flash-lite. Cloud-only and Mixed policies use Gemini as the judge.
The local web scenario used Qwen without cloud inference. Mixed/Economy weather
used the cloud judge and local Qwen plus actual public search; Mixed/Quality coding
used the cloud judge and GPT Mini. Cloud-only also answered with GPT Mini, and
Manual preserved high effort. These are connectivity and contract observations,
not proof of best-model accuracy or financial savings. Profile quality remains
declared; independent P12 admission stays open.

GPT Mini was configured with a conservative pilot context/output bound and the
documented effort values. See [official model documentation](https://developers.openai.com/api/docs/models/gpt-5.4-mini).
Credentials are read in memory only by the isolated pilot proxy, never placed in
the disposable provider store, state or reports. The proxy is specific to this
local pilot, not a new production provider service. It forwards native Responses
and GenerateContent requests, disables OpenAI response storage, does not retry and
enforces12attempts/6perprovider/65536 conservative token reserve/900seconds.
Requests use public test questions; no private documents or connected sources.

The pilot used actual browser/ML/Core/search/models, with39affected ML checks and
five live scenarios. Actual app TypeScript passed. Existing Enterprise V45
contracts were not rerun because no extension/protocol seam changed; this pilot
does not certify Enterprise-specific live web/source ACL deployment.
Production settings/migrations/Autoactivation and main Ollama11434 were untouched.
