// Copyright 2026 Private AI Ltd (SC881079)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Doing the work, in the folder, on this machine.
//!
//! Requests arrive on the socket, already weighed by the instance: the folder is
//! bound to the conversation, the level of trust admits the action, the path
//! passed a check on its shape, and anything that changes a file was put in
//! front of the person and agreed to. None of that is taken on trust here. This
//! is the machine that owns the files, so it asks its own questions again — is
//! this a folder somebody at this keyboard connected, does that level of trust
//! admit this, and where does this path actually lead once links are followed —
//! and only then touches anything.
//!
//! The web view is not involved. It cannot reach this module: requests come from
//! the socket, which the window neither opens nor holds, and the answers go back
//! the same way.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow, bail};
use fosnie_protocol::{ClientFrame, ServerFrame};
use serde_json::{Value, json};
use tauri::{AppHandle, Manager};

use crate::folders::{self, Folder, Tier};
use crate::state::Shell;

/// How many entries a listing returns at most, and how far down it goes.
const MAX_ENTRIES: usize = 500;
const MAX_DEPTH: i64 = 4;
/// How large a file this reads before saying so instead. A model cannot use more
/// than this in one go, and reading a gigabyte to throw most of it away is a way
/// to make a machine unresponsive.
const MAX_READ_BYTES: u64 = 512 * 1024;
/// How much of a command's output is kept for the answer. What is streamed as it
/// happens is not capped by this; what comes back at the end is.
const MAX_OUTPUT_CHARS: usize = 200_000;

/// One command in flight: the process, and the turn that asked for it. The turn
/// is what the window knows a command by — it never sees the call id — so the
/// stop button finds a command through it.
struct InFlight {
    pid: u32,
    turn: String,
}

/// The commands still running, so one can be stopped from the window and all of
/// them can be stopped when the conversation they belong to has gone.
#[derive(Default)]
pub struct Running(Mutex<HashMap<String, InFlight>>);

impl Running {
    fn note(&self, call_id: &str, pid: u32, turn: &str) {
        self.0.lock().unwrap().insert(call_id.to_string(), InFlight { pid, turn: turn.to_string() });
    }

    fn done(&self, call_id: &str) {
        self.0.lock().unwrap().remove(call_id);
    }

    /// Every command running for one turn. The window stops a command by the turn
    /// it belongs to, so this is what the stop button reaches.
    pub fn pids_for_turn(&self, turn: &str) -> Vec<u32> {
        self.0.lock().unwrap().values().filter(|f| f.turn == turn).map(|f| f.pid).collect()
    }

    pub fn all(&self) -> Vec<u32> {
        self.0.lock().unwrap().values().map(|f| f.pid).collect()
    }

