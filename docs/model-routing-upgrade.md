# Model routing upgrade and recovery

Use a matched Core/Enterprise, ML and frontend candidate. Enterprise remains an
overlay on the same Core revision; migrations are owned by Core. The optional
snippet-only search flag requires the matching ML implementation. Installing
the schema does not activate Auto or any caller gate.

## Prepare the target

Record the edition, running versions, deployment method and current routing
gates/policies. Keep the current application artefacts and a restorable database
backup. Save the existing profile/policy configuration and caller gate values.
Use the deployment's normal migration mechanism and environment-owned secrets;
the pilot proxy and its in-memory key recovery are not production components.

Before replacing components, drain or stop inference/jobs using the deployment's
normal maintenance procedure. Do not introduce a second worker against the same
durable leases. Validate the candidate on a separate copy of the target database
before applying it to the working installation.

## Upgrade

1. Take and verify the pre-upgrade database backup and retained artefacts.
2. Run Core's SQLx migration runner through migration 0138. It adds nullable
   message failures/attempts and private provider/credential health tables.
   Do not execute individual migration files repeatedly or edit their checksums.
3. Start the matched backend, ML and edition-specific frontend. Keep existing
   routing gate and policy choices; no migration should silently enable Auto.
4. Check existing Default/Manual/Auto intent, readable history, current profiles,
   provider revisions and the configured checked default. Rejected profiles or
   permissions must produce errors, never an environment-key fallback.
5. Enable only the intended caller gates and policies explicitly. For snippet
   search, select **Web search: use snippets for Quick searches** in Admin and
   cap the agent's maximum web depth at Quick if OCR/rerank services are absent.
   Existing Standard/Deep agents retain their full fixed-service requirements.

Operational acceptance should use bounded public/synthetic inputs and the
deployment's effective actor/Enterprise permissions. Source ACLs, licences and
auditing need target-specific acceptance; a Core-only fixture does not establish
these properties for a real Enterprise installation.

## Disable automatic selection without discarding data

Save **Off** on the policy using its current revision. Keep a checked default
compatible with mandatory allowlists, locality, capabilities and budgets. New
Auto plans use that default, retaining hard restrictions. Manual remains explicit.
Profiles, saved selection/options and history remain intact.

Disable the independent caller gate if that caller must stop entirely. An adopted
conversation cannot silently return to legacy execution when the gate is off.
Policy edits do not substitute another model inside an already pinned tool turn;
stale work must stop or replan under its existing execution contract. Re-enable
the intended gate and Active policy explicitly after correcting the problem.

## Recover from a failed upgrade

Keep failed upgrade output and the backup identity. Stop the failed candidate's
writers and restore the verified backup into a separate database first. Confirm
the saved choices, history, profiles/policies and migration ledger against the
pre-upgrade record before switching the deployment to the restored installation.
Use the retained application artefacts appropriate for that backup's schema.

The operational Off switch does not require down-migrations. Running an older
binary directly against the new schema has not been certified. P12 profiles and
policies contain fields rejected by some older strict readers; follow
[quality downgrade guidance](model-routing-quality.md#verification-and-rollout).
Restoring a backup restores its snapshot; later writes require a separate recovery
decision. Do not delete the new health/history columns in a working database as a
shortcut to downgrade.

## Verified local rehearsal

The isolated release rehearsal used the actual SQLx runner, including checksums
and its migration ledger: 0137 -> 0138 -> repeated runner. Saved intent, profile
JSON and answer content were preserved and the browser gate stayed disabled.
Policy Off retained local-only constraints, refused the cloud manual target and
preserved history/profile/intent. The adopted chat could not fall back to legacy.
Explicit re-enable succeeded without an inference request.

Actual PostgreSQL `pg_dump`/`pg_restore` restored the 0137 snapshot; its captured
data matched and re-upgrade succeeded. This used an owned disposable database,
synthetic rows and no model/server/API calls. It does not certify an arbitrary
older binary, a production backup or a deployment-specific Enterprise licence.

The local candidate contains separate Core/Enterprise production UI bundles,
Windows backend builds for local acceptance, ML source and an unsigned x64 desktop
MSI. Backend build profile/platform are recorded in its manifest. It is not a
signed desktop update, Linux image, published release or production deployment.
Versions/tags are unchanged; a release cut must follow the normal review process.
