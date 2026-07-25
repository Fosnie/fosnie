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

//! Working in a folder on the machine the user is sitting at.
//!
//! The instance has no filesystem of the user's to reach, and that is the point:
//! the folder is on their computer, the client that runs there does the work, and
//! this module is the half that decides whether the work may be asked for at all.
//! A request leaves here only after the folder is known, the conversation is
//! bound to it, the level of trust admits the action, and the path stays inside
//! the folder as written.
//!
//! **Two boundary checks, deliberately.** The one here is on the path as a
//! string, against the folder as it was registered: it catches the shapes that
//! are wrong on their face — a climb out with `..`, an absolute path somewhere
//! else, a drive letter, a stream suffix — before anything is sent anywhere. It
//! cannot catch a link that points outside, because resolving a link needs the
//! filesystem the link is on. The client repeats the check against the real
//! thing, after resolving it, and refuses there too. Neither check is the whole
//! answer; the one on the machine that owns the files is the last word.

use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::{AppError, Result};

/// How much the owner agreed to when they connected the folder. Each level only
/// ever narrows what may happen inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Read, and nothing else.
    ReadOnly,
    /// Read, write and delete.
    ReadWrite,
    /// Read and write, but never delete: the level for a folder whose contents
    /// matter more than the convenience of tidying up.
    ReadWriteNoDelete,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::ReadOnly => "ro",
            Tier::ReadWrite => "rw",
            Tier::ReadWriteNoDelete => "rw_nd",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "ro" => Some(Tier::ReadOnly),
            "rw" => Some(Tier::ReadWrite),
            "rw_nd" => Some(Tier::ReadWriteNoDelete),
            _ => None,
        }
    }

    /// A sentence for the prompt and for an approval card. Written for the person
    /// deciding, not for the log.
    pub fn describe(self) -> &'static str {
        match self {
            Tier::ReadOnly => "read only",
            Tier::ReadWrite => "read, write and delete",
            Tier::ReadWriteNoDelete => "read and write, but not delete",
        }
    }
}

/// What a paired machine sent back about one call.
#[derive(Debug, Clone)]
pub struct DesktopReply {
    pub ok: bool,
    pub result: Value,
}

/// A folder a machine was told it may work in.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub id: Uuid,
    pub device_id: Uuid,
    pub path: String,
    pub label: String,
    pub tier: Tier,
}

/// Where the call to the machine goes out, and how its reply is waited on.
///
/// For an ordinary interactive turn the machine is at the other end of the very
/// socket the turn arrived on, so the outgoing frame rides the turn's own sender
/// and the socket closing is a live signal that nobody will answer. A scheduled
/// job has no such socket: its turn runs on the server with a throwaway sender
/// that never closes, so the call is addressed to the machine's live socket
/// through the hub instead, and its presence is polled rather than watched.
#[derive(Debug, Clone, Copy)]
pub enum DesktopSink {
    /// The turn's own sender is the machine's socket (interactive desktop turn).
    TurnSocket,
    /// The turn runs elsewhere (a scheduled job); address the machine's live
    /// socket by owner and device through the hub.
    DeviceRoute { user_id: Uuid, device_id: Uuid },
}

/// What one turn knows about the folder it is working in. Built once, when the
/// turn starts, from the conversation's binding and the connection's device —
/// so a turn that arrived any other way simply has none of this, and the tools
/// are never offered.
/// A command the folder's owner has agreed to run without being asked again,
/// matched by how a command starts, together with whether that agreement also
/// lets it reach the network. A plain agreement covers only commands that run
/// without the network; agreeing "with network" is the wider grant.
#[derive(Debug, Clone)]
pub struct CommandPrefix {
    pub prefix: String,
    pub with_network: bool,
}

#[derive(Debug, Clone)]
pub struct DesktopToolCtx {
    pub workspace: Workspace,
    /// The commands this folder's owner has already agreed to, matched by the
    /// start of the command line, each with whether the agreement extends to the
    /// network.
    pub command_prefixes: Vec<CommandPrefix>,
    /// How strong a boundary the machine keeps for a command it runs, as the
    /// machine last reported (`"full"` or `"lifecycle"`). Carried so the decision
    /// about what to ask the person can weigh it; a machine that keeps only the
    /// process-lifetime line is still asked about every command.
    pub sandbox_tier: String,
    /// How this turn reaches the machine. Interactive turns leave it at the
    /// default; a scheduled job sets a device route.
    pub route: DesktopSink,
}

/// The sandbox tier a device declared in its opening handshake, folded to the
/// value stored against the device. Anything but an explicit full claim reads as
/// the weaker tier, so a client that is old, or silent, or a browser that has no
/// business claiming anything, is treated as still needing to be asked.
pub fn sandbox_tier_from_capabilities(capabilities: &[String]) -> &'static str {
    if capabilities.iter().any(|c| c == "sandbox:full") {
        "full"
    } else {
        "lifecycle"
    }
}

/// Record against a device how strong a boundary it keeps for a command, from
/// what it said on connecting. Called only for a genuine device connection; the
/// stored value folds to the weaker tier unless the machine claimed the full one.
pub async fn record_sandbox_tier(pg: &sqlx::PgPool, device_id: Uuid, capabilities: &[String]) {
    let tier = sandbox_tier_from_capabilities(capabilities);
    let _ = sqlx::query!("UPDATE devices SET sandbox_tier = $1 WHERE id = $2", tier, device_id)
        .execute(pg)
        .await;
}