    pub fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

/// This run of the client. Backups are grouped by it, so a sweep can drop
/// everything from a run that finished a week ago in one go.
pub fn session_id() -> String {
    // The clock is enough: two runs of one client do not start in the same
    // millisecond, and nothing depends on this being unguessable.
    format!(
        "run-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default()
    )
}

/// Take a request off the socket, do it, and answer.
///
/// Errors are answers too: the instance turns a refusal into a tool error the
/// model can report, which is the difference between "I could not do that
/// because the file is outside your folder" and a conversation that stops.
pub async fn handle(app: AppHandle, call_id: uuid::Uuid, turn_id: uuid::Uuid, tool: String, args: Value) {
    let call = call_id.to_string();
    let outcome = run(&app, &call, &turn_id.to_string(), &tool, &args).await;
    let frame = match outcome {
        Ok(result) => ClientFrame::DesktopToolResult { call_id, ok: true, result },
        Err(e) => {
            tracing::info!(tool = %tool, error = %e, "refused or could not do a folder request");
            ClientFrame::DesktopToolResult {
                call_id,
                ok: false,
                result: json!({ "error": e.to_string() }),
            }
        }
    };
    if let Some(shell) = app.try_state::<Shell>() {
        shell.executor.done(&call);
        if !shell.bridge.send(frame.to_json()).await {
            tracing::warn!("the socket went before the answer could be sent");
        }
    }
}

/// Resolve the folder and the level of trust this request claims, and refuse if
/// this machine has no record of either.
fn folder_for(app: &AppHandle, args: &Value) -> Result<Folder> {
    let workspace_id = args
        .get("workspace_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("that request does not name a folder"))?;
    let base_url = app
        .try_state::<Shell>()
        .and_then(|s| s.paired_base_url())
        .ok_or_else(|| anyhow!("this client is not paired with an instance"))?;
    folders::resolve(app, &base_url, workspace_id).ok_or_else(|| {
        anyhow!("this computer has no record of a folder connected for that conversation")
    })
}

async fn run(app: &AppHandle, call_id: &str, turn: &str, tool: &str, args: &Value) -> Result<Value> {
    let folder = folder_for(app, args)?;
    if !folder.tier.allows(tool) {
        bail!(
            "{} is connected as {}, which does not allow that",
            folder.path,
            describe(folder.tier)
        );
    }
    let root = Path::new(&folder.path);
    let rel = args.get("path").and_then(|v| v.as_str()).unwrap_or("");

    match tool {
        "desktop.fs_list" => {
            let dir = folders::within(root, rel, true)?;
            let depth = args.get("depth").and_then(|v| v.as_i64()).unwrap_or(1).clamp(1, MAX_DEPTH);
            let mut entries = Vec::new();
            walk(&std::fs::canonicalize(root)?, &dir, depth, &mut entries)?;
            let truncated = entries.len() >= MAX_ENTRIES;
            Ok(json!({ "entries": entries, "truncated": truncated }))
        }

        "desktop.fs_read" => {
            let file = folders::within(root, rel, true)?;
            let meta = std::fs::metadata(&file).context("could not look at that file")?;
            if meta.is_dir() {
                bail!("that is a folder, not a file");
            }
            if meta.len() > MAX_READ_BYTES {
                return Ok(json!({
                    "content": format!(
                        "[too large to read in one go: {} bytes. Ask for part of it with a command, \
                         or work from a smaller file.]",
                        meta.len()
                    ),
                    "too_large": true,
                    "bytes": meta.len(),
                }));
            }
            let bytes = std::fs::read(&file).context("could not read that file")?;
            // Remember the file's state at read time, so a later write can tell if
            // it changed underneath the agent without asking the model to carry a
            // hash (which it cannot do reliably).
            app.state::<Shell>()
                .read_hashes
                .lock()
                .unwrap()
                .insert(file.to_string_lossy().into_owned(), sha256_hex(&bytes));
            match String::from_utf8(bytes) {
                Ok(content) => Ok(json!({ "content": content, "bytes": meta.len() })),
                // Not text: describe it rather than hand back mangled characters
                // the model would then reason about as if they meant something.
                Err(_) => Ok(json!({
                    "content": format!("[not a text file: {} bytes]", meta.len()),
                    "binary": true,
                    "bytes": meta.len(),
                })),
            }
        }

        "desktop.fs_write" => {
            let file = folders::within(root, rel, false)?;
            let key = file.to_string_lossy().into_owned();
            // Refuse only if the agent read this file and it has since changed on
            // disk — the same "changed since read" guard, but tracked by us rather
            // than a hash the model has to carry. A file the agent never read (a new
            // one it is creating) has no recorded hash and writes freely.
            if let Some(seen) = app.state::<Shell>().read_hashes.lock().unwrap().get(&key).cloned() {
                let actual = std::fs::read(&file).ok().map(|b| sha256_hex(&b));
                if actual.as_deref() != Some(seen.as_str()) {
                    bail!(
                        "{rel} has changed since it was read, so it was not overwritten. Read it \
                         again and decide what the contents should be."
                    );
                }
            }
            let content = args
                .get("new_content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("no contents to write"))?;
            // The copy is taken first, and a failure to take it stops the write:
            // a change that cannot be undone is not one to make quietly.
            let session = app.state::<Shell>().session.clone();
            let change = crate::backup::keep(app, &session, turn, &file, "write")?;
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent).context("could not create the folder for it")?;
            }
            std::fs::write(&file, content).context("could not write that file")?;
            // Record what we just wrote, so a second write this session compares
            // against the new contents rather than the pre-write read.
            app.state::<Shell>()
                .read_hashes
                .lock()
                .unwrap()
                .insert(key, sha256_hex(content.as_bytes()));
            Ok(json!({
                "written": folders::display(&file),
                "bytes": content.len(),
                "change_id": change.id,
                "created": !change.existed,
            }))
        }

        "desktop.fs_delete" => {
            let target = folders::within(root, rel, true)?;
            if target == std::fs::canonicalize(root)? {
                bail!("the connected folder itself cannot be deleted");
            }
            let session = app.state::<Shell>().session.clone();
            let change = crate::backup::keep(app, &session, turn, &target, "delete")?;
            if target.is_dir() {
                std::fs::remove_dir(&target)
                    .context("that folder is not empty, so it was left alone")?;
            } else {
                std::fs::remove_file(&target).context("could not delete that file")?;
            }
            Ok(json!({ "deleted": folders::display(&target), "change_id": change.id }))
        }

        "desktop.terminal_run" => {
            let command = args
                .get("command")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .ok_or_else(|| anyhow!("no command to run"))?;
            let secs = args.get("timeout_secs").and_then(|v| v.as_i64()).unwrap_or(60).clamp(1, 600);
            terminal(app, call_id, turn, root, command, secs as u64).await
        }

        other => bail!("this client does not know how to {other}"),
    }
}

fn describe(tier: Tier) -> &'static str {
    match tier {
        Tier::ReadOnly => "read only",
        Tier::ReadWrite => "read, write and delete",
        Tier::ReadWriteNoDelete => "read and write, but not delete",
    }
}

