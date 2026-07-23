# Desktop release keys and signing

Everything in this file is an operator runbook, not something the build does for
you. Two separate secrets are involved and they protect different things:

- **The update key** proves that an update the client downloads came from us.
  Without it a client will not install anything.
- **The Windows code-signing certificate** proves to the operating system that
  the installer came from us. Without it, Defender and SmartScreen treat the
  application as unknown software, which for an application that talks to a
  company's own systems is not a warning users should be trained to click past.

## The update key

### Generating it

Run this once, on a machine you control, offline:

```sh
npm --prefix desktop exec -- tauri signer generate -w fosnie-desktop-updater.key
```

It writes two files: the private key and `fosnie-desktop-updater.key.pub`.

- The **public** key goes into `desktop/src-tauri/tauri.conf.json` under
  `plugins.updater.pubkey`, replacing the placeholder. It is public by
  definition: every installed client carries it.
- The **private** key never leaves the same custody as the licence signing key,
  under the same handling. It is not committed, not put in a shared drive, and
  not pasted into a chat.

You will be asked for a password. Use one, and store it with the key.

### The password and the environment: do not

The updater's tooling accepts the key password from an environment variable. It
is known not to work reliably: builds pick up an empty password, produce an
artefact signed with something nobody can reproduce, and the failure only shows
up when a client refuses the update. Enter the password interactively when
signing, or sign on a machine where you can.

If a release is ever signed with the wrong password, do not try to patch around
it. Rebuild and re-sign.

### Signing a release

`tauri build` produces the installer and, because `createUpdaterArtifacts` is on,
a signed update artefact plus a `.sig` file beside it. Signing happens during
that build, so the private key has to be available to it:

```sh
export TAURI_SIGNING_PRIVATE_KEY="$(cat /path/to/fosnie-desktop-updater.key)"
npm --prefix desktop run build     # enter the key password when asked
```

Without the key that command fails: it writes the installer and then exits
non-zero, because it was asked for an update artefact it cannot sign. A machine
that has no key builds with `npm --prefix desktop run build:unsigned`, which
produces the same client without the update artefact. That is what CI does when
the key secret is absent, and what to use for a local test build.

Then publish, at `https://get.fosnie.dev/desktop/latest.json`:

```json
{
  "version": "0.1.1",
  "notes": "What changed, in a sentence.",
  "pub_date": "2026-07-22T12:00:00Z",
  "platforms": {
    "windows-x86_64": {
      "signature": "<contents of the .sig file>",
      "url": "https://get.fosnie.dev/desktop/Fosnie_0.1.1_x64_en-US.msi.zip"
    }
  }
}
```

Clients check that manifest at startup and once a day, download quietly, and ask
before installing.

### Rotating the key

A client only trusts the public key it was compiled with, so a new key cannot
sign an update for clients that do not yet know it. Rotation therefore takes
three releases and cannot be rushed:

1. **Release A** — still signed with the OLD key. Its only change is that the
   configuration carries the NEW public key. Every client that updates to A is
   now able to verify the new key.
2. **Wait.** Until you are satisfied that installations have taken release A.
   Anything still on an older version will be stranded by step 3 and will have to
   be reinstalled by hand.
3. **Release B** — signed with the NEW key. Clients on A accept it; clients that
   skipped A do not.

Keep the old private key until step 3 has shipped and settled. Do not delete it
the day you generate the new one.

### If the private key is lost or exposed

There is no revocation. A lost key means no further updates can be issued to
existing installations, and they must be replaced with a fresh install. An
exposed key means someone else can sign an update your clients will install,
which is a security incident: rotate immediately by the sequence above, and
accept that installations which do not take release A have to be reinstalled.

## Windows code signing (Azure Trusted Signing)

Unsigned installers are not published. An unsigned application that connects to
company systems and runs in the background is precisely the shape endpoint
protection is built to stop, and it will be stopped.

Account setup, which is an administrative task rather than a build one:

1. In the Azure portal, create a **Trusted Signing** account (about $10 a month)
   in a supported region.
2. Create a **certificate profile** of type Public Trust. Identity validation
   takes a few days: the business details must match Companies House exactly
   (Private AI Ltd, SC881079).
3. Create a service principal (app registration) for CI and grant it the
   **Trusted Signing Certificate Profile Signer** role on the account.
4. Put these into the repository's Actions secrets:
   - `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`
   - `AZURE_SIGNING_ENDPOINT`, `AZURE_SIGNING_ACCOUNT`, `AZURE_SIGNING_PROFILE`
5. The release workflow has a signing step that switches itself on when
   `AZURE_SIGNING_ACCOUNT` is present, and until then builds an unsigned artefact
   and says so in its job summary. That step has never run against real
   credentials, so **the first signed release must be verified by hand** before
   anything is published: check the file's digital signature tab names Private AI
   Ltd, and run the installer on a clean machine to see what SmartScreen does.

New certificates have no reputation with SmartScreen, so the first few signed
releases may still show a warning until enough installations accumulate. That is
expected and resolves itself; it is not a reason to go back to unsigned builds.

## The Windows upgrade code

`bundle.windows.wix.upgradeCode` in `desktop/src-tauri/tauri.conf.json` is fixed
forever. Windows uses it to recognise that a new installer replaces the existing
installation rather than sitting beside it, and management tooling keys off it.
Changing it produces duplicate entries in Add/Remove Programs on every machine
and there is no clean way back. It is not a value to regenerate when copying the
configuration around.