impl DesktopToolCtx {
    /// The prefix that already covers this command, if one does. `None` means the
    /// user has to be asked. When the command needs the network, only an agreement
    /// that was made with the network covers it.
    pub fn allowed_prefix(&self, command: &str, need_network: bool) -> Option<&str> {
        matching_prefix(command, &self.command_prefixes, need_network)
    }

    /// The same context, addressing the machine over the hub rather than the
    /// turn's own socket. Used when the turn is not itself on the machine's
    /// connection (a scheduled job, or a resumed approval).
    pub fn with_route(mut self, route: DesktopSink) -> Self {
        self.route = route;
        self
    }
}

/// The folder this turn may work in, or nothing.
///
/// Nothing is the ordinary answer, and it is the whole of the refusal to run a
/// turn from a browser against somebody's disk: a turn that did not arrive from
/// a paired machine has no device, a conversation that was never bound to a
/// folder has no binding, and a folder that has been withdrawn stops matching.
/// In all three cases the tools are never offered, so the model is never in the
/// position of explaining a capability it does not have.
///
/// The folder must belong to the very machine the turn came in on. A second
/// machine's folder is not reachable from this one's socket, and quietly using
/// the binding anyway would be sending one computer's files to another.
pub async fn load_ctx(
    pg: &sqlx::PgPool,
    chat_id: Uuid,
    device_id: Option<Uuid>,
) -> Option<DesktopToolCtx> {
    let device_id = device_id?;
    let row = sqlx::query!(
        "SELECT w.id, w.device_id, w.path, w.label, w.tier, d.sandbox_tier \
         FROM chat_workspace cw \
         JOIN device_workspaces w ON w.id = cw.workspace_id \
         JOIN devices d ON d.id = w.device_id \
         WHERE cw.chat_id = $1 AND w.device_id = $2 \
           AND w.revoked_at IS NULL AND d.revoked_at IS NULL",
        chat_id,
        device_id,
    )
    .fetch_optional(pg)
    .await
    .ok()
    .flatten()?;

    let tier = Tier::parse(&row.tier)?;
    let prefixes = sqlx::query!(
        "SELECT prefix, with_network FROM workspace_command_prefixes WHERE workspace_id = $1",
        row.id,
    )
    .fetch_all(pg)
    .await
    .map(|rows| {
        rows.into_iter()
            .map(|r| CommandPrefix { prefix: r.prefix, with_network: r.with_network })
            .collect()
    })
    .unwrap_or_default();

    // Touched here rather than on every call: one write per turn that actually
    // has a folder, which is what "last used" means to the person reading it.
    let _ = sqlx::query!("UPDATE device_workspaces SET last_used_at = now() WHERE id = $1", row.id)
        .execute(pg)
        .await;

    Some(DesktopToolCtx {
        workspace: Workspace {
            id: row.id,
            device_id: row.device_id,
            path: row.path,
            label: row.label,
            tier,
        },
        command_prefixes: prefixes,
        sandbox_tier: row.sandbox_tier,
        // The default is the interactive turn's own socket; a scheduled job
        // overrides it with `with_route` once it knows the target machine.
        route: DesktopSink::TurnSocket,
    })
}

/// The five things a client may be asked to do. Named here rather than matched
/// as strings all over, so adding a sixth is one place and one compiler error at
/// every site that has to decide about it.
pub const FS_LIST: &str = "desktop.fs_list";
pub const FS_READ: &str = "desktop.fs_read";
pub const FS_WRITE: &str = "desktop.fs_write";
pub const FS_DELETE: &str = "desktop.fs_delete";
pub const TERMINAL_RUN: &str = "desktop.terminal_run";

pub const ALL: &[&str] = &[FS_LIST, FS_READ, FS_WRITE, FS_DELETE, TERMINAL_RUN];

/// Is this one of the local-execution tools?
pub fn is_desktop_tool(name: &str) -> bool {
    ALL.contains(&name)
}

/// Whether a folder call must be asked about before it runs. The single place the
/// rule lives, so the live classifier and its test can never drift apart.
///
/// A read-only call is never asked about. A write is asked about every time,
/// unless the automation running it pre-approved writes to this folder. Deletion
/// is asked about every time, with no exception at all: no pre-approval covers it,
/// because running it again more carefully does not undo it. A command is asked
/// about unless it begins with a prefix the owner has already agreed to.
///
/// `terminal_prefix_ok` is the caller's answer to "does an agreed prefix already
/// cover this command, at the network level it needs", and `pre_approved_writes`
/// is only ever true on an unattended automation run whose owner opted in.
///
/// A command carries a further relief on a machine that keeps a real boundary: if
/// the machine confines it (`sandbox_full`) and it does not ask for the network
/// (`net_requested` is false), it can run without a card at all, because there is
/// nothing left for the person to weigh — it cannot leave the folder or reach the
/// network. A command that does want the network is still asked about, so the
/// person sees the one thing the boundary is being opened for. An administrator
/// can switch that relief off instance-wide with `always_prompt`, which puts the
/// card back on everything.
pub fn desktop_call_gated(
    tool: &str,
    is_authorised: bool,
    effect: crate::tools::ToolEffect,
    pre_approved_writes: bool,
    terminal_prefix_ok: bool,
    sandbox_full: bool,
    net_requested: bool,
    always_prompt: bool,
) -> bool {
    // A name that was never offered this turn is not a request to weigh up; it goes
    // down the ordinary path, where the authorisation seam refuses it.
    if !is_desktop_tool(tool) || !is_authorised {
        return false;
    }
    // Reading changes nothing.
    if matches!(effect, crate::tools::ToolEffect::ReadOnly) {
        return false;
    }
    // A pre-approved write, and only a write, skips the card. Deletion never does.
    if pre_approved_writes && tool == FS_WRITE {
        return false;
    }
    if tool == TERMINAL_RUN {
        // The paranoid-administrator setting: ask about every command, whatever
        // else is true.
        if always_prompt {
            return true;
        }
        // A command the owner already agreed to by how it starts, at the network
        // level it needs, is not re-asked.
        if terminal_prefix_ok {
            return false;
        }
        // The boundary's relief: a confined command that does not want the network
        // has nothing left to weigh, so it runs without a card.
        if sandbox_full && !net_requested {
            return false;
        }
        return true;
    }
    true
}

