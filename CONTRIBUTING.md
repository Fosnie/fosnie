# Contributing to Fosnie

Thanks for your interest in contributing to **Fosnie**. This document explains how to propose changes and the legal sign-off we require on every contribution.

> Fosnie is the open-source (Apache-2.0) edition. Fosnie **Enterprise** features (advanced compliance/audit tooling, moderation, federated SSO/SCIM, white-label, and similar organisation-scale capabilities) live in a separate, proprietary repository and are **out of scope** for contributions here. PRs that re-implement Enterprise features inside Core may be declined — please open a discussion first if you're unsure where something belongs.

## Ways to contribute

- **Bugs / issues:** open a GitHub issue with clear reproduction steps, expected vs actual behaviour, and your environment.
- **Features / changes:** for anything non-trivial, open an issue or discussion **before** writing code so we can agree on scope and design.
- **Documentation:** corrections and clarifications are very welcome.

## Developer Certificate of Origin (DCO) — required

We use the [Developer Certificate of Origin](./DCO) (DCO 1.1) instead of a CLA. **Every commit must be signed off.** By signing off you certify that you wrote the change (or have the right to submit it) under the project's licence — see the full text in [`DCO`](./DCO).

Add the sign-off automatically with `-s`:

```bash
git commit -s -m "Your commit message"
```

This appends a trailer to your commit message using the real name and email in your Git config:

```
Signed-off-by: Jane Doe <jane@example.com>
```

Use your **real name** (no pseudonyms or anonymous contributions). Set it once with:

```bash
git config user.name "Jane Doe"
git config user.email "jane@example.com"
```

Forgot to sign off? Amend the last commit with `git commit --amend -s --no-edit`, or sign off a whole branch with `git rebase --signoff main`. A DCO check runs on every pull request and must pass before merge.

## Licensing of your contributions (inbound = outbound)

Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in Fosnie is licensed under the **Apache License, Version 2.0** (the same licence as the project), per Section 5 of that licence. You retain copyright to your contribution; you grant the project the copyright and patent licences described in Apache-2.0. No separate copyright assignment is required.

## Pull request guidelines

- Keep PRs focused and reasonably small; one logical change per PR.
- Include tests for new behaviour and bug fixes. All checks must be green:
  - Rust: `cargo test` and `cargo check --all-targets` (uses `SQLX_OFFLINE=true`; run `cargo sqlx prepare` if you change SQL).
  - Frontend: `npm run build` / `tsc --noEmit`.
  - ML service: the Python test suite under `ml/`.
- Match the existing code style; do not reformat unrelated code.
- Write a clear PR description: what changed, why, and how it was verified.
- Do not commit secrets, credentials, or customer data.

## Reporting security issues

**Do not** open public issues for security vulnerabilities. Follow the private disclosure process in [`SECURITY.md`](./SECURITY.md).

## Code of Conduct

Participation in this project is governed by our [Code of Conduct](./CODE_OF_CONDUCT.md). By participating, you agree to uphold it.
