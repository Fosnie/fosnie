# Connections, models and variants

Connections hold a write-only credential and endpoint. Models reference a
connection; variants add effort and generation settings. Profiles combine the
variants into routing rules. All are Core features.

The authenticated v2 API supports catalogue, availability, connection/deployment/
variant CRUD, profile CRUD/export/import and project bindings under
`/api/routing/v2/`. The existing policy ID is also the profile ID. Personal
profiles may reference visible shared variants with BYOK disabled. Editing a
shared connection/model requires its management permission; personal credentials
and variants never become visible to other users.

Discovery retrieves model IDs only. It performs no model generation and does not
attest capabilities. Configure exact capabilities and supported effort values
before enabling Auto. A disabled connection disables its models. Changed model
configurations invalidate stale variant bindings; changed credentials retain the
model identity while rechecking credential/revision authority at dispatch.

A planning snapshot contains only authorised current variants, policy/catalogue
revisions and an authority hash. It is revalidated at dispatch. Model roles guide
ordinary ranking; explicit task assignments override role recommendations.
Mandatory restrictions, quotas, capabilities, locality and budgets always apply.
The configured capacity is at most 256 active distinct variant configurations;
overflow is explicit. An empty restricted pool cannot fall back to a wider pool.

The v2 protocol negotiates selection/profile versions and never silently drops an
exact variant or profile requirement for an old server. Legacy providers and v1
selection remain supported. Existing SQL migrations are retained for upgrade
checksum compatibility; retired evaluation tables have no active runtime/API.

See [Routing profiles](model-routing-profiles.md) for setup and user rules.
