# Model routing failures and recovery

V2 profiles support [one bounded technical fallback](model-routing-profiles.md#fallback-persistence-and-sharing). A failed answer is not sent to an AI judge or replayed for quality assessment.

Routing now carries safe provider failure data through ML, Core and the chat UI.
It distinguishes a confirmed quota/spending error from a plain HTTP 429, payment
requirements, rejected credentials/access, invalid requests, connection failures,
timeouts and unavailable providers. It does not infer billing from arbitrary
exception text. Unknown failure causes stop safely with a general diagnostic.

OpenAI quota detection uses the documented codes `credit_balance_exhausted`,
`organization_spend_limit_exceeded`, `project_spend_limit_exceeded`,
`organization_usage_limit_exceeded` and the compatible `insufficient_quota` type.
See [official error codes](https://developers.openai.com/api/docs/guides/error-codes).
Other adapters preserve their native HTTP status; an ambiguous 429 is not a claim
that the account's credit balance has been measured. No billing API is queried.

## One safe alternate

Enable **Technical fallback before output** on an **Active Auto** policy. The
stateless text API retains its original guard. A separate private browser witness
also permits one alternate for an eligible general browser/paired-desktop text
turn before any token, reasoning, tool call or other model event is emitted.
The ordinary artefact permission attached to agents is not an advertised tool;
the retry path still rejects an actual artefact request or artefact intent.

The same submitted question, turn, original actor, current access witness and
spent budget are retained. A second user message or second judge call is not
created. The alternate must pass the current policy/profile revision, permission,
locality, capability, context, effort, rule-pool and remaining-allowance checks
before its key is resolved. The first failed attempt is never treated as free.
Unknown actual usage may remain conservatively reserved when its upper bound is
known; absent a valid accounting/bound or sufficient allowance, the retry stops.

Manual/Default selection, cancelled work, emitted output, tools, RAG/fixed-service
work, artefacts, voice and durable phases do not replay automatically. Failed ML
infrastructure is not repaired by trying another provider. There is no unlimited
model loop, silent prompt truncation or allowance increase.

Parallel browser title generation has been removed from the pre-answer path.
The existing post-completion title and moderation hooks are retained, with their
original purposes, restrictions and allowance. Explicit auxiliary assignments are
not silently replaced when the main model changes; their own failures remain
subject to their existing handling.

## Cooldown

Provider failures produce revision-bound temporary exclusion in the shared
database. The default is 30 seconds for transient errors and 300 seconds for
confirmed quota/payment/credential failures; a bounded positive Retry-After is
honoured when present. Only current provider/profile references use that state.
Completed but malformed judge replies cool the judge purpose, not ordinary text
inference from the same model. Expired entries are pruned when recording failures.

Credential equality is tracked only where it is known: matching endpoint/key
bindings use an opaque process-peppered digest, never a plaintext key or public
hash. It is not a claim to identify every vendor account or relate different keys.
Provider cooldowns are shared; credential equivalence can be incomplete after a
process restart or across replicas with different peppers. A rotated provider
revision invalidates the old source witness. Access checks remain authoritative.

## User feedback and history

New clients receive an additional `chat.routing_failure` frame; old clients still
receive `chat.error`. Stable codes map to product descriptions without rendering
vendor bodies. Failed/partial answers retain their content and safe failure data
for reload. An authorised submitted question is also retained when planning
fails before the normal turn worker creates its message rows. No write is made
after the original access witness has become invalid.

The UI offers **Choose model** where relevant. **Prepare retry** only restores the
original question into the composer for a safe pre-output failure; it does not
send another request. The user explicitly sends the next turn. Confirmed waits
are shown with a persisted deadline, so reload does not restart the wait.
Partial output is shown with a stopped-generation notice; cancellation is neutral.
Infrastructure/access errors do not suggest that changing the model repairs them.

Successful alternates show **Alternate model answered**. Interrupted attempts
show **Alternate model attempted**. Stored failed-model names are filtered against
the caller's currently visible catalogue; keys, endpoints and hidden provider IDs
are never included in the product diagnostic.

## Migration and verification

Migration `0138_routing_failures.sql` adds nullable message diagnostics/attempts
and private provider/credential cooldown tables. Deploy it through the normal
upgrade process with matching Core/ML/frontend versions. This documentation and
local verification do not authorise production migration or activation.

Synthetic tests exercise native quota/status parsing, error-body bounds, phase
and accounting proof, safe browser alternates, same-message retention, cooldown,
manual refusal, partial output, hard allowance, credential grouping and revision
changes, existing stateless fallback, browser/desktop behaviour and Enterprise
consumption. The product browser fixture verifies typed feedback, preserved
output after reload and preparing a retry without another request. No real credit
exhaustion or external model call is needed. These checks do not admit P12 quality
evidence or establish financial savings.
