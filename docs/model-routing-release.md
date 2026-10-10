# Configure model routing

Routing is Apache-2.0 Core functionality in both Fosnie editions. Core does not
require an Enterprise checkout or licence. Enterprise restrictions can narrow
eligible providers and source access through the existing extension interfaces.
Installing the migrations or saving a profile/policy does not enable routed chat.

Optional [reviewed ranking and quality escalation](model-routing-quality.md) add
held-out evaluation imports and bounded structural answer checks for stateless
non-streaming Auto requests. Both policy options default to disabled.

## First-shipment acceptance

The supported first-shipment implementation has passed the consolidated Core and
Enterprise [contract acceptance](model-routing-acceptance.md). Independent P12 model
quality admission and financial-savings claims are separate. The source-review tools
do not require a user to label benchmark answers before using the base feature.
Configure current profiles and explicitly enable the intended controlled caller;
passing acceptance does not activate a deployment.

## Local, cloud and mixed installations

1. Add the intended models through the existing provider settings. Each row
   represents an endpoint, model and credential together. Use its supported
   adapter; do not substitute an incompatible endpoint or assume capabilities
   from a familiar model name. Keep credentials in the existing provider store.
   An explicitly empty key sends no Bearer header and never inherits another
   provider's authentication. Routed inference does not follow HTTP redirects.
2. Open **Providers → Routing** and create current profiles. Declare context and
   output limits, tools/JSON/vision support, compatible effort values, task tiers
   and any known tariffs. An operator declaration is not measured quality.
   Leave unavailable prices/capabilities unknown. A provider manager must attest
   locality before that profile can satisfy a strict local-only request.
3. Create a default policy with an available, compatible checked default. Choose
   Rules, Classifier or Hybrid, then Economy, Balanced or Quality. Preferences
   rank only eligible models; they cannot override access, locality, capability,
   context, effort or budget requirements.
4. For Classifier/Hybrid, explicitly select a judge and its input/output/deadline
   bounds. It describes the task, minimum tier and required capabilities; it
   does not name an executor or invent a model's relative quality. A local judge
   is optional: a cloud-only installation can use a cloud judge. A mandatory
   local-only policy must also allow a trusted local judge and local helpers.
5. Set allowlists, budgets and helper assignments. A rule's executor pool applies
   to the main model and eligible technical alternate. The mandatory policy pool
   applies to every inference purpose, including judge/helpers/fixed services.
   These pools serve different scopes. Helpers use explicit assignments or the
   selected main model; embeddings, rerank, OCR and speech use separate profiles.
6. Use the no-inference preview to inspect declared requirements and exclusions.
   A probe or judge test makes an actual inference call when explicitly launched;
   it does not establish task accuracy. Use bounded evaluation only with
   authorised models and a budget. Review unknowns and failed probes before
   activating a policy or caller entry point.

Local-only installations attest the executor, judge and relevant fixed/helper
profiles as local and use mandatory local-only constraints. Cloud-only setups
need no local LLM runtime. Mixed setups can use a local executor and permitted
cloud judge/helper when only the rule's executor pool is local. Select mandatory
local-only if text must not reach any cloud inference endpoint. Permitted web
search is a separate tool permission; local inference does not itself grant it.

Profile → Model routing supports personal policies using visible shared models.
Personal keys/profiles additionally require enabled BYOK and ownership. A personal
profile cannot self-attest trusted locality or relax deployment restrictions.
Profile/provider edits invalidate old evidence and planned references; reload
current revisions after a conflict rather than overwriting another editor.

## Default, Manual and Auto

Default resolves the effective saved routing-policy default on an adopted caller.
Manual selects the exact visible provider and bypasses the judge; it still obeys
mandatory restrictions, known requirements and the shared run allowance.
Auto determines requirements with the saved strategy and selects a compatible
executor/effort. An omitted selection inherits the saved conversation choice;
explicit Default resets it. Local-only and effort options are saved separately.

Rules-only never invokes a classifier. In Hybrid, a clear terminal rule avoids
the judge; an uncertain unmatched task may invoke it once. Rules can tighten
tiers/pools and use bounded ALL/ANY conditions. Length alone is not difficulty:
short proofs and long extraction have different requirements. Unknown task,
low confidence, truncated context or a failed judge uses a compatible checked
default within the same remaining allowance. Cancellation does not classify or
retry again. Confidence is not a calibrated probability.

Policy Off uses the checked default without executing routing rules or a judge.
Shadow keeps proposed and actual targets separate: the checked default executes,
and any judge spend is still counted. Active executes the eligible proposal.
Actual model/effort is shown only after accounted dispatch, including on reload.
Synthetic connectivity or a model's name never promotes it to verified quality.

## Supported entry points

Each opt-in below is independent and starts disabled. Opening settings, enabling
one caller or adding a policy does not activate the others. Public management
writes require effective `providers.manage`; a scoped-only Enterprise grant is
insufficient for a deployment-wide change.