/// The longest a registered folder's path may be. Generous for a real path and
/// far short of anything that is one.
const MAX_PATH: usize = 4096;

/// Fold a folder's path into the form it is stored and compared in.
///
/// Rejects what cannot be a folder someone means to work in: a relative path (it
/// would have no fixed meaning), a climb out, a network share (it is somebody
/// else's filesystem, reached over a wire this instance cannot see), and
/// anything carrying a control character. Trailing separators go, so that the
/// same folder written two ways is the same folder.
pub fn normalise_root(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::Validation("a folder path is required".into()));
    }
    if trimmed.len() > MAX_PATH {
        return Err(AppError::Validation("that folder path is too long".into()));
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err(AppError::Validation("that folder path is not a path".into()));
    }
    let windows_style = looks_like_windows_root(trimmed);
    if trimmed.starts_with("\\\\") || trimmed.starts_with("//") {
        return Err(AppError::Validation(
            "a network share cannot be connected as a folder".into(),
        ));
    }
    if !windows_style && !trimmed.starts_with('/') {
        return Err(AppError::Validation("the folder path must be absolute".into()));
    }
    let sep = if windows_style { '\\' } else { '/' };
    let mut parts: Vec<&str> = Vec::new();
    for part in trimmed.split(['/', '\\']) {
        match part {
            "" | "." => continue,
            ".." => {
                return Err(AppError::Validation(
                    "the folder path must not contain '..'".into(),
                ))
            }
            other => parts.push(other),
        }
    }
    if windows_style {
        // The drive letter came through as the first part ("C:"); everything
        // after it is a folder name.
        let (drive, rest) = parts.split_first().expect("a windows path has a drive");
        let mut out = format!("{}{sep}", drive.to_uppercase());
        out.push_str(&rest.join(&sep.to_string()));
        Ok(out.trim_end_matches(sep).to_string() + if rest.is_empty() { "\\" } else { "" })
    } else {
        Ok(format!("{sep}{}", parts.join(&sep.to_string())))
    }
}

fn looks_like_windows_root(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Join a path the model asked for onto the folder, refusing everything that
/// would not stay inside it.
///
/// The result is for display, for the audit trail and for the client to resolve
/// properly; it is not evidence that the file exists or that the real path stays
/// inside once links are followed. The client answers that question.
pub fn resolve_within(root: &str, relative: &str) -> Result<String> {
    let rel = relative.trim();
    if rel.is_empty() || rel == "." {
        return Ok(root.to_string());
    }
    if rel.len() > MAX_PATH {
        return Err(AppError::Validation("that path is too long".into()));
    }
    if rel.chars().any(|c| c.is_control()) {
        return Err(AppError::Validation("that path is not a path".into()));
    }
    if rel.starts_with('/') || rel.starts_with('\\') || looks_like_windows_root(rel) {
        return Err(AppError::Forbidden(outside(root)));
    }
    // A colon anywhere else on a Windows-shaped path is a drive-relative path or
    // an alternate stream, both of which address something other than the file
    // they appear to.
    if rel.contains(':') {
        return Err(AppError::Forbidden(outside(root)));
    }
    let windows_style = looks_like_windows_root(root);
    let sep = if windows_style { '\\' } else { '/' };
    let mut parts: Vec<&str> = Vec::new();
    for part in rel.split(['/', '\\']) {
        match part {
            "" | "." => continue,
            ".." => return Err(AppError::Forbidden(outside(root))),
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return Ok(root.to_string());
    }
    let base = root.trim_end_matches(['/', '\\']);
    Ok(format!("{base}{sep}{}", parts.join(&sep.to_string())))
}

fn outside(root: &str) -> String {
    format!("that path is outside the connected folder ({root})")
}

/// Does this level of trust admit this tool? The refusal is worded for the model,
/// which has to be able to tell the user why it stopped.
pub fn tier_allows(tier: Tier, tool: &str) -> Result<()> {
    let refused = match (tier, tool) {
        (Tier::ReadOnly, FS_WRITE | FS_DELETE | TERMINAL_RUN) => {
            Some("the connected folder is read only")
        }
        (Tier::ReadWriteNoDelete, FS_DELETE) => {
            Some("the connected folder does not allow deleting files")
        }
        _ => None,
    };
    match refused {
        Some(why) => Err(AppError::Forbidden(why.into())),
        None => Ok(()),
    }
}

/// Characters that make one command line into several. A command carrying any of
/// them is never covered by an agreement about how it starts, because what it
/// starts with stops predicting what it does.
const CHAINING: [&str; 9] = ["&", "|", ";", "\n", "\r", ">", "<", "`", "$("];

/// The agreed prefix covering this command, if any.
///
/// Matching is on whole words: agreeing to `npm test` does not agree to
/// `npm testify`. A command that chains, redirects or substitutes is never
/// covered however it begins, so an agreement to run one thing cannot be spent
/// on running another after it.
pub fn matching_prefix<'a>(
    command: &str,
    prefixes: &'a [CommandPrefix],
    need_network: bool,
) -> Option<&'a str> {
    let cmd = command.trim();
    if cmd.is_empty() || CHAINING.iter().any(|c| cmd.contains(c)) {
        return None;
    }
    prefixes
        .iter()
        .find(|cp| {
            let p = cp.prefix.trim();
            // A command that needs the network is only covered by an agreement made
            // with the network; a command that does not is covered by either.
            (!need_network || cp.with_network)
                && !p.is_empty()
                && cmd.starts_with(p)
                && cmd[p.len()..].chars().next().map(|c| c.is_whitespace()).unwrap_or(true)
        })
        .map(|cp| cp.prefix.as_str())
}

