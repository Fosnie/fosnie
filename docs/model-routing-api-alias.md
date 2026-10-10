# API Auto alias

The open-source Core API can publish one deployment-configured virtual model for
clients that require a nonempty `model`. It uses the existing routing planner,
inference policy, budgets and audit ledger. Enterprise uses the same path and
can restrict the eligible providers through its existing policy seam.

## Configuration and discovery

Migration 0133 creates a disabled alias named `fosnie/auto`. It enables no caller
or policy. A manager with effective `providers.manage` configures it in
**Admin → Providers → Model routing → API Auto alias**, or through session-authenticated
`GET /api/routing/public-api-alias` and `PUT`:

```json
{
  "name": "fosnie-route/auto",
  "enabled": true,
  "objective": "balanced",
  "expected_revision": 42
}
```

Read the current revision first. Stale saves and name collisions return 409;
there is no automatic overwrite. Names are case-sensitive, 1–128 ASCII letters,
digits, dots, underscores, slashes or hyphens, starting with a letter or digit.
The `agent/` prefix is reserved.

Activation checks the addressable name of **every LLM provider**, including
disabled and other users' personal providers: label, otherwise model, otherwise
provider UUID. Existing names such as `auto` and `fosnie/auto` keep their original
meaning. Choose a different alias when they are occupied. While enabled, the
alias reserves its name against subsequent provider creation or edits. Database
triggers serialize concurrent activation and provider writes; stale
repeatable-read transactions cannot bypass the check. Changing unrelated
providers does not change the alias revision.

The settings status is `disabled`, `conflict`, `routing_disabled` or `ready`.
Collision status takes precedence even while disabled, so an occupied default
name is visible without breaking an upgrade. The separate public routing gate,
`routing.public_api_enabled`, defaults to false; a manager explicitly enables it
through `PUT /api/routing/public-api` with `{"enabled":true}`. The existing
global public-API configuration and the caller's public-API entitlement must
also allow access. Enabling the alias does not change policy rollout or permit
any previously forbidden model.

Once ready, API-key-authenticated `GET /v1/models` lists the configured name with
`owned_by: "fosnie-routing"` and `fosnie_routing.virtual_alias: true`.
`GET /v1/routing` reports `virtual_alias` and `alias_configuration`. Disabled or
conflicting aliases are not listed. A matching enabled alias with the routing
gate off returns 503 on completion; it does not execute a legacy model.

## Raw completions

Send the published alias as `model` to `POST /v1/chat/completions`:

```json
{
  "model": "fosnie-route/auto",
  "messages": [{"role": "user", "content": "Explain this algorithm."}],
  "max_completion_tokens": 1024,
  "stream": false
}
```

This implies Auto with the alias's saved objective and the caller's effective
personal/deployment policy. Rules/Classifier/Hybrid and Off/Shadow/Active retain
their ordinary meaning. An explicit Auto `model_selection` can supply another
objective or a permitted policy; Default/Manual with the alias is a 400.

Raw calls are stateless text completions. They support the existing text and
sampling subset and never start Fosnie tools, web search or RAG. Client tools,
images, unsupported extra fields and `x-fosnie-chat-id` are refused before
inference. If the task requires Fosnie tools, use an agent request. Eligibility,
known-price requirements under monetary caps, helper/judge budgets and lack of
a usable route follow the shared planner rather than silently choosing a model.

## Agent completions

The alias does not replace the `agent/<uuid>` namespace. To use it with an agent,
add the explicit `fosnie_agent_id` UUID:

```json
{
  "model": "fosnie-route/auto",
  "fosnie_agent_id": "00000000-0000-0000-0000-000000000001",
  "messages": [{"role": "user", "content": "Find and summarize recent updates."}],
  "stream": true
}
```

Use an actual visible agent UUID. The supported routed agent path takes one
nonempty user text, configured agent sampling and server-owned history/tools.
It checks the original agent and attached sources, uses the full routed plan,
and returns `x-fosnie-chat-id`. Continue by sending that header with the same
agent and the alias again. Unsupported modes, tool combinations and client
sampling are refused before inference. `fosnie_agent_id` without a ready alias,
an invalid UUID or an explicit null is a 400.

Existing `agent/<uuid>` requests preserve their meaning. Without selection,
unadopted agent conversations remain legacy. An adopted routed conversation
requires explicit routed intent on every turn; sending its chat ID to a legacy
request fails rather than switching execution paths.

## Responses and interruption

JSON completion `model` and every SSE completion chunk's `model` preserve the
requested alias. The JSON response or terminal SSE chunk also contains:

```json
{
  "fosnie_routing": {
    "schema_version": 1,
    "requested_model": "fosnie-route/auto",
    "alias_revision": 43,
    "run_id": "<run UUID>",
    "selection": {"mode": "auto", "objective": "balanced", "policy_id": null},
    "decision": {"actual": {"provider_id": "<provider UUID>", "model": "actual-model", "effort": null}},
    "usage": {"calls": 1, "tokens": 140, "cost_microusd": 0, "unknown_cost_calls": 0}
  }
}
```

This is an abbreviated example, not a promise of a particular model or price.
The actual decision includes the existing policy, revisions, proposal and
reason codes. Agent metadata also carries `agent_id`. Ordinary OpenAI-shaped
`usage` describes the validated final model call; routing usage includes judge
and helper calls. Audit records preserve both the requested alias and actual
target without storing raw prompt text in that trace.

The admitted alias revision is pinned to the request. Disabling, renaming or
editing the alias during planning or execution interrupts the request through
the existing live access checks. Credential, actor, source, policy and edition
checks remain in force. Partial streams can end in an error; interrupted calls
retain ledger reservations and are never automatically replayed. Clients must
not treat partial output as a completed response or transparently retry a
stateful agent turn.

The explicit extension without an alias remains available: omit/empty `model`
and provide `model_selection` for raw calls, or use `agent/<uuid>` with selection
for agents. Its response model contract is unchanged. Offline fixtures use an
isolated database and loopback providers; real classification quality remains a
separate evaluation gate.
