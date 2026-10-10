# Evidence catalogue for machine semantic reviews

The versioned `semantic_catalog_judge.py` protocol lets a machine reviewer select
evidence IDs instead of reproducing quotations. It complements the existing blind
semantic review bundle and uses the same criteria and outcome rules. It changes
neither production routing nor P12 evidence admission.

The catalogue contains deterministic slices of the supplied question, saved answer
and reference excerpts. Each slice is at most 400 characters, remains an exact
substring of its original source, and carries its source and reference index.
Leading/trailing whitespace may be removed without rewriting internal text.
The bounded catalogue is shown alongside the complete original input. It is not
a summary, reference answer, new authority or set of independent facts.

The reviewer returns criterion ID, pass/fail/unknown, rationale and evidence IDs.
The native schema enumerates the allowed IDs. The tool refuses invented, duplicate
or excessive IDs and missing criteria, then expands the chosen IDs to exact source
anchors. The original strict semantic assessment validator still checks the
normalised result. A non-unknown outcome still requires at least one anchor.
Evidence references do not establish the truth or usefulness of the reasoning.
Model prompt injection resistance and reviewer impartiality are not guaranteed.

The frozen catalogue protocol binds its own source, the earlier V37 protocol,
shared comparison implementation and semantic-review module. Native bodies and
selected cases are bound before dispatch. Existing V37 code and receipts remain
unchanged; a catalogue proposal cannot be interpreted as a V37 proposal or vice
versa. The shared comparison retains the actual catalogue plan and observation
hashes, rather than substituting a synthetic legacy request binding.

The bounded pilot's shared dispatcher accepts a versioned protocol while retaining
the fixed reviewer models, destinations, caps, credential handling and no-retry
behaviour. It records the native terminal status, parsed reviewer JSON before
assessment validation, normalised anchors for valid assessments, and a fixed safe
failure category for contract refusal. Oversized or credential-bearing grader
content is omitted and cannot become a valid assessment. Provider error bodies and
authentication headers are not captured. Credential pattern checks are not a
general classifier of sensitive data; evaluation inputs must be public and
explicitly authorised before dispatch.

The actual V37 experiment had 14 attempts, all HTTP 200: five valid OpenAI reviews
and nine contract-unknown replies. Its original guard did not retain those invalid
JSON assessments or exact failure categories, so their causes cannot be inferred
or repaired after the fact. All consensus outcomes are unknown. Keep this failed
comparison and its consumed slots; do not rerun it or claim that quote errors alone
caused those historical failures.

V38 uses four other public creative/editing requests and the original frozen V36
machine-authored criteria. It does not repeat V37 cases or regenerate executor
answers. The fresh allocation allows at most 32 reviewer attempts, separately
accounted from V37's 14. Its full synthetic protocol and the affected tests cover
catalogue conversion, exact bindings, disagreement, failure trace capture and
credential suppression. A higher valid-JSON rate on this different cohort would
not prove a causal improvement in semantic accuracy.

The user explicitly authorised public test questions, saved model responses,
criteria and exact fragments to OpenAI Responses and Gemini GenerateContent for
V38 and future bounded routing evaluations. This covers those public payload types
and destinations; it excludes private/user data, Core/Enterprise documents and
secrets. Each experiment still needs a fresh fixed plan, bounded allocation and
no implicit retry or production activation. Preserve the recorded scope grant
alongside per-allocation hashes. Do not ask again for the same authorised scope.

Both individual machine assessments, all nine agreement cells, conservative
consensus, unavailable cases, calibration/test separation and the full original
denominator remain visible. Results are always diagnostic, with
`quality_evidence=false`, `p12_eligible=false` and no automatic policy update.
Independent source, workload, family and rubric review plus a separately frozen
live quality comparison remain prerequisites for admission.
