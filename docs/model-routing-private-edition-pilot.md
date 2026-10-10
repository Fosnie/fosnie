# Private edition routing fixture

The `routing_pilot` CLI loads `support/routing_pilot_shared.rs` with Core boot
defaults and the Core migration set. An external edition example may include
that shared module and install its real RBAC/features/audit implementations
through `PilotEdition`. It supplies its own bootstrap/source fixtures/private
routes and the same Core migrations. Production Core never imports Enterprise.

The fixture keeps loopback provider/ML guards, the dedicated routing_test
database on15439, its own routing_pilot_UUID schema and cleanup, a separate Redis
port and opt-in browser/index metadata. A consumer may choose an ordinary user
and create delegated management permissions before saving profiles/policies.
This tests edition seams against the same production Core router and planner.

Migration macros stay in the entry-point crate so Cargo resolves paths correctly.
The shared module contains no embedded consumer-specific migration path. Private
hooks/routes are absent from the deployed HTTP router. Existing consumed pilot
allocations/ledgers are not rerun after this refactor.

The Enterprise source consumer documents its exact scope in the sibling
docs/model-routing-source-pilot.md. Its public corpus, synthetic materialised
ACLs and fixture licence are operational tests, not production data or admitted
P12 quality evidence.
