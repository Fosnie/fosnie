# Fosnie desktop client

An installable client for a Fosnie instance. It renders the platform's own web
application (built from `../frontend`, not a fork of it) and adds the two things
a browser tab cannot do well: hold the connection reliably, and tell you when
something has finished while you are looking elsewhere.

## What it can and cannot do

It is a governed window onto an instance. It has **no local capabilities**: no
filesystem access, no command execution, no local tools. The plugins that would
provide any of that are not dependencies of the crate, so this is a property of
what is compiled in rather than of configuration. `src-tauri/capabilities/main.json`
grants the window exactly one permission, listening for the client's own events;
everything else goes through the handful of commands in `src-tauri/src/commands.rs`.

Two other decisions worth knowing before reading the code:

- **The socket lives in the client, not the web view.** Web views drop
  long-lived connections: the Windows one does it silently, the macOS one after
  about a minute of idling. Either would cut an answer off mid-stream. So the
  connection, its reconnect and its resume live in Rust, and the web view
  receives frames as events. `src-tauri/src/ws.rs` and
  `../frontend/src/ws/transport-shell.ts` are the two ends of that.
- **The window has its own content security policy.** In a browser the
  application is protected by the policy the instance sends with its pages; in
  the client the bundle is local, so no instance header reaches it and the policy
  is set in `src-tauri/tauri.conf.json` instead. It matches the instance's on
  everything that stops rendered model output from executing: `script-src 'self'`
  (no inline scripts), `object-src 'none'`, `base-uri 'self'`,
  `frame-ancestors 'none'`. Two directives are deliberately wider than the
  instance's, and neither can be avoided: `connect-src` has to include
  `ipc: http://ipc.localhost`, without which nothing in the window can call this
  client at all, and it cannot name the instance, because which instance this is
  is decided at pairing time and a compiled-in policy cannot know it. Plain
  `http:`/`ws:` are allowed alongside the TLS schemes because an instance reached
  over a private network without TLS is a real deployment, not a mistake.
- **The device token is only ever in the operating system's credential store.**
  It is handed to the web view once per start, into memory, and written nowhere
  else. Pairing is done with a short code minted from a signed-in web session;
  this client never asks for a password.

## Running it

Requires the Rust toolchain, Node, and (on Windows) the WebView2 runtime, which
Windows 11 already has.

```sh
npm install                # in this directory, for the Tauri CLI
npm run dev                # builds ../frontend and opens the client
```

Pair it against a running instance: enter the address, then a code from
**Profile → Connected devices → Pair a device** in a browser signed in to it.

## Building an installer

```sh
npm run build              # MSI on Windows, .app/.dmg on macOS
```

Releases are signed. Both keys, how a release is signed, and the three-release
key rotation are in [docs/updater-keys.md](docs/updater-keys.md) — read it before
publishing anything, and note in particular that the Windows upgrade code in
`src-tauri/tauri.conf.json` is fixed permanently and must never be regenerated.

## Versioning

The client has its own version, independent of the platform's. It is installed
once and updates on its own schedule, so the two numbers routinely differ; both
are shown together under Profile → Connected devices when the application is
running inside the client.
