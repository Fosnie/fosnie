# Preserve the answer budget before invoking a judge

Auto now checks that the shared run can spare two calls before dispatching a
classifier. If only one remains, it selects the independently checked default
without a judge reservation or model call. The decision explains this with
`classifier_skipped_answer_call_budget`. Failed/unknown previous reservations still
count; the check never refunds or relaxes a limit. Atomic reservation remains the
final authority and all deployment/personal/Enterprise constraints still intersect.

The optional classifier field `answer_reserve_ms` leaves captured time for the
executor. It accepts50..30000ms; absent/null keeps existing timeout settings.
Routing settings expose **Reserve answer time (ms)**, which can be cleared.
Core clips the judge's configured timeout to remaining shared time minus this
reserve, rechecks after resolution/audit and tightens both the ML wire grant and
Core timeout. It never extends the run deadline. No viable judge window yields
`classifier_skipped_answer_time_budget` and the checked default, without inference.

The checked default retains capability, effort, policy, locality, access and
budget filters. A skipped classifier cannot resolve an unknown task against a
default's task exclusions: Core refuses instead of silently bypassing suitability.
Known excluded tasks also refuse. Cancellation still wins over default selection.
Missing default or hard incompatibility remains an error, not permission to pick
a weaker/unauthorised model. The standalone classify API keeps its old return type;
only the planner can interpret the new internal skipped outcome.

The time reserve is a minimum planning allowance, not a latency/answer-completion
guarantee. Later tighter restrictions or time spent outside the model request can
still stop a run. A started audit/reservation remains conservatively charged if
dispatch subsequently cannot fit; it does not become a free retry. This feature
does not increase generated tokens or fix semantic selection errors.

V35 actual Core/ML endpoints with synthetic native replies confirm:

- max_calls1: one checked-default primary call, judge0 and explicit call-budget skip.
-1000ms request with5000ms reserve: one primary call, judge0 and time-budget skip.
-30000ms request/20000ms reserve: judge grant≤10000ms, then one primary; same ledger.

72 Core routing tests pass, one ignored; Core/Enterprise targets and frontend types
pass, six routing settings tests pass. An additional exclusion test found a known
task gap, which was repaired and verified before the final protocol. A first
synthetic preflight used an older example binary while rebuild was still pending;
its failure is retained with zero real calls. No API/live comparison was repeated.

Offline V34 diagnostics separate primary transport/generation-cap/contract losses
from descriptive manual counterfactuals. Several non-pass Auto answers had passing
manual results from the same selected model. This is not proof of semantic regret,
judge accuracy or minimum-tier truth: generations and timing differ. Original
scores remain unchanged. P12/verified evidence and production policy are untouched.

Read workspace report.v35.md/summary, diagnostic JSON, final synthetic receipt and
cleanup. Own schemas/apps/services were cleared; no model server was started.
