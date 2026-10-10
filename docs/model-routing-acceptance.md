# First-shipment routing acceptance — 9 October 2026

The documented first-shipment implementation is complete. Core and the actual
Enterprise consumer passed the consolidated acceptance below. This establishes
execution contracts for the supported scope; independent P12 quality admission,
deployment-specific model accuracy and financial savings remain open.

## Result

| Check | Scope | Result |
| --- | --- | --- |
| protocol | Selection persistence and protocol compatibility | pass |
| unit | Rules, eligibility, efforts, budgets and P12 admission invariants | pass |
| acceptance-inference | Cloud-only/local/mixed routing, classifier failures and bounded technical fallback | pass |
| management-probe | One-call standalone judge test, management access, source/revision revocation and audit receipts | pass |
| acceptance-ml | Exact target, lease, effort, output schema and native adapter contracts | pass |
| acceptance-frontend | Chat intent and routing settings serialisation | pass |
| acceptance-types | Frontend TypeScript compatibility | pass |
| browser | Product chat and routing settings against fixture APIs | pass |
| stack | Actual product browser -> Core -> ML -> loopback provider | pass |
| acceptance-ee | Actual Enterprise restrictions, source ACL, licence/audit integration and effort capabilities | pass |

One initial integration check had an obsolete expectation: after the earlier answer
budget protection, a one-call Auto evaluation correctly skips its judge and uses the
checked default. The assertion now checks the actual answer, zero classifier spend
and explicit skip reason. Inspection also found a real related regression: explicit
standalone judge tests reused the planner's two-call/time reserve and therefore could
not run under their single-call allowance. A shared internal bounded-dispatch path now
reserves answer capacity only for ordinary planning. No restrictions, current target
checks, accounting or retry limits were relaxed.

The management fixture exercises a one-call policy with a 30-second answer reserve,
Off rollout, actual judge success/failure, zero answer calls, stale revisions, access
revocation and durable start receipts. Normal Auto keeps its original reserve behavior.
The initial failure is retained in the V43 workspace receipt. Only affected library
and inference checks were repeated; the untouched remainder then continued.

Four old ML stage fixtures also predated the existing storage-root, authorised
rebuild-corpus and bounded summary-grant requirements. Their setup now supplies
those prerequisites and a mock storage client. The dual-write fixture checks both
successful replacement of the pinned collections and refusal before any mutation
when the active alias moves after inference. The existing ML runtime guards were
preserved. Only the affected ML check was repeated after this fixture correction.

One settings browser case used the old 'Browser rollout gates' heading. Its selector
now follows the existing 'Routing rollout gates' heading. Only that failed case was
repeated; the initial ten unaffected browser passes and original failure are retained.
Frontend runtime code was unchanged.

## Using the completed feature

Follow [local/cloud/mixed onboarding](model-routing-release.md): configure providers,
current profiles, the policy and an optional local/cloud judge, inspect the dry-run
preview, and explicitly enable the desired caller for a controlled setup. Default,
Manual and Auto are user selections; Rules, Classifier and Hybrid are Auto strategies.
Cloud-only setup requires no local model. Routing remains an open Core feature, and
Enterprise intersects access/locality/budget restrictions through its existing seams.

All production settings and caller gates were left unchanged in this acceptance.
No deployment, real notification, production migration or global Active rollout was
performed. No cloud model API call, real local model server or installer rebuild was
needed. Previous model-evaluation artifacts and both dirty working trees were preserved;
the only runtime source change is the standalone classifier probe fix.

## Repeat the bounded acceptance when relevant code changes

With the existing runtimes and isolated routing PostgreSQL/Redis:

```powershell
$env:FOSNIE_ROUTING_TEST_DATABASE_URL = 'postgres://routing_test:routing_test_only@127.0.0.1:15439/routing_test'
./tools/routing/test.ps1 -Lane acceptance -WithEnterprise -Output tools/routing/reports/acceptance
```

On POSIX, use `bash tools/routing/test.sh acceptance --with-enterprise` with the same
isolated database variable. Without `-WithEnterprise`/`--with-enterprise`, this lane
is Core-only; that does not verify Enterprise. Missing required services or checkouts
fail. Use `-Only`/`--only` for a failed affected check and `-StartAt`/`--start-at` for
the untouched remainder. The broader [release matrix](model-routing-release-matrix.md)
and existing native release checks remain relevant to changes in those components;
this acceptance does not replace them or imply they were all rebuilt today.

## Quality research is a separate workstream

The optional P12 admission gates still require comparable independently reviewed
families, current model/profile fingerprints and the configured quality floor. Machine
review agreement or declared capabilities cannot promote that evidence automatically.
V42's source-review tools are available for that work, but human source adjudication
and further live benchmarks are not required to finish the base routing feature.
Do not automatically continue another benchmark round after this acceptance.

Remaining deployment work requires the intended environment, authorised activation
and configured model profiles. Remaining quality claims require an explicitly scoped
evaluation; unsupported callers and downgrade/billing limits remain in the onboarding.