/// Fold a prefix into the form it is stored and matched in. A prefix that could
/// chain is refused at the point it is agreed, not silently ignored later.
pub fn normalise_prefix(raw: &str) -> Result<String> {
    let prefix = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if prefix.is_empty() {
        return Err(AppError::Validation("a command prefix is required".into()));
    }
    if prefix.len() > 200 {
        return Err(AppError::Validation("that command prefix is too long".into()));
    }
    if CHAINING.iter().any(|c| prefix.contains(c)) {
        return Err(AppError::Validation(
            "a command prefix cannot contain shell operators".into(),
        ));
    }
    Ok(prefix)
}

/// What the user is being asked to agree to, in a shape a client can render.
/// The sentence beside it says the same thing for a client that cannot.
pub fn approval_detail(ctx: &DesktopToolCtx, tool: &str, args: &Value) -> Option<Value> {
    let root = &ctx.workspace.path;
    let path_arg = args.get("path").and_then(|v| v.as_str()).unwrap_or_default();
    let full = resolve_within(root, path_arg).unwrap_or_else(|_| path_arg.to_string());
    match tool {
        FS_WRITE => Some(json!({
            "kind": "diff",
            "path": path_arg,
            "full_path": full,
            "workspace": root,
            "workspace_id": ctx.workspace.id,
            // The old text is not here: the instance does not hold the file. The
            // client renders the difference against what is on its own disk, which
            // is the only copy there is.
            "new_content": args.get("new_content").and_then(|v| v.as_str()).unwrap_or_default(),
        })),
        FS_DELETE => Some(json!({
            "kind": "delete",
            "path": path_arg,
            "full_path": full,
            "workspace": root,
            "workspace_id": ctx.workspace.id,
        })),
        TERMINAL_RUN => {
            let command = args.get("command").and_then(|v| v.as_str()).unwrap_or_default();
            Some(json!({
                "kind": "command",
                "command": command,
                "cwd": root,
                "workspace": root,
                "workspace_id": ctx.workspace.id,
                // Whether the command asked to reach the network. The card offers
                // to allow it with the network when so, and an agreement remembered
                // then is remembered as covering the network too.
                "net": args.get("net").and_then(|v| v.as_bool()).unwrap_or(false),
                // What agreeing to the prefix would agree to next time. Absent when
                // the command is one that can never be covered by an agreement.
                "prefix": suggested_prefix(command),
            }))
        }
        _ => None,
    }
}

/// The sentence form of the same question, for a client that has not learnt to
/// render the structured one.
pub fn approval_summary(ctx: &DesktopToolCtx, tool: &str, args: &Value) -> String {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("a file");
    match tool {
        FS_WRITE => format!("Write {path} in {}?", ctx.workspace.path),
        FS_DELETE => format!("Delete {path} from {}?", ctx.workspace.path),
        TERMINAL_RUN => format!(
            "Run `{}` in {}?",
            args.get("command").and_then(|v| v.as_str()).unwrap_or(""),
            ctx.workspace.path
        ),
        other => format!("Run `{other}` in {}?", ctx.workspace.path),
    }
}

/// The prefix worth offering to remember: the command's first two words, which is
/// where the difference between `npm test` and `npm publish` lives. `None` when
/// the command could never be covered by one.
fn suggested_prefix(command: &str) -> Option<String> {
    let cmd = command.trim();
    if cmd.is_empty() || CHAINING.iter().any(|c| cmd.contains(c)) {
        return None;
    }
    let words: Vec<&str> = cmd.split_whitespace().take(2).collect();
    if words.is_empty() { None } else { Some(words.join(" ")) }
}

/// The schema the model sees for each tool. Kept beside the rules above so a new
/// argument and the check that reads it are written in one place.
pub fn def(name: &str) -> Option<Value> {
    let (description, parameters) = match name {
        FS_LIST => (
            "List what is in the connected folder on the user's own computer. Paths are relative to that folder. Use this before reading or writing, so you are working from what is actually there.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "A folder inside the connected folder. Omit for the folder itself." },
                    "depth": { "type": "integer", "description": "How many levels down to list. Defaults to 1." }
                }
            }),
        ),
        FS_READ => (
            "Read a file in the connected folder on the user's own computer. Returns its text; a file that is not text comes back as a description of it instead.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "The file, relative to the connected folder." }
                },
                "required": ["path"]
            }),
        ),
        FS_WRITE => (
            "Create or replace a file in the connected folder on the user's own computer. The user is shown the change and has to agree to it before anything is written. Pass the whole intended contents, not a fragment. If you are replacing a file, read it first: a write is refused automatically if the file changed on disk since you read it, so re-read it and try again if that happens.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "The file, relative to the connected folder." },
                    "new_content": { "type": "string", "description": "The complete contents the file should have afterwards." }
                },
                "required": ["path", "new_content"]
            }),
        ),
        FS_DELETE => (
            "Delete a file, or an empty folder, in the connected folder on the user's own computer. The user is asked every time.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "The file or empty folder, relative to the connected folder." }
                },
                "required": ["path"]
            }),
        ),
        TERMINAL_RUN => (
            "Run a command on the user's own computer, in the connected folder. Its output comes back to you; long output is truncated. On a computer that confines commands, one that does not need the internet may run without interrupting the user, so do not set `net` unless the command genuinely needs to reach the network (installing packages, fetching a URL, cloning a remote): declaring it means the user is asked, and leaving it off but needing the network means the command simply cannot connect.",
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command line to run, as the user would type it." },
                    "timeout_secs": { "type": "integer", "description": "How long to allow it, in seconds. Defaults to 60." },
                    "net": { "type": "boolean", "description": "Set true only if the command needs to reach the network. Defaults to false, and the command then runs with no network access." }
                },
                "required": ["command"]
            }),
        ),
        _ => return None,
    };
    Some(json!({
        "type": "function",
        "function": { "name": name, "description": description, "parameters": parameters }
    }))
}

