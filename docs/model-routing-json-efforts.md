# JSON output compatibility by effort

`ModelCapabilities.structured_output_efforts` is an optional operator allowlist
of exact provider API effort values for JSON-producing calls. This is a hard
compatibility restriction, independent of model quality or price.

| Saved value | Behaviour |
|---|---|
| Absent or `null` | Existing structured-output evidence and effort checks apply. |
| `[]` | No JSON-producing call is compatible. Ordinary text/tool calls remain eligible. |
| `["high"]` | JSON is compatible only with an explicitly resolved `high` effort. |

With an allowlist, an unspecified provider default is incompatible even if that
provider currently defaults to a listed value. Allowlisted values must also
appear in `capabilities.efforts`; the list is bounded to16 entries. Capability
evidence must still be declared or verified. Selecting values does not verify
quality or JSON reliability. Do not infer capability from the model name.

Auto resolves each candidate's own preset before checking the restriction.
Explicit request/rule effort takes precedence. Ranked and forced targets,
checked defaults, technical fallback, helpers and dispatch share the same
filter. Manual does not inherit the Auto preset, so a constrained JSON call
requires its own allowed effort. An independent classifier uses its configured
effort, including when it parses plain-text JSON without native schema mode.
An incompatible classifier fails before credentials, lease or model dispatch;
the existing planner may use only its checked default. No effort escalation,
output repair, schema removal or additional inference retry is introduced.

The reason code is `structured_output_effort`; unknown global capability still
uses `structured_output_unknown`. Existing privacy/RBAC/licence/budget checks
also apply. Enterprise consumes this implementation from Core without a new
edition-specific routing implementation. Profiles are already stored as JSON;
no database migration is necessary. Saving changes the normal profile revision.

Settings expose an opt-in restriction and supported-effort checkboxes. Enabling
it starts with an empty deny-all list; removing an API effort also removes it
from that allowlist. Capability edits clear draft quality evidence. A present
allowlist, including an empty one, changes the evaluation fingerprint; absent
allowlists preserve old fingerprints. Old quality records cannot be reused
after changing the restriction. Audited snapshots preserve this distinction.
The opt-in private `routing_pilot` model fixture accepts the same optional field;
existing consumed experiment configurations and ledgers are not modified.

## Local diagnostic limitation

V21 used the installed Qwen4B/Ollama0.30.8 at `none`, synthetic supplied tool data,
zero cloud calls and no weather egress. Four simple direct JSON controls passed.
The original weather prompt then returned a fenced answer through the actual ML
endpoint despite the exact schema being observed before egress. Direct compatible
and native replay of that same history also failed. Adding the tool name did not
resolve it. Fosnie retained HTTP422/`markdown_fence` and the consumed call usage.

The installed version's [official chat-handler source](https://github.com/ollama/ollama/blob/v0.30.8/server/routes.go)
defers the JSON grammar for thinking-capable models with a parser and applies
it on a thinking-to-content transition. The immediate-format exception in that
version is specific to gemma4. This is a plausible runtime explanation for the
observed `none` failure; no parser trace or patched-runtime comparison was made.
Switching endpoints alone did not fix the weather fixture. A few valid JSON
answers do not establish grammar enforcement or general semantic quality.

Keep JSON capability unverified for this tested runtime/preset. A conservative
private profile can set `structured_output_efforts:[]` while ordinary weather
web/tool use remains eligible. Do not add `high` as supported JSON output until
it has been tested for the same runtime/model configuration. No live or saved
deployment profile was changed by this diagnostic, and no P12 evidence was
admitted. Primary local observations and checks are in the workspace V21 report.
