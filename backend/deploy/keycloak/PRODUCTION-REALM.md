# Production Keycloak realm — hardening checklist

`import/fosnie-realm.json` is a **local development seed**: it ships test users
(`alice`/`bob`/`carol`), `sslRequired: "none"`, `http://localhost` redirect URIs and
permissive client settings so the stack runs on a laptop. **Do not import it into a
production Keycloak.** Provision the production realm from your IdP's hardened baseline
and apply the deltas below. (The platform code is realm-agnostic; these are Keycloak
configuration, not application changes.)

Each item maps to a finding from the platform-access security review.

## Realm

- [ ] **Remove all test users** (`alice`, `bob`, `carol`). Provision real users via your
      directory / federation. *(Critical — seed creds are public in this repo.)*
- [ ] **`sslRequired: "all"`** (or at least `"external"`). *(Forces HTTPS at Keycloak.)*
- [ ] **Brute-force protection ON**, e.g.:
      ```json
      "bruteForceProtected": true,
      "failureFactor": 5,
      "waitIncrementSeconds": 60,
      "maxFailureWaitSeconds": 900,
      "minimumQuickLoginWaitSeconds": 60,
      "quickLoginCheckMilliSeconds": 1000,
      "maxDeltaTimeSeconds": 43200
      ```
      *(Closes password brute-force / user enumeration on the login endpoint.)*
- [ ] **Explicit, short token lifespans**, e.g.:
      ```json
      "accessTokenLifespan": 600,
      "refreshTokenLifespan": 3600,
      "ssoSessionIdleTimeout": 1800,
      "ssoSessionMaxLifespan": 7200
      ```
      *(Bounds the value of a stolen token; makes logout effective without server-side
      revocation.)*
- [ ] **Registration disabled** (`registrationAllowed: false` — already the seed default).
- [ ] **Consider enforced MFA** (required action / conditional OTP) for a regulated
      deployment. *(Defence-in-depth on the credential.)* In Keycloak mode the second
      factor is Keycloak's job — add **OTP** (or WebAuthn) as a realm *Required action*
      (Realm settings → Authentication → Required actions → "Configure OTP"), or gate
      it with a conditional-OTP flow. The application's own 2FA (`auth.require_mfa`,
      the `/api/auth/mfa/*` endpoints) is **local-auth only** and returns `409` here.

## Client `fosnie-spa` (public, browser PKCE)

- [ ] `redirectUris` → `https://<your-domain>/*` only (drop `http://localhost`).
- [ ] `webOrigins` → `https://<your-domain>` only.
- [ ] `post.logout.redirect.uris` → production URL.
- [ ] PKCE `S256` enabled (already the seed default — keep it).

## Client `fosnie` (confidential, backend audience)

- [ ] `redirectUris` / `post.logout.redirect.uris` → production `https://…`.
- [ ] **`webOrigins`** → restrict to your domain (the seed uses `*`).
- [ ] **`directAccessGrantsEnabled: false`** unless a non-interactive API integration
      genuinely needs the resource-owner-password grant. *(Removes a weaker auth path.)*
- [ ] Rotate the **client secret** (the seed value `fosnie-secret` is public);
      deliver it to the backend via `PAI__KEYCLOAK__CLIENT_SECRET` (env), never committed.

## Identity brokering (Enterprise SSO — SAML 2.0 / OIDC)

The Enterprise edition brokers a customer's IdP (Okta / Entra ID / Ping / ADFS /
Google) *through* this realm: the customer IdP is registered as a Keycloak
**Identity Provider**, the platform stays a plain OIDC/PKCE + Bearer client, and no
SAML is implemented in the platform itself. The SSO onboarding API
(`POST /api/admin/sso/idp`) creates the broker IdP with the hardened defaults below;
this checklist is the manual/air-gapped equivalent and the review list.

**Service account (already in the seed).** `fosnie` has
`serviceAccountsEnabled: true` and its service account holds the `realm-management`
client roles **`manage-identity-providers`** and **`view-realm`** — the minimum the
onboarding API needs to create/list/delete broker IdPs. It deliberately does **not**
hold `manage-users`: provisioning writes to the platform database (SCIM), never into
Keycloak. In production, keep this scope minimal and rotate the client secret (above).

**Per-IdP hardening (SAML broker):**

- [ ] **`wantAssertionsSigned: true`** and **`validateSignature: true`** — reject
      unsigned/forged assertions. Import the IdP's signing certificate (from its
      metadata) so signatures are actually checked.
- [ ] **`wantAuthnRequestsSigned: true`** — sign our AuthnRequests.
- [ ] **NameID policy `persistent`** (fallback `emailAddress` — document which the IdP
      emits; the platform links identities by *verified email* regardless).
- [ ] **`allowedClockSkew`** small — default **60s**, cap **300s**. *(Replay window.)*
- [ ] **Signed logout is a per-IdP toggle.** Entra ID does **not** sign its
      `LogoutResponse` → enabling logout-signature validation yields *"Invalid signature
      in response from IdP"*. Turn logout-signature validation **off for Entra**, leave
      it on where the IdP signs.
- [ ] **Attribute/claim mappers:** `email`, `givenName`/`familyName` → `displayName`,
      and the IdP's group attribute/claim → the **`groups`** user-attribute (surfaced to
      `fosnie-spa` by the `brokered-groups-to-groups-claim` mapper in the seed). Set mapper
      **sync mode `FORCE`** so attributes refresh on every login.
- [ ] **SP metadata:** hand the customer IdP admin our SP metadata (entityId, ACS URL,
      SP certificate) from `GET /api/admin/sso/sp-metadata`.
- [ ] **Certificate rotation:** if the IdP publishes a metadata URL, the platform's
      rotation job re-reads it and warns when a certificate is < 30 days from expiry;
      otherwise re-import the metadata manually before expiry.

**Air-gapped `kcadm.sh` equivalents** (no onboarding API; run against the realm):

```sh
# authenticate as the service account (or an admin)
kcadm.sh config credentials --server https://<kc>/ --realm fosnie \
  --client fosnie --secret "$PAI__KEYCLOAK__CLIENT_SECRET"

# create a SAML broker IdP from the customer's metadata
kcadm.sh create identity-provider/instances -r fosnie \
  -s alias=<customer> -s providerId=saml -s enabled=true \
  -s 'config.wantAssertionsSigned=true' -s 'config.validateSignature=true' \
  -s 'config.wantAuthnRequestsSigned=true' -s 'config.nameIDPolicyFormat=persistent' \
  -s 'config.allowedClockSkew=60' -s 'config.signingCertificate=<base64-cert>'

# list / delete
kcadm.sh get  identity-provider/instances -r fosnie
kcadm.sh delete identity-provider/instances/<customer> -r fosnie
```

## Cross-checks (platform side)

- [ ] `PAI__SERVER__PUBLIC_URL` and `PAI__KEYCLOAK__URL` are `https://…` — the backend
      **refuses to boot otherwise** on a non-loopback host.
- [ ] TLS terminates at the reverse proxy; the proxy sets `X-Forwarded-For`; the backend
      is not directly exposed.

> The default-role gotcha (seed users lacked `default-roles-fosnie`, breaking the Keycloak
> account console) is fixed in the seed; ensure real users receive the realm default
> role so `Manage password & MFA` works.
