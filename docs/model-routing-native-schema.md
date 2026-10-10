# Native structured output in routing

Routing judges can opt into a native JSON contract with
`classifier.native_schema: true`, or the **Require native JSON schema from judge**
setting. It defaults to false for existing policies. The saved judge remains the
same local or cloud provider; this setting does not select a different model.

Enable it after establishing that the actual endpoint and model support native
structured output. Fosnie translates the contract for OpenAI Chat Completions
(including compatible Ollama endpoints), OpenAI Responses, and Gemini
`generateContent`. Anthropic, streaming and calls with tools are not supported by
this initial contract path. Unsupported requests fail before inference; a
provider rejection never triggers another judge invocation without the schema.
The orchestrator can still use its compatible checked default after a failed
judge, subject to the existing constraints and shared run budget.

The fixed judge contract contains task, minimum tier, freshness, tool, vision and
structured-output requirements, confidence and uncertainty. A judge classifies
requirements rather than choosing a provider. Reading supplied text or existing
tool results does not itself require a new tool call. A missing live weather
observation still requires freshness and an external tool. Existing requirements
can only become stricter after classification.

The manager-owned `/api/routing/evaluate` endpoint also accepts an optional
`response_schema` alongside its existing required selection, saved policy
revision, messages and execution caps. This is a bounded non-streaming text
evaluation, not a change to the public chat API. For example:

```json
{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "servings": {"type": "integer", "minimum": 1},
    "flour_g": {"type": "integer", "minimum": 0}
  },
  "required": ["servings", "flour_g"]
}
```

The portable v1 subset is a flat object with 1–64 required scalar fields. Types
are string, boolean, number and integer; supported constraints are enum (up to
32 entries), numeric minimum/maximum and short descriptions. Every field must
be required and `additionalProperties` must be false. The encoded schema is
limited to 8 KiB. References, nested objects, arrays, nullable/optional fields,
unions and unrecognised keywords are rejected rather than silently removed.

Schema bytes and a translation allowance count towards input reservations in
Core and ML. Long judge context is shortened only after reserving those bytes,
and still marks the decision uncertain. The schema cannot expand context,
output, cost, locality, RBAC or Enterprise restrictions. The same Core path is
used by both editions without an Enterprise implementation or licence gate.

Fosnie requires a completed text answer and validates the returned object:
missing, additional or duplicate fields, wrong types, invalid bounds, Markdown
fences, refusals and incomplete output fail the contract. Output is not repaired
or resent. A structural contract does not establish semantic correctness or
model quality; answer validators and reviewed evaluation evidence remain
separate.

Policies written with the new field require a compatible Core reader. When
preparing an older-binary rollback, remove the field from policy JSON using the
normal reviewed rollback process; older strict deserialisers may reject it even
when it is false. No database migration is introduced by this option.

Provider contracts: [OpenAI structured outputs](https://developers.openai.com/api/docs/guides/structured-outputs),
[Gemini generation configuration](https://ai.google.dev/api/generate-content#ResponseFormatConfig),
[Ollama OpenAI compatibility](https://docs.ollama.com/api/openai-compatibility).