/// Check a call against the folder before it is sent anywhere: the level of trust
/// admits it, and every path it names stays inside. Returns the arguments to put
/// on the wire, with paths resolved against the folder so the client is not asked
/// to redo the joining (it still redoes the checking).
pub fn prepare(ctx: &DesktopToolCtx, tool: &str, args: &Value) -> Result<Value> {
    tier_allows(ctx.workspace.tier, tool)?;
    let root = &ctx.workspace.path;
    let mut out = args.clone();
    let obj = out.as_object_mut().ok_or_else(|| {
        AppError::Validation("the arguments for a folder tool must be an object".into())
    })?;
    match tool {
        FS_LIST => {
            let rel = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let full = resolve_within(root, rel)?;
            obj.insert("full_path".into(), json!(full));
            let depth = args.get("depth").and_then(|v| v.as_i64()).unwrap_or(1).clamp(1, 4);
            obj.insert("depth".into(), json!(depth));
        }
        FS_READ | FS_DELETE => {
            let rel = required_path(args)?;
            obj.insert("full_path".into(), json!(resolve_within(root, rel)?));
        }
        FS_WRITE => {
            let rel = required_path(args)?;
            obj.insert("full_path".into(), json!(resolve_within(root, rel)?));
            if args.get("new_content").and_then(|v| v.as_str()).is_none() {
                return Err(AppError::Validation(
                    "new_content is required, and must be the file's whole intended contents".into(),
                ));
            }
        }
        TERMINAL_RUN => {
            let command = args
                .get("command")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .ok_or_else(|| AppError::Validation("a command is required".into()))?;
            obj.insert("command".into(), json!(command));
            let secs = args.get("timeout_secs").and_then(|v| v.as_i64()).unwrap_or(60).clamp(1, 600);
            obj.insert("timeout_secs".into(), json!(secs));
            // Settle the network request to a plain flag the machine acts on: a
            // command that did not ask runs with no network.
            let net = args.get("net").and_then(|v| v.as_bool()).unwrap_or(false);
            obj.insert("net".into(), json!(net));
        }
        other => return Err(AppError::Validation(format!("unknown folder tool '{other}'"))),
    }
    obj.insert("workspace_id".into(), json!(ctx.workspace.id));
    obj.insert("workspace_path".into(), json!(root));
    Ok(out)
}

/// Ask the machine at the other end of this turn's socket to do the work, and
/// wait for it to say what happened.
///
/// Three ways this ends, and none of them is waiting forever: the machine
/// answers, the socket goes (the person shut the lid, the client crashed), or
/// the budget runs out. The last two come back as a tool error the model can
/// report, because a conversation that hangs on a machine that is not there is
/// worse than one that says so.
pub async fn execute(
    state: &crate::state::AppState,
    auth: &crate::auth::AuthContext,
    chat_id: Uuid,
    tx: &tokio::sync::mpsc::Sender<crate::ws::protocol::ServerFrame>,
    ctx: &DesktopToolCtx,
    turn_id: Uuid,
    tool: &str,
    args: &Value,
) -> Result<String> {
    use crate::ws::protocol::ServerFrame;

    // A refusal here is a path that would have left the folder, or an action the
    // level of trust does not admit. Both are recorded before the call goes
    // anywhere: the attempt is the interesting part, and a model asking for a
    // file outside the folder it was given is exactly what somebody watching for
    // an injected instruction is watching for.
    let prepared = match prepare(ctx, tool, args) {
        Ok(prepared) => prepared,
        Err(e) => {
            audit_refused(state, auth, chat_id, ctx, tool, args, &e.to_string()).await;
            return Err(e);
        }
    };
    let call_id = Uuid::now_v7();
    // Bound to the folder's machine. `load_ctx` already proved this is the device
    // the turn came in on, so only that machine's socket may answer the call.
    let rx = state.desktop_calls.register(call_id, turn_id, ctx.workspace.device_id);
    // Whatever happens next — an error, a timeout, the whole turn being dropped —
    // the waiter goes with it rather than being left for a reply that will never
    // be matched to anything.
    let _guard = CallGuard { calls: state.desktop_calls.clone(), call_id };

    let frame = ServerFrame::DesktopToolCall {
        call_id,
        turn_id,
        tool: tool.to_string(),
        args: prepared,
    };
    // Interactive turns push on their own socket; a scheduled job addresses the
    // machine's live socket through the hub. Either way a failure to hand the
    // frame off means the machine is not there, said plainly.
    let delivered = match ctx.route {
        DesktopSink::TurnSocket => tx.send(frame).await.is_ok(),
        DesktopSink::DeviceRoute { user_id, device_id } => {
            state.hub.send_to_device(user_id, device_id, frame)
        }
    };
    if !delivered {
        return Err(AppError::Validation(
            "the desktop client is no longer connected, so nothing was done".into(),
        ));
    }

    let budget = crate::tools::timeout_for(tool, &state.boot.tool_timeout_secs);
    let reply = match ctx.route {
        DesktopSink::TurnSocket => tokio::select! {
            reply = rx => reply.ok(),
            // The socket's writer has gone: nobody is going to answer.
            _ = tx.closed() => {
                return Err(AppError::Validation(
                    "the desktop client disconnected before this finished".into(),
                ))
            }
            _ = tokio::time::sleep(budget) => {
                return Err(AppError::Validation(
                    "the desktop client did not answer in time".into(),
                ))
            }
        },
        // The throwaway sender of a scheduled turn never closes, so it cannot
        // signal a gone machine; a presence poll stands in for `tx.closed()`,
        // stopping the wait as soon as the machine's socket drops.
        DesktopSink::DeviceRoute { user_id, device_id } => tokio::select! {
            reply = rx => reply.ok(),
            _ = tokio::time::sleep(budget) => {
                return Err(AppError::Validation(
                    "the device did not answer in time".into(),
                ))
            }
            _ = async {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
                loop {
                    tick.tick().await;
                    if !state.hub.is_device_online(user_id, device_id) {
                        break;
                    }
                }
            } => {
                return Err(AppError::Validation(
                    "the device went offline before this finished".into(),
                ))
            }
        },
    };
    let reply = reply.ok_or_else(|| {
        AppError::Validation("the desktop client did not answer".into())
    })?;
    if !reply.ok {
        return Err(AppError::Validation(reply_message(&reply.result)));
    }
    Ok(render(tool, &reply.result))
}

