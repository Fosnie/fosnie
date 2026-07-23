# Fosnie desktop client

An installable client for a Fosnie instance. It renders the platform's own web
application (built from `../frontend`, not a fork of it) and adds the things a
browser tab cannot: hold the connection reliably, tell you when something has
finished while you are looking elsewhere, and work in a folder on the machine it
is running on.

## What it can and cannot do

It is a governed window onto an instance, and the one thing it does that a
browser cannot — touch the machine's files — is fenced deliberately, so the fence
is worth stating plainly.

- **The window has no reach of its own.** It cannot read a file, run a program,
  or open a picker. `src-tauri/capabilities/main.json` grants it exactly one
  permission, listening for the client's own events; everything else goes through
  the named commands in `src-tauri/src/commands.rs`, and not one of those reads a
  file or runs a program.
- **Folder work comes from the instance, not the window.** A request to list,
  read, write, delete or run a command arrives on the socket, for a conversation
  the owner bound to a folder. The folder was chosen at this keyboard through the
  system picker and agreed a level of trust for (`src-tauri/src/folders.rs`);
  nothing in it is read before that agreement.
- **Every path is checked against the real filesystem.** The instance checks the
  path as written; the client resolves it, follows any links, and refuses
  anything that lands outside the folder — the check that can see where a link
  actually leads (`folders::within`, `src-tauri/src/executor.rs`).
- **Every change is shown first and can be undone.** A write is put in front of
  you as its difference, a command as the command, a deletion as what would go;
  each write and deletion is copied aside so it can be restored per file or per
  turn (`src-tauri/src/backup.rs`). What undo does not cover — files a command
  changed — is said the first time you use it.
- **A command inherits none of the client's credentials.** `FOSNIE_*`, `PAI__*`
  and the instance token are stripped from a spawned command's environment.

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
