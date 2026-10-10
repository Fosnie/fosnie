# Supplied-passage QA and explicit routing rules

The versioned `qa_span_answers.py`, `qa_span_corpus.py` and `qa_span_pilot.py`
evaluate agreement with published answer spans for supplied-passage questions.
They reuse the existing Core request construction and target/profile/policy/
effort/ledger attestation. They do not replace production routing or import P12
learning evidence.

The public [SQuAD dataset](https://rajpurkar.github.io/SQuAD-explorer/) contains
human-written questions and annotated spans in Wikipedia passages. The source
here is the revision-pinned SQuAD 1.1 **train** split. It is distributed under
CC BY-SA 4.0; source URL, revision, byte hash, article names and attribution are
retained. It is a new Fosnie document sample, not evidence that these examples
were absent from model training. The old cached dev split had only 13 unused
article groups, so the new source supplies twenty other document groups.

One question per article is selected by fixed hashes before model outputs. All
annotation offsets must exactly match their original context substring, and all
normalised references must be nonempty. Existing case IDs/article groups are
excluded. Corpus load reconstructs the selection from the copied source file,
exposure registry and pinned manifests, and checks question, context, annotation,
group and evaluator bindings. It does not certify semantic uniqueness of an
annotated answer or statistical independence of articles.

The metric is normalised **published answer-span exact match**, not F1, an official
leaderboard submission or a universal semantic-quality score. Frozen normalisation
lowercases, removes ASCII punctuation and English articles, and collapses spaces.
Word order is retained. Only a single terminal final span is assessed; an earlier
reference echo in a rationale is insufficient. Bare values and limited presentation
wrappers are recognised, with final-marker compliance reported separately.
Truncated/unattested outputs, multiple/nonterminal markers and empty/unscorable
values stay unknown. A recognised value that misses all published references fails
this lexical metric, even when a paraphrase or another valid span might answer
the question. Do not interpret every EM failure as proven factual error.

Read-only `qa_span_diagnostics.py` supplies context-substring/token-sequence and
longer/shorter-gold hints. It also flags indefinite questions that may have more
than one supported answer. These hints do not adjudicate entailment, accept new
references, trim predictions, rewrite outcomes or establish new semantic truth.
For example, a longer phrase containing a gold noun or a second listed network
can differ from a single published annotation. Keep the original score and expose
this limitation for a separately preregistered source/rubric review.

Five fixed strategies are compared: manual OpenAI, manual Gemini, Rules,
Classifier and Hybrid. All see the same passage and question. The caller supplies
the known extraction workflow intent, not a gold answer or independent difficulty
label. Rules and Hybrid have a declared extraction-to-Gemini selection rule;
hard provider/access/locality/capability/context/effort/budget filters still apply.
An eligible terminal Hybrid rule skips the classifier. Classifier remains a
separate requirement-assessment path. Gemini/high is retained as the previous
calibration baseline; current test outcomes do not select a new baseline.

The private pilot allocates at most 100 Core requests and 140 model calls for
twenty cases. A fully matching rule ordinarily yields 120 model calls because
only Classifier needs twenty judge calls. Core/native ledgers verify actual
judge counts; do not infer bypass solely from the strategy name. Responses from
the same model are separate stochastic generations, so score differences between
Rules, Hybrid and manual Gemini do not prove a quality benefit from a rule.
Lower judge-call count does not prove invoice savings.

Model/effort/confidence/timeout/reserve and policy snapshots are frozen before
generation. No automatic retry, spent-slot replay, baseline regeneration, test
fitting or production activation. Inputs are public evaluation questions and
passage fragments under the recorded continuing OpenAI/Gemini payload scope,
excluding user/private data, Core/Enterprise documents and secrets. Credentials
stay in memory. Only owned temporary Core/ML/database fixtures are launched.

Twenty different article groups are still **candidate** families. Source/prompt/
rubric consistency, semantic grouping and minimum-tier labels are unreviewed.
Actual source retrieval, web search, multilingual support and Enterprise ACL
behaviour are not exercised by supplied text. Public train exposure and alternate
valid answers remain material limits. Outputs keep `quality_evidence=false`,
`p12_eligible=false` and `policy_updated=false`; preserve the existing independent
family/profile/purpose/Wilson admission gates.