/// Record an attempt that was refused before it left the instance.
async fn audit_refused(
    state: &crate::state::AppState,
    auth: &crate::auth::AuthContext,
    chat_id: Uuid,
    ctx: &DesktopToolCtx,
    tool: &str,
    args: &Value,
    reason: &str,
) {
    let mut ev = crate::audit::AuditEvent::action("tool.denied", auth.role.as_str());
    ev.actor_user_id = auth.user_id;
    ev.outcome = crate::audit::AuditOutcome::Failure;
    ev.outcome_reason = Some("workspace".into());
    ev.resource_type = Some("tool".into());
    ev.resource_id = Some(chat_id);
    ev.payload = Some(json!({
        "chat_id": chat_id,
        "tool": tool,
        "denied": "workspace",
        "reason": reason,
        "device_id": ctx.workspace.device_id,
        "workspace_id": ctx.workspace.id,
        "requested_path": args.get("path").and_then(|v| v.as_str()),
    }));
    let _ = crate::audit::append(&state.pg, &ev).await;
}

struct CallGuard {
    calls: crate::state::DesktopCalls,
    call_id: Uuid,
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        self.calls.forget(self.call_id);
    }
}

/// How much of a machine's answer is worth putting in front of the model. Output
/// beyond this is cut with a note saying so, rather than filling the context with
/// the middle of a build log.
const MAX_RESULT_CHARS: usize = 24_000;

fn reply_message(result: &Value) -> String {
    result
        .get("error")
        .or_else(|| result.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("the desktop client could not do that")
        .to_string()
}

/// Turn what the machine sent back into what the model reads.
fn render(tool: &str, result: &Value) -> String {
    let body = match tool {
        FS_READ => result
            .get("content")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| result.to_string()),
        TERMINAL_RUN => {
            let stdout = result.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
            let stderr = result.get("stderr").and_then(|v| v.as_str()).unwrap_or("");
            let code = result.get("exit_code").and_then(|v| v.as_i64()).unwrap_or_default();
            let mut out = format!("exit code {code}\n");
            if !stdout.is_empty() {
                out.push_str(stdout);
            }
            if !stderr.is_empty() {
                out.push_str("\nstderr:\n");
                out.push_str(stderr);
            }
            out
        }
        _ => serde_json::to_string(result).unwrap_or_else(|_| result.to_string()),
    };
    truncate(&body)
}

fn truncate(body: &str) -> String {
    if body.chars().count() <= MAX_RESULT_CHARS {
        return body.to_string();
    }
    let kept: String = body.chars().take(MAX_RESULT_CHARS).collect();
    format!("{kept}\n… (truncated; ask for a narrower part if you need the rest)")
}

