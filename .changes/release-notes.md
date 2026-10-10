# Fosnie v0.7.0

## Automatic prompt routing

Connect several local/cloud providers and choose Default, a model directly or Auto.
Rules, Classifier and Hybrid policies select a compatible executor/effort from task
requirements, current profiles, permissions and shared limits. A local judge is
optional; cloud-only deployments can use a cloud judge. This is open-source Core
functionality available with the Enterprise overlay.

The release includes profiles/policies, saved intent and actual-model metadata,
independent caller opt-ins, previews/Shadow rollout and an OpenAI-compatible Auto
alias. Judge/helpers/answer/retry share accounting. Typed provider failures,
cooldowns and one eligible pre-output text alternate preserve the original actor,
question and spent allowance; tool/action/durable work is not replayed.

Quick search can explicitly use bounded search-result excerpts without page
fetching or OCR/reranking inference. Standard/Deep retain their existing behaviour.
Fixed-role and ingestion/index recovery retain explicit assignments and fences.

## Deployment names and upgrades

Templates and services now use Fosnie names. Legacy PAI environment/header/backup
aliases remain compatible where documented; service units, accounts and paths
need host-level changes or explicit overrides. PostgreSQL role/database are not
automatically renamed. Routing gates stay off until explicitly enabled.

Use matched backend/ML/frontend versions and the normal migration runner through
0138. Back up first. Policy Off retains constraints/settings/history; backup
restore is distinct from running an arbitrary older binary on the new schema.

## Documentation and validation

- [Detailed routing guide](https://docs.fosnie.dev/model-routing)
- [v0.7.0 notes and naming migration](https://docs.fosnie.dev/changelog/v0.7.0)
- Contract, failure, mixed/local/cloud/web and actual upgrade/restore checks passed
  in isolated environments; the release workflow builds platform images/assets.

No claim of universally best-model selection, factual accuracy or measured savings
follows from these operational checks. Independent quality admission remains
separate. Platform v0.7.0 does not bump or publish the independently versioned
desktop client.
