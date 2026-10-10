# Objective final-value routing comparison

`objective_answers.py`, `objective_corpus.py` and `objective_pilot.py` compare saved
final values from an actual Core routing run against formally recomputed public
targets. They complement instruction compliance and blind machine review; neither
model opinions nor fluency determine the correct target.

The corpus uses the previously pinned public
[BIG-Bench-Hard source](https://github.com/suzgunmirac/BIG-Bench-Hard/tree/9ee07bd481feebf959a6b59d61ea57bdcf30964d).
Five bounded task grammars are supported: Boolean expressions, integer arithmetic,
ASCII word sorting, unmatched-bracket completion and deterministic navigation.
The existing `reference_audit.solve` computes each result. A source instance is
eligible only when its computed result matches the published target, its grammar
is supported and its prior case ID/original prompt is excluded. No source labels
are corrected and no model outcomes are used to choose cases.

Twenty instances, four per task, are selected by fixed hashes. Question text is
retained and a final-value directive is explicitly appended to every strategy's
prompt. Corpus, answer bank, source files, source revision, exposure registry,
formal solver and evaluator hashes are frozen before inference. Loading rebuilds
the corpus from the copied pinned source files and rejects changed questions,
targets, proof metadata, hashes or evaluator code. Source byte binding is not a
cryptographic attestation of source authorship or family independence.

The metric is **final-value accuracy**. Optional reasoning is permitted, with a
requested terminal `Final answer: <value>` line. A single, unambiguous terminal
value can also be recognised without that marker, with format compliance reported
separately. Decimal integers and legal thousands separators, Boolean case,
word separators and bracket whitespace have frozen canonicalisation rules.
Only limited outer presentation wrappers are removed. Model-supplied code or
arithmetic expressions are never executed. A match elsewhere in a rationale is
never sufficient for a pass.

Duplicate/nonterminal final markers, unsupported value forms, missing values and
transport/truncated/unattested output remain unknown. A recognised final value
that differs from the verified target fails. `SyntaxError`, an answer with opening
brackets where a completion is expected, or another unsupported typed value is
unscorable under this version; it is not silently converted into a factual fail or
pass. A correct final value does not establish correct reasoning, useful prose or
absence of other contradictory statements. These limits are part of the frozen
metric, not a universal semantic-quality rubric.

The adapter reuses `audited_run` request construction and Core target/profile/
policy/effort/ledger attestation. It sends no gold targets or unreviewed task/tier
labels as routing hints. Manual OpenAI/Gemini and Classifier/Hybrid see identical
original questions and directive. The Gemini/high baseline is retained from the
previous calibration choice, never selected on this cohort. Its saved answer is
reused for paired comparisons. The fixed classifier timeout and answer-time reserve
are declared before dispatch. No profile, confidence threshold or ranking fitting
on current outcomes and no evidence import or production activation.

Reports preserve all pass/fail/unknown cells, missing cases in the denominator,
calibration/test separation, marker compliance and unknown reasons. The original
Core verdict, final value and answer hash are recorded together. Availability,
generation caps, formatting and model selection can affect observed outcomes;
manual and Auto responses are separate stochastic generations. Descriptive
counterfactuals cannot prove causal model-selection regret or actual savings.

This is a new Fosnie instance sample from already exposed public task groups;
training exposure is unknown. Five task groups do not meet P12's twenty independent
families per comparable workload/profile/purpose. Complexity, workload labels and
family independence remain unreviewed. No live web, retrieval, images, code
execution, multilingual coverage or Enterprise source permissions are evaluated
by this corpus. Outputs keep `quality_evidence=false`, `p12_eligible=false` and
`policy_updated=false` even when final values match all targets.

The native pilot uses fresh bounded allocations, fixed cloud destinations/models,
no retries and hidden in-memory credentials under the user's standing public-eval
payload authorisation. It launches only its own temporary Core/ML/database fixtures
and removes them afterward. It never loads or modifies the main local model server,
regenerates a baseline for pairing, reopens consumed allocations or rewrites old
scores after seeing their outcomes. Use a new version and cohort for a changed
evaluator or protocol; preserve all failed and unknown records.
