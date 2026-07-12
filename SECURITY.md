# Security Policy

## Reporting a vulnerability

**Please do not report security vulnerabilities through public GitHub issues, discussions, or pull requests.**

Report privately using one of:

- **GitHub Private Vulnerability Reporting** — the "Report a vulnerability" button under this repository's **Security** tab (preferred), or
- **Email** — security@fosnie.dev. If you wish to encrypt your report, request our PGP key in your first message.

Please include, as far as you can:

- a description of the issue and its potential impact;
- the affected component/version and configuration (e.g. backend, ML service, frontend; edition; deployment target);
- step-by-step reproduction (proof-of-concept if available);
- any suggested remediation.

## What to expect

- **Acknowledgement** within **3 business days**.
- An initial **assessment and severity triage** within **10 business days**.
- Regular updates on remediation progress.
- **Coordinated disclosure**: we will agree a disclosure timeline with you and credit you (if you wish) once a fix is available. Please give us a reasonable opportunity to remediate before any public disclosure — typically up to **90 days**, sooner for actively exploited issues.

## Scope

This policy covers Fosnie (this repository). Vulnerabilities in third-party dependencies should normally be reported upstream; if a dependency issue specifically affects Fosnie, let us know so we can mitigate.

## Safe harbour

We will not pursue or support legal action against researchers who, in good faith, discover and report vulnerabilities in accordance with this policy, who avoid privacy violations and service disruption, and who do not access or modify data beyond the minimum necessary to demonstrate the issue. Do not run tests against deployments you do not own or have explicit permission to test.

## Supported versions

Until a stable `1.0` release, security fixes are provided for the **latest released version** on the default branch. The supported-version table will be maintained here once formal release branches exist.
