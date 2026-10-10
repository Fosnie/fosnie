# Routing settings

Routing administration is open-source Core functionality. Enterprise uses the
same forms and HTTP services, with effective permissions and inference limits
supplied by its existing RBAC and `InferencePolicy` seams.

Deployment managers open **Admin → Providers → Model routing**. Every user can
open **Profile → Model routing**. Personal policies remain available with BYOK
disabled: they can reference visible deployment models. Personal provider and
fixed-profile writes require ownership and enabled BYOK; shared writes and
locality attestation require effective `providers.manage`, not an admin role
name. `GET /api/routing/management` supplies these management capabilities and
validates the original authenticated user and role.

Opening the page performs reads only. No migration or form discovery enables a
gate, runs a probe, or sends a conversation to a model. Browser/desktop remote
chat and routed regeneration gates are independent explicit manager actions.
These remote paths exclude local folders and device tools. Other caller gates
retain their existing settings.

Deployment managers also configure **API Auto alias** here. The alias name,
default objective and enabled flag use their saved revision; a conflict keeps
the draft for review. Alias activation checks all LLM provider names, including
disabled and personal providers. It does not enable the separate API routing
gate: the UI shows `routing disabled` until that gate is explicitly enabled.
Opening this section only reads configuration. See [API Auto alias](model-routing-api-alias.md)
for discovery, raw and agent requests, response metadata and cancellation rules.

## Models and evidence

Connect models through the existing Providers settings, then create their
routing profiles. Adapter, context/output limits, tools/vision/JSON compatibility,
supported effort values, task tiers, prices and locality describe the actual
configured deployment. The form does not infer quality from a model name.

Unknown capability/quality is distinct from an operator declaration and existing
verified evidence. The form cannot create new verified evidence. Editing an
existing measured tier or evaluation provenance downgrades it to declared;
updating a stale provider profile removes its verification and locality
attestation and disables Auto until explicitly reviewed. Evaluation version and
sample count are shown; the current schema does not store an evaluation date.

Prices are entered as USD per million tokens, stored as integer USD millionths
per million tokens. Empty is unknown; `0` is a known zero price. Price source and
checked-at fields are optional, never invented. Monetary run/test limits are
USD, stored as integer USD millionths. Decimal parsing is exact to six places,
and unsafe integer values are rejected in the browser.

Locality must be explicitly attested by a deployment manager for local-only
inference. Saving a personal profile without that permission removes any prior
attestation; the form discloses this before saving. Local-only also applies to
the judge, helpers and fixed services and never grants web tool permission.

## Policies and rules

Default/Manual/Auto describe selection intent. Rules/Classifier/Hybrid describe
the internal Auto strategy. Economy/Balanced/Quality are ranking preferences
inside the eligible pool; they cannot widen deployment or Enterprise limits.

- Off plans the checked default without a judge.
- Shadow records a proposal and plans the checked default for the answer.
- Active applies the Auto proposal.
- Rules performs no classifier calls. Classifier classifies requirements.
  Hybrid tries terminal rules first. Supported browser calls can skip a judge
  when only one compatible candidate remains.

The form assigns the checked default and local or cloud judge separately. It
supports provider allowlists, local-only, call/output/total-token/duration/cost
limits, rules and explicit helper assignments. An unrestricted pool is `null`;
an empty restricted pool denies all providers. The pool includes fixed services
and must permit judges and helpers too. Fixed services retain separate profiles.

Rules have stable UUIDs and integer priorities (higher first, UUID tie-break).
Local conditions combine with AND; selected tasks/purposes/languages match any
value inside that set. Empty sets match any; language codes are case insensitive.
Nested `all` requires every child; a nonempty `any` requires one child. The DSL
permits four levels, sixteen children per node and 512 nodes per policy. Fresh
information and structured output are explicit capability conditions too.
Select/Classify are terminal; Require only raises the minimum tier.
`restrict_pool` is nonterminal and intersects earlier rule pools for the main
Auto executor and its technical alternate. Judge and explicitly assigned helpers
remain separate within mandatory policy/deployment/Enterprise constraints. An
empty main pool fails Active before judge inference; Shadow still plans the
checked default and records no eligible proposal. A classify action is rejected in Rules mode.
Rule efforts use provider API values and are checked again at runtime.

Each save/delete uses the original `expected_revision`. A 409 preserves the
draft and never overwrites another revision automatically. Reload latest
explicitly discards the draft. Only one default can exist in a scope; edit the
existing default or explicitly remove its default flag before assigning another.
The UI does not silently clear another policy's default.

Fixed profiles preserve provider/model/index identities. Active and desired
embedding stages remain separate even when they share a provider UUID. Editing
locality does not change an embedding model or enqueue re-indexing. Configure
the provider/index first when the catalog has no model.

## Previews and explicit tests

`POST /api/routing/preview` remains non-inferencing. It receives declared workload
requirements and an optional `expected_policy_revision`; the UI supplies the
saved revision. The response reports `inference_calls:0`, rollout, proposal,
`execution_target` and judge requirement. `decision.actual` is always null.
Off/Shadow defaults use the same pure hard-filter helper as runtime planning;
Shadow may have no eligible proposal while still having a checked default.
An active path needing a judge has no promised execution target in this dry
response. It does not simulate semantic classification or final helper budgets.

Compatibility tests use the existing profile probe endpoint. The UI requires
an explicit cost cap and known input/output prices. A synthetic connectivity,
tool-schema or JSON call is limited to one call, 128 output tokens and ten
seconds. Returned tools are never executed; evidence is not automatically saved
and answer quality is not evaluated.

`POST /api/routing/classifier-probe` explicitly tests a saved policy's configured
judge, including while rollout is Off:

```json
{
  "policy_id": "<saved policy UUID>",
  "expected_policy_revision": 42,
  "conversation": "Weather in Glasgow?",
  "max_cost_microusd": 1000
}
```

Only the policy's owner or a manager of the shared policy can run it. The
conversation is bounded to 12,000 characters; classification uses the existing
strict requirements schema. Saved judge limits default to a 4096-token
conservative input allowance (serialized UTF-8 bytes including escaping,
instructions and a 1024-token template margin), 512 output tokens, 5 seconds
for a trusted local judge and 3 seconds for cloud/unknown locality. Input/output
ceilings are 16384/2048; timeouts are 50–30000 ms. Oversized input preserves its
beginning and recent continuation; truncation always marks uncertainty and uses
the checked default. Known task/tier/capabilities/effort are never relaxed.
Without a measured tier floor, a confident simple classification can select
Economy. Unknown tasks remain uncertain even with high self-confidence.
The one-call lease intersects deployment, saved policy and edition limits. Only
one completed, identity-matching accounted call can supply classification; absent,
partial or malformed reports retain conservative reservations. The explicit
monetary cap requires known prices. It never runs an answer, tools, fallback or
automatic retry. Failed calls retain conservative ledger reservations and
durable audit receipts. The response exposes requirements and ledger usage,
`answer_calls:0` and `quality_evaluated:false`, not credentials or targets chosen
by the prompt. Rights, original role, policy/judge revisions and current limits
are rechecked before returning; a late stale result is suppressed.

The offline acceptance lane uses an isolated PostgreSQL schema and loopback ML
mock. Browser fixtures exercise these production forms using synthetic HTTP.
No live model or paid test is needed for implementation acceptance; actual
classification quality remains a separate evaluation gate.
