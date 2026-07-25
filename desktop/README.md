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

`release/publish-desktop.sh` assembles what is published from a finished build:
the installer under its versioned name, the same file under a stable one, and the
update manifest. The release workflow runs that same script, so a release
uploaded by hand and one uploaded by CI are the same bytes arranged the same way.

## Building it yourself

Anyone can build this client from source, and an organisation that will only run
software it compiled itself is meant to:

```sh
npm --prefix ../frontend install
npm install
npm run build:unsigned
```

The installer appears under `src-tauri/target/release/bundle/msi/`. Two honest
caveats, in full at
[fosnie.dev/docs/desktop-build](https://fosnie.dev/docs/desktop-build): the
result is unsigned, so Windows and endpoint protection will warn about it on
every machine that installs it; and updates verify against the public key
compiled into the build, so a self-built client updates from your own signed
manifest or not at all.

## Where updates come from

The client asks the instance it is paired with before it asks anything else:
`<instance>/api/desktop/latest.json`, with its device token, and only then the
published channel. An instance serving no installer answers with a plain "nothing
here" and the check carries on, so this costs an ordinary installation nothing
and lets a closed network keep its whole estate current from its own server.

`plugins.updater.dangerousInsecureTransportProtocol` in `tauri.conf.json` is on
for that reason, and it is a smaller thing than the name suggests: an instance is
frequently reached over plain HTTP inside a private network, and without this the
client would refuse to ask it and quietly fall back to the internet — the exact
outcome the arrangement exists to avoid. What it relaxes is the transport of the
manifest. The installer itself is verified against the signing key compiled into
the build, over any transport, and an update that does not verify is not
installed.

## The icon

`app-icon.png` is the source: the brand mark on the brand's near-black, composed
once at 1024 so every size below it is a reduction of the same picture. The set
in `src-tauri/icons/` is generated from it:

```sh
npx tauri icon app-icon.png
```

Two things are then done by hand, and both matter. The generator produces mobile
icon sets this client has no use for (`src-tauri/icons/android`, `.../ios`);
delete them. And at 16 to 32 pixels — the taskbar, the tray, Alt-Tab — the mark
reduced from a padded square turns to mush, so those entries are rendered from a
tighter crop of the same source (about 92% of the square rather than 76%) and
put into `icon.ico` alongside the larger ones, with the tight 32 also written
over `32x32.png`, which is what the tray and Linux window use:

```sh
magick app-icon.png -resize 48x48 large48.png     # and 64, 256, from this source
magick <brand>/logo-simple.png -trim +repage -resize 940x940 \
  -background "#0c0a11" -alpha remove -gravity center -extent 1024x1024 tight.png
magick tight.png -resize 16x16 small16.png        # and 24, 32, from the tight one
magick small16.png small24.png small32.png large48.png large64.png large256.png \
  src-tauri/icons/icon.ico
```

## The window frame

On Windows the client asks for no system decorations and the application draws
the title bar itself, which is why `src-tauri/tauri.windows.conf.json` exists.
Tauri merges a platform configuration into the main one as a JSON merge patch,
and a merge patch **replaces an array outright** rather than merging its members
— so that file repeats the whole window definition, and any change to the window
in `tauri.conf.json` has to be made in both. macOS and Linux take the main
configuration unchanged and keep their own decorations.

The trade-off of an application-drawn frame on Windows 11 is that hovering the
maximise control no longer offers the Snap Layouts flyout. Keyboard snapping
(`Win` + arrows) is unaffected.

## Versioning

The client has its own version, independent of the platform's. It is installed
once and updates on its own schedule, so the two numbers routinely differ; both
are shown together under Profile → Connected devices when the application is
running inside the client.