fn walk(root: &Path, dir: &Path, depth: i64, out: &mut Vec<Value>) -> Result<()> {
    if depth <= 0 || out.len() >= MAX_ENTRIES {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir).context("could not list that folder")?;
    for entry in entries.flatten() {
        if out.len() >= MAX_ENTRIES {
            break;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let path = entry.path();
        let shown = path.strip_prefix(root).unwrap_or(&path);
        out.push(json!({
            "path": shown.to_string_lossy(),
            "kind": if meta.is_dir() { "folder" } else { "file" },
            "bytes": meta.len(),
        }));
        if meta.is_dir() {
            walk(root, &path, depth - 1, out)?;
        }
    }
    Ok(())
}

/// Run a command in the folder, sending its output on as it appears.
async fn terminal(
    app: &AppHandle,
    call_id: &str,
    turn: &str,
    root: &Path,
    command: &str,
    secs: u64,
) -> Result<Value> {
    use tokio::io::AsyncReadExt;

    let mut cmd = shell_command(command);
    cmd.current_dir(root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Make the command its own process-group leader, so stopping it can take the
    // whole tree. A command line usually starts a shell that starts the real
    // program; without this the group `-pid` that `stop` signals does not exist,
    // and only the shell dies while the program it launched runs on. (Windows
    // does the same job with `taskkill /T` in `stop`.)
    #[cfg(unix)]
    cmd.process_group(0);
    strip_credentials(&mut cmd);

    let mut child = cmd.spawn().context("could not start that command")?;
    let shell = app.state::<Shell>();
    if let Some(pid) = child.id() {
        shell.executor.note(call_id, pid, turn);
    }
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");

    // Output is forwarded as it arrives, so a command that takes a minute is
    // visible while it takes it rather than arriving in one lump at the end.
    let streamer = {
        let app = app.clone();
        let call = call_id.to_string();
        tauri::async_runtime::spawn(async move {
            let mut collected = String::new();
            let mut buf = [0u8; 4096];
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                        if collected.chars().count() < MAX_OUTPUT_CHARS {
                            collected.push_str(&chunk);
                        }
                        if let (Some(shell), Ok(id)) =
                            (app.try_state::<Shell>(), uuid::Uuid::parse_str(&call))
                        {
                            let frame = ClientFrame::DesktopToolProgress { call_id: id, chunk };
                            if !shell.bridge.send(frame.to_json()).await {
                                break;
                            }
                        }
                    }
                }
            }
            collected
        })
    };

    let waited =
        tokio::time::timeout(std::time::Duration::from_secs(secs), child.wait()).await;
    let status = match waited {
        Ok(status) => status.context("that command ended badly")?,
        Err(_) => {
            let _ = child.kill().await;
            let _ = streamer.await;
            bail!("that command ran past the {secs} seconds it was given and was stopped");
        }
    };

    let out = streamer.await.unwrap_or_default();
    let mut err = String::new();
    let _ = stderr.read_to_string(&mut err).await;
    Ok(json!({
        "stdout": cap(&out),
        "stderr": cap(&err),
        "exit_code": status.code().unwrap_or(-1),
    }))
}

