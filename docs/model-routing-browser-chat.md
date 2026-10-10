# Browser chat routing

General browser and paired desktop remote chats share a capability-discovered
routing entry. Desktop-specific compatibility is described in
[Desktop remote routing](model-routing-desktop.md).
It is an open-source Core feature, also used by the Enterprise overlay. Its
default-off runtime gate is `routing.browser_chat_enabled`. This gate does not
enable API, voice, regeneration or research gates.

`GET /api/routing/browser-chat` advertises schema version 1, the frame
`chat.routed.send`, supported selection modes and the current execution scope.
An actor with `providers.manage` can use `PUT` on the same endpoint with
`{"enabled":true}` or `{"enabled":false}`. Changes use the audited runtime
configuration store. No migration enables the gate automatically.

Configure current model profiles and a default routing policy first. The composer
shows Default, Auto and visible models with current profiles. Auto's objective
is Economy, Balanced or Quality; rules/classifier/hybrid and off/shadow/active
belong to policy administration. Default resolves the configured routing-policy
default. Manual avoids the judge and pins the explicit provider. An existing
legacy conversation keeps its legacy selection until the user adopts routing;
new supported browser drafts use routed Default when the gate is enabled.

The opt-in WebSocket request is:

```json
{
  "version": 1,
  "type": "chat.routed.send",
  "request": {
    "chat_id": null,
    "project_id": null,
    "agent_id": null,
    "content": "Explain this question",
    "model_selection": {"mode":"auto","objective":"balanced"},
    "options": {"local_only":true,"effort":null}
  }
}
```

For an existing owned chat, omitted `model_selection` and `options` inherit the
saved values independently. Explicit Default resets selection; options need an
explicit replacement to reset local-only or effort. Present null selection or
options, unknown nested fields and legacy provider/reasoning/attachment fields
inside the request are errors. Clients never serialize a proposed plan.

Selection/options and a permanent browser adoption marker commit before any
judge call. Failed planning preserves that intent and does not fall back to the
legacy turn. A per-conversation advisory lease rejects concurrent routed sends.
`chat.created` makes a new attempt discoverable; `chat.routing_started` supplies
the turn ID before planning so Stop can cancel the judge. The established
started/token/completed/error frames handle the answer.

Local-only restricts inference, including the judge, helpers and fixed services.
It does not grant web access: the agent still needs web tool permission, and
sources still pass access checks. Effort is accepted only by compatible current
profiles. This first browser entry uses a 1024-token output bound for every
purpose; it cannot relax deployment or Enterprise limits. An active Auto policy
may skip the judge when exactly one compatible candidate remains and no rule
explicitly requests classification. A lower-tier candidate that could become
eligible after classification prevents this shortcut. Other caller paths keep
their existing classification behavior.

`GET /api/chats/{chat_id}/routing` is owner-only and returns saved intent/options,
adoption and scope support. Saved Auto never becomes a legacy request when the
gate is off or configuration reads fail. Old servers without the capability
continue to use the established legacy composer after a 404 capability response;
network/server failures block sending until discovery succeeds. Attachments, desktop folders,
voice/API/job conversations and regeneration-branch continuation are explicitly
unsupported by this browser entry. The backend rejects unsupported provenance,
agent tools and separate legacy overrides before inference. Model controls are
locked during generation, active voice and dictation; local-only hides the
independent dictation/read-aloud controls for that selected turn/answer.

The messages endpoint includes optional `routing` metadata for an owned routed
answer: selection mode, persisted actual model/effort, readable reason codes,
local-only and completion/interruption state. It never exposes a shadow proposal
as the answering model, provider credentials, private labels or exclusion counts.
If the actual provider is no longer visible, the model is withheld and
`actual_unavailable` is true. The immutable stored decision is retained. The
browser merges this metadata after completion and loads it again on reload.

With the independent regeneration capability enabled, a completed routed answer
sends `chat.routed.regenerate`. The server creates a new conversation, preserves
the original history and carries the answer's selection/options into the new
branch. Auto reselects under current policy; Manual uses the same provider's
current compatible profile. The browser keeps the original until the new branch
is announced, then loads that branch's history. Routed editing/restart-in-place
and branch continuation remain refused.

Verification uses `backend/tests/common/model_routing_browser_cases.rs`, the
combined `model_routing_schema` fixture, protocol strict/golden tests and the
production Chat/Composer harness `frontend/tests/document-jobs/chat.spec.ts`.
Only disposable PostgreSQL/Redis, local HTTP mocks and synthetic browser data
are used. These checks do not measure live model quality or savings. Shared
[routing administration](model-routing-settings.md) and the collision-safe
[API Auto alias](model-routing-api-alias.md) have separate documented contracts.
Desktop remote text and regeneration share this entry with original device-key
checks. Remaining stage acceptance is recorded in the implementation plan.