fn required_path(args: &Value) -> Result<&str> {
    args.get("path")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| AppError::Validation("a path is required".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefix(p: &str, with_network: bool) -> CommandPrefix {
        CommandPrefix { prefix: p.into(), with_network }
    }

    fn ws(tier: Tier) -> DesktopToolCtx {
        DesktopToolCtx {
            workspace: Workspace {
                id: Uuid::nil(),
                device_id: Uuid::nil(),
                path: "C:\\work\\demo".into(),
                label: "demo".into(),
                tier,
            },
            command_prefixes: vec![prefix("npm test", false)],
            sandbox_tier: "lifecycle".into(),
            route: DesktopSink::TurnSocket,
        }
    }

    use crate::tools::ToolEffect;

    /// A command on a lifecycle machine: asked about unless an agreed prefix covers
    /// it, and never relieved by the boundary (there is none). Fixes the arguments
    /// that only matter on a full-tier machine so each test reads as one case.
    fn gated_lifecycle(tool: &str, pre_approved: bool, prefix_ok: bool) -> bool {
        desktop_call_gated(tool, true, ToolEffect::RequiresRun, pre_approved, prefix_ok, false, false, false)
    }

    #[test]
    fn sandbox_tier_reads_full_only_from_an_explicit_claim() {
        // An explicit full claim is honoured; everything else settles to the
        // weaker tier, so an old, silent, or wrongly-claiming client is only ever
        // trusted less than it says.
        assert_eq!(sandbox_tier_from_capabilities(&["sandbox:full".into()]), "full");
        assert_eq!(
            sandbox_tier_from_capabilities(&["folder".into(), "sandbox:full".into()]),
            "full"
        );
        assert_eq!(sandbox_tier_from_capabilities(&["sandbox:lifecycle".into()]), "lifecycle");
        assert_eq!(sandbox_tier_from_capabilities(&[]), "lifecycle");
        assert_eq!(sandbox_tier_from_capabilities(&["sandbox:other".into()]), "lifecycle");
    }

    #[test]
    fn a_lifecycle_device_gates_a_command_exactly_as_before() {
        // A command on a machine that keeps only the process-lifetime line is asked
        // about unless an agreed prefix already covers it: the rule from before the
        // boundary existed, unchanged.
        let d = ws(Tier::ReadWrite);
        assert_eq!(d.sandbox_tier, "lifecycle");
        assert!(d.allowed_prefix("npm test --watch", false).is_some());
        assert!(gated_lifecycle(TERMINAL_RUN, false, false));
        assert!(!gated_lifecycle(TERMINAL_RUN, false, true));
    }

    #[test]
    fn a_full_device_runs_a_confined_command_without_asking() {
        // The payoff: a confined command that does not want the network has nothing
        // left to weigh, so it runs without a card, even with no agreed prefix.
        let full = true;
        assert!(!desktop_call_gated(TERMINAL_RUN, true, ToolEffect::RequiresRun, false, false, full, false, false));
        // But a command that wants the network is still asked about.
        assert!(desktop_call_gated(TERMINAL_RUN, true, ToolEffect::RequiresRun, false, false, full, true, false));
        // And the paranoid-administrator switch puts the card back on everything.
        assert!(desktop_call_gated(TERMINAL_RUN, true, ToolEffect::RequiresRun, false, false, full, false, true));
    }

    #[test]
    fn a_network_command_is_covered_only_by_a_network_agreement() {
        // A plain agreement does not cover a command that needs the network; an
        // agreement made with the network does.
        let plain = [prefix("git", false)];
        let networked = [prefix("git", true)];
        assert!(matching_prefix("git status", &plain, false).is_some());
        assert!(matching_prefix("git pull", &plain, true).is_none());
        assert!(matching_prefix("git pull", &networked, true).is_some());
        // A network agreement still covers a command that does not need it.
        assert!(matching_prefix("git status", &networked, false).is_some());
    }

    #[test]
    fn gating_asks_about_writes_unless_pre_approved() {
        // A write is asked about by default, and skips the card only when the
        // automation pre-approved writes to this folder.
        assert!(gated_lifecycle(FS_WRITE, false, false));
        assert!(!gated_lifecycle(FS_WRITE, true, false));
    }

    #[test]
    fn gating_never_lets_a_delete_through_a_pre_approval() {
        // Deletion is asked about every time; pre-approved writes do not touch it.
        assert!(gated_lifecycle(FS_DELETE, true, false));
        assert!(gated_lifecycle(FS_DELETE, false, false));
    }

    #[test]
    fn gating_skips_a_command_with_an_agreed_prefix_only() {
        assert!(gated_lifecycle(TERMINAL_RUN, false, false));
        assert!(!gated_lifecycle(TERMINAL_RUN, false, true));
        // A pre-approval on writes must not spill over onto a command.
        assert!(gated_lifecycle(TERMINAL_RUN, true, false));
    }

    #[test]
    fn gating_ignores_reads_and_unoffered_names() {
        assert!(!desktop_call_gated(FS_READ, true, ToolEffect::ReadOnly, false, false, false, false, false));
        // A name that was not offered this turn is not weighed up here.
        assert!(!desktop_call_gated(FS_WRITE, false, ToolEffect::RequiresRun, false, false, false, false, false));
    }

    #[test]
    fn a_folder_path_is_absolute_local_and_free_of_climbs() {
        assert_eq!(normalise_root("C:\\work\\demo\\").unwrap(), "C:\\work\\demo");
        assert_eq!(normalise_root("c:/work/demo").unwrap(), "C:\\work\\demo");
        assert_eq!(normalise_root("/home/ada/demo/").unwrap(), "/home/ada/demo");
        assert_eq!(normalise_root("C:\\").unwrap(), "C:\\");
        assert!(normalise_root("work/demo").is_err());
        assert!(normalise_root("C:\\work\\..\\other").is_err());
        assert!(normalise_root("\\\\server\\share").is_err());
        assert!(normalise_root("//server/share").is_err());
        assert!(normalise_root("").is_err());
        assert!(normalise_root("C:\\work\\demo\u{0}").is_err());
    }

    #[test]
    fn a_path_that_leaves_the_folder_is_refused_however_it_is_written() {
        let root = "C:\\work\\demo";
        assert_eq!(resolve_within(root, "notes.md").unwrap(), "C:\\work\\demo\\notes.md");
        assert_eq!(resolve_within(root, "src/main.rs").unwrap(), "C:\\work\\demo\\src\\main.rs");
        assert_eq!(resolve_within(root, "./a/./b").unwrap(), "C:\\work\\demo\\a\\b");
        assert_eq!(resolve_within(root, "").unwrap(), root);

        for escape in ["..\\secrets", "../secrets", "a/../../b", "\\Windows\\system32",
                       "/etc/passwd", "D:\\other", "notes.md:stream"] {
            assert!(
                resolve_within(root, escape).is_err(),
                "{escape} was not refused"
            );
        }
    }

    #[test]
    fn a_posix_folder_joins_with_its_own_separator() {
        assert_eq!(resolve_within("/home/ada/demo", "src/lib.rs").unwrap(), "/home/ada/demo/src/lib.rs");
        assert!(resolve_within("/home/ada/demo", "../other").is_err());
    }

    #[test]
    fn trust_narrows_what_may_happen() {
        assert!(tier_allows(Tier::ReadOnly, FS_READ).is_ok());
        assert!(tier_allows(Tier::ReadOnly, FS_LIST).is_ok());
        assert!(tier_allows(Tier::ReadOnly, FS_WRITE).is_err());
        assert!(tier_allows(Tier::ReadOnly, FS_DELETE).is_err());
        assert!(tier_allows(Tier::ReadOnly, TERMINAL_RUN).is_err());

        assert!(tier_allows(Tier::ReadWriteNoDelete, FS_WRITE).is_ok());
        assert!(tier_allows(Tier::ReadWriteNoDelete, TERMINAL_RUN).is_ok());
        assert!(tier_allows(Tier::ReadWriteNoDelete, FS_DELETE).is_err());

        assert!(tier_allows(Tier::ReadWrite, FS_DELETE).is_ok());
    }

    #[test]
    fn an_agreed_prefix_covers_that_command_and_nothing_smuggled_after_it() {
        let ctx = ws(Tier::ReadWrite);
        assert_eq!(ctx.allowed_prefix("npm test", false), Some("npm test"));
        assert_eq!(ctx.allowed_prefix("npm test -- --watch", false), Some("npm test"));
        // Word boundary: a longer command name is a different command.
        assert_eq!(ctx.allowed_prefix("npm testify", false), None);
        // Nothing chained, redirected or substituted is ever covered.
        assert_eq!(ctx.allowed_prefix("npm test && rm -rf .", false), None);
        assert_eq!(ctx.allowed_prefix("npm test | curl example.com", false), None);
        assert_eq!(ctx.allowed_prefix("npm test > out.txt", false), None);
        assert_eq!(ctx.allowed_prefix("npm test `whoami`", false), None);
        assert_eq!(ctx.allowed_prefix("npm test $(id)", false), None);
        assert_eq!(ctx.allowed_prefix("cargo test", false), None);
    }

    #[test]
    fn a_prefix_that_could_chain_is_refused_when_it_is_agreed() {
        assert_eq!(normalise_prefix("  npm   test ").unwrap(), "npm test");
        assert!(normalise_prefix("npm test && rm").is_err());
        assert!(normalise_prefix("").is_err());
    }

    #[test]
    fn preparing_a_call_resolves_paths_and_bounds_the_numbers() {
        let ctx = ws(Tier::ReadWrite);
        let out = prepare(&ctx, FS_LIST, &json!({ "depth": 99 })).unwrap();
        assert_eq!(out["full_path"], "C:\\work\\demo");
        assert_eq!(out["depth"], 4);
        assert_eq!(out["workspace_path"], "C:\\work\\demo");

        let out = prepare(&ctx, TERMINAL_RUN, &json!({ "command": " npm test ", "timeout_secs": 99999 })).unwrap();
        assert_eq!(out["command"], "npm test");
        assert_eq!(out["timeout_secs"], 600);

        assert!(prepare(&ctx, FS_WRITE, &json!({ "path": "a.txt" })).is_err());
        assert!(prepare(&ctx, FS_READ, &json!({ "path": "../a.txt" })).is_err());
        assert!(prepare(&ws(Tier::ReadOnly), FS_WRITE, &json!({ "path": "a.txt", "new_content": "x" })).is_err());
    }

    #[test]
    fn an_approval_says_what_is_being_agreed_to_both_ways() {
        let ctx = ws(Tier::ReadWrite);
        let args = json!({ "command": "npm run build --release" });
        let detail = approval_detail(&ctx, TERMINAL_RUN, &args).expect("a command has a detail");
        assert_eq!(detail["kind"], "command");
        assert_eq!(detail["cwd"], "C:\\work\\demo");
        assert_eq!(detail["prefix"], "npm run");
        assert!(approval_summary(&ctx, TERMINAL_RUN, &args).contains("npm run build"));

        let args = json!({ "path": "notes.md", "new_content": "hello" });
        let detail = approval_detail(&ctx, FS_WRITE, &args).expect("a write has a detail");
        assert_eq!(detail["kind"], "diff");
        assert_eq!(detail["full_path"], "C:\\work\\demo\\notes.md");
        // The card needs the folder's id to agree a command prefix or ask for a
        // preview, and cannot get it from the raw model arguments.
        assert_eq!(detail["workspace_id"], ctx.workspace.id.to_string());

        // A command that chains offers nothing to remember: agreeing to how it
        // starts would agree to whatever follows.
        let detail = approval_detail(&ctx, TERMINAL_RUN, &json!({ "command": "a && b" })).unwrap();
        assert!(detail["prefix"].is_null());

        assert!(approval_detail(&ctx, FS_READ, &json!({ "path": "a" })).is_none());
    }
}