fn cap(text: &str) -> String {
    if text.chars().count() <= MAX_OUTPUT_CHARS {
        return text.to_string();
    }
    text.chars().take(MAX_OUTPUT_CHARS).collect::<String>() + "\n… (output cut here)"
}

/// A command line as the person would have typed it, run by the system's own
/// shell so that pipes and redirection behave as they look.
fn shell_command(command: &str) -> tokio::process::Command {
    #[cfg(windows)]
    {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(command);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(command);
        c
    }
}

/// The prefixes and names of environment variables that must not reach a command
/// this client starts. The client's own configuration and anything shaped like a
/// credential for the instance: a command run in a folder acts for the person,
/// but it has no business inheriting the means to act *as* the instance.
const WITHHELD_PREFIXES: [&str; 3] = ["FOSNIE_", "PAI__", "TAURI_"];
const WITHHELD_NAMES: [&str; 2] = ["DATABASE_URL", "REDIS_URL"];

/// True when a variable is one the child must not see.
///
/// The instance's own configuration, and anything shaped like a credential —
/// which is what a name carrying `SECRET`, `TOKEN` or `KEY` almost always is
/// (`AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, `GITHUB_TOKEN`, `GH_TOKEN`,
/// `NPM_TOKEN`, `HF_TOKEN`, `OPENAI_API_KEY`, …). A command run in a folder acts
/// for the person, but it has no business inheriting the keys to their other
/// accounts, and stripping a few harmless ones (a `*_KEYMAP`) is a price worth
/// paying to catch the ones that are not. Zero-egress by default means the wider
/// perimeter comes later (a full egress cage is a separate step); this is the
/// cheap half now.
pub fn withheld(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    WITHHELD_PREFIXES.iter().any(|p| upper.starts_with(p))
        || WITHHELD_NAMES.contains(&upper.as_str())
        || upper.contains("SECRET")
        || upper.contains("TOKEN")
        || upper.contains("_KEY")
        || upper.ends_with("KEY")
}

