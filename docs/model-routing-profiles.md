# Routing profiles

Model routing is available in Core. Open **Settings → Model routing** for your
personal profiles. You can use visible shared models without an administrator
role or a personal API key. Administrators manage shared connections and models;
personal connections follow the deployment's existing BYOK setting. Personal variants of shared models do not require BYOK.

## Set up models

1. Add a connection, or use a shared connection configured by its owner.
2. Configure a model's exact API ID and capabilities in Models. The capability
   editor is under **Model capabilities, helper services and legacy policies**.
   Set its role to economy (routine), balanced (standard) or advanced (complex).
   These are your preferences; they do not depend on the model's brand or price.
3. Create one or more variants with the effort and output settings you want.
   Variants share credentials. Unsupported effort/settings are rejected.
4. Create a profile from Everyday, Thorough, Custom Auto or Full Auto.

## Profiles and rules

**Custom Auto** uses selected variants. **Full Auto** uses every visible, enabled,
compatible variant, including new variants added later. It does not purchase
models or discover credentials. Both modes use the same planner and dispatcher.

Everyday matches declared model roles to classified task requirements. Thorough
prefers higher configured roles. The lower-price preference uses configured
tariffs when known; no savings percentage is claimed or required.

Choose a default variant for uncertain or failed classification. Choose a task
classifier with structured JSON support. It makes at most one bounded call and
returns task type, complexity and required capabilities, never a provider ID.
The whole conversation is supplied as untrusted data within a bounded envelope.
Rules-only profiles use requirements supplied by the caller, without a classifier;
complexity rules need classification.

Each rule can match tasks, complexity (trivial/routine/standard/complex), language,
workflow purpose, tools, vision, structured output, freshness and input limits.
Nested ALL/ANY conditions are supported. Actions can assign an exact variant,
prefer a variant, restrict the pool or exclude variants. Higher-priority exact
assignments win; contradictory assignments at equal priority fail explicitly.
Every matching restriction intersects; a restriction never widens the pool.

A routine recommendation can be assigned to your strongest model. A complex task
can be assigned to a smaller model. Inferred complexity does not veto an explicit
assignment. Ownership, capabilities, locality, current revisions, availability
and run budgets still apply. A fresh-information request needs permitted tools
and sources; choosing a model alone does not grant web access.

## Fallback, persistence and sharing

Set a default personal profile or choose a profile in the chat/workflow selector.
Chat and project bindings retain the selected profile. Manual selection pins a
model and optional variant. Default restores ordinary inheritance.

An optional technical fallback permits one alternate attempt before any output
at an eligible text boundary. Exact assignments stay exact. Tools, external
actions and partly delivered answers are not automatically replayed. Classifier,
executor and fallback share the original budget and cancellation.

Clone shared profiles into personal profiles. Export contains no credentials;
import maps model/variant slots to permitted local entries before saving. Revision
conflicts preserve the editor draft. Changed profiles affect the next safe run.

Routing does not require model profiling, AI judges, benchmark collection,
calibration artefacts or an economic certificate. Older optional evaluation fields
are read-compatible metadata and are cleared when a profile is saved.
