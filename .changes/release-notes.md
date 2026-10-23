# Fosnie v0.7.1

This patch update extends the model routing included in 0.7.0 with configurable
personal profiles and model variants.

- Create Custom Auto profiles with a selected pool or Full Auto profiles that
  include available variants. Start from the editable Everyday and Thorough presets.
- Route by task type and complexity, assign an exact model and effort, set
  preferences, restrict pools and choose a default for uncertain classification.
- Use personal profiles and variants of shared models in **Settings → Model routing**,
  including ordinary users with personal API keys disabled.
- Retain access controls, capability and locality checks, shared usage limits,
  chat/project bindings, credential-free import/export and bounded technical fallback.
- Correct production/test container build inputs for the simplified routing implementation.

Routing follows configured model roles and user assignments. No benchmark or
calibration certificate is required to use Auto, and this release makes no
measured savings claim.

Use matching backend, ML and frontend versions. Database migrations run through
the normal startup process; back up an existing installation before upgrading.
The independently versioned desktop client is unchanged.

Validation used local providers: routine and complex tasks, both inverse user
assignments, ordinary-user/BYOK-off settings, desktop/mobile layouts and installed
Enterprise restrictions passed. No paid model calls were required.

[Routing profiles guide](https://github.com/Fosnie/fosnie/blob/v0.7.1/docs/model-routing-profiles.md)