| Entry point | Supported scope and documentation |
|---|---|
| Browser and paired desktop remote composer | General remote text, saved intent/options, actual metadata and supported regeneration. [Browser](model-routing-browser-chat.md), [desktop](model-routing-desktop.md). Local folders/device tools, attachments and branch continuation are explicitly refused. |
| Raw API and Auto alias | Stateless text, compatible JSON/SSE, no implicit tools/RAG. [API alias](model-routing-api-alias.md). Existing provider labels retain their meaning, and alias activation checks collisions. |
| Agent API | Explicit adopted agent/library/web scope and guarded continuation. Unsupported agent tools or approval/resume fail before actions. [Architecture contract](model-routing.md#public-api-explicit-stateless-routing). |
| Document/research/workflow jobs | Explicit queue opt-in, original actor/source access, immutable selection, fenced lease and accounting. Existing receipt/review/delivery semantics apply; routing does not add permission to send externally. [Contract](model-routing.md#public-document-job-api). |
| Voice and telephone | Separate opt-in with fixed STT/TTS assignments and bounded batch audio. Realtime/speculation and unsupported phone actions remain excluded. [Contract](model-routing.md#fixed-voice-rest-adoption). |
| Immediate artefact export | Supported completed answers and bounded renderer formats without another LLM call. Native PDF rendering has a separate prerequisite. [Contract](model-routing.md#routed-draft-export). |

The full call-site/acceptance map is in [the test baseline](../tools/routing/baseline.md).
Unsupported combinations return an explicit error; they must not silently change
provider, fall back to an environment key, remove effort/tools, adopt a legacy
caller or repeat a stateful action.

## Disable and re-enable

To disable automatic selection while retaining routed execution, save **Off** on
the policy using the current revision. Choose a default compatible with all
mandatory constraints first. Local-only, allowlist, effort/context and budget
checks still apply; an incompatible cloud default fails before credential access
or inference. Off does not discard profiles, policies, decisions or history.
Re-enabling Active applies current references/permissions to new runs.

To stop a caller entirely, disable its independent runtime gate. Current access
witnesses stop/cancel supported in-flight work when they observe revocation;
completed history remains available according to its existing access rules.
Adopted conversations stay adopted and cannot downgrade to legacy. A fresh,
unadopted legacy conversation retains its existing compatibility behaviour, which
is outside the new policy's execution contract. Do not use gate-off as a promise
that every historical legacy caller now follows routing restrictions.

A policy edit never substitutes a new provider midway through an existing answer.
Pinned revisions/access/remaining allowances are rechecked, and stale work fails
or requires explicit replanning. The technical retry is exclusive to eligible
stateless text requests before output/action begins; no tool, voice or durable
job automatically replays a failed attempt with a fresh budget.
An optional quality contract can replace a completed but still buffered stateless
answer once; it shares the technical retry allowance and never replays actions.

## Upgrade and accounting limits

Deploy matching Core backend, protocol/frontend and ML changes. Apply the additive
Core migrations through the normal deployment process; do not create a second
Enterprise routing schema. Legacy provider pointers are backfilled to Manual,
existing defaults keep their precedence, and upgrades leave routing caller gates
and the API alias disabled. This documentation does not authorise a production
migration. No down-migration is required for the operational off switch.
Downgrading to an older binary with the new schema has not been established and
must be checked separately before attempting it.
P12-written JSON profiles/policies include new fields rejected by older strict
readers; follow the [quality feature downgrade guidance](model-routing-quality.md#verification-and-rollout).

Run budgets count judge, main answer, helpers and any eligible retry together.
Reservations remain conservative for failed/cancelled/unconfirmed calls. Known
profile prices are estimates, including when input/reasoning/cache counts are
normalised; they are not invoices or measured savings. Unknown prices/spend remain
unknown. Fixed services have separate byte/call/deadline accounting; hard token
or monetary limits that lack a proven bound refuse those calls before dispatch.
Bounded multimodal requests remain refused until adapter-specific accounting is
supported. See [evaluation/accounting scope](model-routing-evaluation.md).

Reproducible offline fixtures and local end-to-end tests establish execution
contracts. They do not measure model accuracy or establish quality preservation.
Paid live evaluation remains explicit and budgeted; no global Active rollout or
financial-savings claim follows from synthetic results.

## Verification

From Core with the existing project runtimes and isolated PostgreSQL/Redis:

```powershell
./tools/routing/test.ps1 -Lane all -WithEnterprise -Output tools/routing/reports/release
# Repeat only a failed/affected check after a fix:
./tools/routing/test.ps1 -Lane all -WithEnterprise -Only stack -Output tools/routing/reports/stack-repair
./tools/routing/test.ps1 -Lane native -Output tools/routing/reports/native
# Required when native client/connectivity code changes:
./tools/routing/test.ps1 -Lane native-release -Output tools/routing/reports/native-release
```

`all` without `-WithEnterprise` is Core-only and has no private dependency.
PowerShell and `bash tools/routing/test.sh all --with-enterprise` use one shared
queue. Missing required services/checkouts fail before other commands run.
`--list`/`-List` only displays the queue. Generated logs/receipts are ignored by
Git; exact checks and limits are in [the release matrix](model-routing-release-matrix.md).

Native judge contracts and bounded evaluation schemas are documented in [Native structured output](model-routing-native-schema.md).

Model and judge effort settings, precedence, evidence invalidation and offline field diagnostics are documented in [Auto effort presets](model-routing-effort.md).


Provider quota, billing, cooldown and early text failover, with preserved error and
partial-answer feedback, are documented in [Routing failures](model-routing-errors.md).
Enable Technical fallback before output only for the intended Active Auto policy.

The [mixed pilot and optional snippet search](model-routing-mixed-pilot.md) describe
cloud-only judges and Quick search without unused OCR/rerank dependencies.

Upgrade/off-switch and backup recovery are described in [Upgrade and recovery](model-routing-upgrade.md).