fn strip_credentials(cmd: &mut tokio::process::Command) {
    for (name, _) in std::env::vars() {
        if withheld(&name) {
            cmd.env_remove(name);
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Stop a command that is still running. Used by the button beside its output,
/// and when the socket goes: work asked for by a conversation that has ended
/// should not carry on.
pub fn stop(pid: u32) {
    #[cfg(windows)]
    {
        // `taskkill /T` takes the tree with it: a command line usually starts a
        // shell, and killing only the shell leaves whatever it started running.
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &format!("-{pid}")])
            .status()
            .or_else(|_| std::process::Command::new("kill").arg(pid.to_string()).status());
    }
}

/// Is this frame a request to do something in a folder? Parsed through the
/// shared frame types, so a change to the protocol is a change here too.
///
/// The turn is carried out as its own field on the frame, not inside `args` —
/// the window knows a running command by its turn, so a command that loses its
/// turn here is one the stop button can never find.
pub fn request(text: &str) -> Option<(uuid::Uuid, uuid::Uuid, String, Value)> {
    match serde_json::from_str::<ServerFrame>(text).ok()? {
        ServerFrame::DesktopToolCall { call_id, turn_id, tool, args } => {
            Some((call_id, turn_id, tool, args))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_recognised_and_nothing_else_is() {
        let call = ServerFrame::DesktopToolCall {
            call_id: uuid::Uuid::from_u128(1),
            turn_id: uuid::Uuid::from_u128(2),
            tool: "desktop.fs_read".into(),
            args: json!({ "path": "a.md" }),
        };
        let (id, turn, tool, args) = request(&call.to_json()).expect("a request");
        assert_eq!(id, uuid::Uuid::from_u128(1));
        // The turn is carried out of the frame's own field, not from args — that
        // is what the stop button matches a running command by.
        assert_eq!(turn, uuid::Uuid::from_u128(2));
        assert_eq!(tool, "desktop.fs_read");
        assert_eq!(args["path"], "a.md");

        let token = ServerFrame::ChatToken { turn_id: uuid::Uuid::nil(), delta: "hi".into() };
        assert!(request(&token.to_json()).is_none());
        assert!(request("not json").is_none());
    }

    #[test]
    fn the_clients_own_configuration_never_reaches_a_command() {
        assert!(withheld("FOSNIE_TOKEN"));
        assert!(withheld("fosnie_token"));
        assert!(withheld("PAI__DATABASE_URL"));
        assert!(withheld("TAURI_SIGNING_PRIVATE_KEY"));
        assert!(withheld("DATABASE_URL"));
        // Third-party credentials the client never had a reason to hold, but might
        // be in the environment it was launched from.
        assert!(withheld("OPENAI_API_KEY"));
        assert!(withheld("ANTHROPIC_API_KEY"));
        assert!(withheld("AWS_SECRET_ACCESS_KEY"));
        assert!(withheld("AWS_SESSION_TOKEN"));
        assert!(withheld("GITHUB_TOKEN"));
        assert!(withheld("GH_TOKEN"));
        assert!(withheld("NPM_TOKEN"));
        assert!(withheld("HF_TOKEN"));
        // Ordinary things a command needs are left alone.
        assert!(!withheld("PATH"));
        assert!(!withheld("HOME"));
        assert!(!withheld("USERPROFILE"));
        assert!(!withheld("LANG"));
    }

    #[test]
    fn output_is_capped_with_a_note_rather_than_cut_silently() {
        let short = "hello";
        assert_eq!(cap(short), short);
        let long: String = std::iter::repeat('x').take(MAX_OUTPUT_CHARS + 10).collect();
        let capped = cap(&long);
        assert!(capped.ends_with("(output cut here)"));
        assert!(capped.chars().count() < long.chars().count() + 40);
    }

    #[test]
    fn a_command_is_found_by_the_turn_the_window_knows_it_by() {
        let running = Running::default();
        running.note("call-1", 4242, "turn-a");
        running.note("call-2", 4343, "turn-a");
        running.note("call-3", 5555, "turn-b");
        let mut a = running.pids_for_turn("turn-a");
        a.sort();
        assert_eq!(a, vec![4242, 4343]);
        assert_eq!(running.pids_for_turn("turn-b"), vec![5555]);
        assert!(running.pids_for_turn("turn-c").is_empty());
        assert_eq!(running.all().len(), 3);
        running.done("call-1");
        assert_eq!(running.pids_for_turn("turn-a"), vec![4343]);
        running.clear();
        assert!(running.all().is_empty());
    }
}
