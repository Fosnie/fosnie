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

//! The folders this machine has been told it may work in.
//!
//! Kept here, on the machine, and not taken from the instance. The instance has
//! its own copy — that is what shows the owner what they have granted and what
//! the audit trail refers to — but a client that resolved a request against the
//! path in the request would have a folder boundary only as strong as the
//! connection. What may be touched is decided from this file, written when the
//! person in front of the machine agreed to it.
//!
//! Nothing secret is in it: a path, a level of trust, the instance it belongs
//! to. The credential lives in the operating system's store and only there.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// How much the owner agreed to. The same three levels the instance records;
/// parsed rather than trusted as a string so an unknown value is a refusal and
/// not an accidental promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tier {
    #[serde(rename = "ro")]
    ReadOnly,
    #[serde(rename = "rw")]
    ReadWrite,
    #[serde(rename = "rw_nd")]
    ReadWriteNoDelete,
}

impl Tier {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "ro" => Some(Tier::ReadOnly),
            "rw" => Some(Tier::ReadWrite),
            "rw_nd" => Some(Tier::ReadWriteNoDelete),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::ReadOnly => "ro",
            Tier::ReadWrite => "rw",
            Tier::ReadWriteNoDelete => "rw_nd",
        }
    }

    /// Does this level admit the tool being asked for? The last of the three
    /// checks this request passes (the instance made the same decision from its
    /// own copy); it is here because the machine that owns the files is the one
    /// whose answer counts.
    pub fn allows(self, tool: &str) -> bool {
        match (self, tool) {
            (Tier::ReadOnly, "desktop.fs_write" | "desktop.fs_delete" | "desktop.terminal_run") => {
                false
            }
            (Tier::ReadWriteNoDelete, "desktop.fs_delete") => false,
            _ => true,
        }
    }
}

/// One folder, as this machine holds it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Folder {
    /// The instance's id for this grant, which is what a request names.
    pub workspace_id: String,
    /// The canonical path, as this machine resolves it.
    pub path: String,
    pub tier: Tier,
    /// Which instance granted it, so a machine re-paired to another one does not
    /// carry the first one's folders over.
    pub base_url: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Book {
    #[serde(default)]
    folders: Vec<Folder>,
}

/// Where the record lives. Beside the backups, in this application's own data
/// directory, so uninstalling takes both away together.
fn book_path(app: &AppHandle) -> Result<PathBuf> {
    let dir = app.path().app_data_dir().context("this system has no application data directory")?;
    std::fs::create_dir_all(&dir).context("could not create the application data directory")?;
    Ok(dir.join("folders.json"))
}

fn read_book(app: &AppHandle) -> Book {
    // A record that cannot be read is treated as no record: the person is asked
    // to connect the folder again, which is a nuisance. Guessing at a damaged
    // file would be worse than a nuisance.
    let Ok(path) = book_path(app) else { return Book::default() };
    match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "the folder record could not be read; starting a new one");
            Book::default()
        }),
        Err(_) => Book::default(),
    }
}

fn write_book(app: &AppHandle, book: &Book) -> Result<()> {
    let path = book_path(app)?;
    let raw = serde_json::to_string_pretty(book).context("could not write the folder record")?;
    std::fs::write(&path, raw).context("could not write the folder record")
}

/// Every folder this machine holds for the instance it is paired with.
pub fn list(app: &AppHandle, base_url: &str) -> Vec<Folder> {
    read_book(app).folders.into_iter().filter(|f| f.base_url == base_url).collect()
}

/// The folder a request names, or nothing. Nothing means the request is not
/// worked on: an id this machine has no record of is not a folder anybody here
/// agreed to.
pub fn resolve(app: &AppHandle, base_url: &str, workspace_id: &str) -> Option<Folder> {
    read_book(app)
        .folders
        .into_iter()
        .find(|f| f.workspace_id == workspace_id && f.base_url == base_url)
}

/// Record a folder the person has just agreed to. Replaces an earlier record of
/// the same grant, so re-connecting at a different level of trust changes it
/// rather than leaving two answers to the same question.
pub fn remember(app: &AppHandle, folder: Folder) -> Result<()> {
    let mut book = read_book(app);
    book.folders.retain(|f| f.workspace_id != folder.workspace_id);
    book.folders.push(folder);
    write_book(app, &book)
}

/// Drop one folder from the record.
pub fn forget(app: &AppHandle, workspace_id: &str) -> Result<()> {
    let mut book = read_book(app);
    book.folders.retain(|f| f.workspace_id != workspace_id);
    write_book(app, &book)
}

/// Keep only the folders the instance still lists as live.
///
/// Withdrawing a folder is done from the web, and the machine holding it may not
/// have been running at the time. Without this it would keep a record of a grant
/// that no longer exists — harmless, because the instance would refuse to send it
/// any work, but a record that says something untrue about what this machine may
/// do is not worth keeping.
pub fn keep_only(app: &AppHandle, base_url: &str, live_ids: &[String]) -> Result<()> {
    let mut book = read_book(app);
    let before = book.folders.len();
    book.folders.retain(|f| f.base_url != base_url || live_ids.contains(&f.workspace_id));
    if book.folders.len() != before {
        tracing::info!(dropped = before - book.folders.len(), "folders withdrawn on the instance");
    }
    write_book(app, &book)
}

/// Resolve a path a request named, refusing anything that does not stay inside
/// the folder.
///
/// This is the check that can see what the instance's cannot: the path is
/// resolved against the real filesystem, so a symbolic link, a junction or a
/// case-folded duplicate is followed to wherever it actually leads before being
/// compared with the folder. A file that does not exist yet is checked through
/// the nearest ancestor that does, so a link part-way up cannot be used to land
/// outside.
pub fn within(root: &Path, relative: &str, must_exist: bool) -> Result<PathBuf> {
    let rel = relative.trim();
    let root = std::fs::canonicalize(root).context("the connected folder is not there any more")?;
    if rel.is_empty() || rel == "." {
        return Ok(root);
    }
    // Anything absolute, or naming a drive, addresses somewhere of its own
    // choosing rather than somewhere in the folder.
    if rel.starts_with('/') || rel.starts_with('\\') || rel.contains(':') {
        bail!("that path is outside the connected folder");
    }
    let joined = root.join(rel);
    let resolved = if must_exist {
        std::fs::canonicalize(&joined).with_context(|| format!("no such path: {rel}"))?
    } else {
        // A path that is not there yet is checked through the nearest ancestor
        // that is, so a link part-way up cannot be used to land outside. But the
        // final component itself may already exist and be a link — a file named
        // `notes.md` inside the folder that is really a symlink to somewhere else.
        // Appending it unresolved and calling it internal is the mistake that
        // lets a write follow the link out of the folder while the approval shows
        // an innocent inner path. So the leaf is resolved too.
        let parent = joined.parent().context("that path has nowhere to go")?;
        let base = std::fs::canonicalize(parent)
            .with_context(|| format!("no such folder for: {rel}"))?;
        let name = joined.file_name().context("that path has no file name")?;
        let candidate = base.join(name);
        match std::fs::symlink_metadata(&candidate) {
            // It is a link (a symbolic link, or a Windows junction): a write must
            // not go through it, wherever it leads. Refused rather than followed,
            // because a file whose name is a link is not a file to write in place.
            Ok(meta) if meta.file_type().is_symlink() => {
                bail!("that path is a link, which cannot be written through");
            }
            // It exists and is a real file or folder: resolve it fully and make
            // sure it still lands inside — defence against a hard link or a
            // case-folded duplicate that a plain join would not catch.
            Ok(_) => std::fs::canonicalize(&candidate)
                .with_context(|| format!("no such path: {rel}"))?,
            // Nothing is there yet: an ordinary new file. There is no link to
            // follow, and the parent was already resolved and checked.
            Err(_) => candidate,
        }
    };
    if !resolved.starts_with(&root) {
        bail!("that path is outside the connected folder");
    }
    Ok(resolved)
}

/// A path as a person would read it. Canonicalising on Windows yields the
/// verbatim form (`\\?\C:\…`), which is correct and unreadable, and is not what
/// the instance was told the folder is called.
pub fn display(path: &Path) -> String {
    path.to_string_lossy().trim_start_matches("\\\\?\\").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_level_of_trust_only_ever_narrows() {
        assert!(Tier::ReadOnly.allows("desktop.fs_read"));
        assert!(Tier::ReadOnly.allows("desktop.fs_list"));
        assert!(!Tier::ReadOnly.allows("desktop.fs_write"));
        assert!(!Tier::ReadOnly.allows("desktop.fs_delete"));
        assert!(!Tier::ReadOnly.allows("desktop.terminal_run"));

        assert!(Tier::ReadWriteNoDelete.allows("desktop.fs_write"));
        assert!(Tier::ReadWriteNoDelete.allows("desktop.terminal_run"));
        assert!(!Tier::ReadWriteNoDelete.allows("desktop.fs_delete"));

        assert!(Tier::ReadWrite.allows("desktop.fs_delete"));
    }

    #[test]
    fn an_unknown_level_is_not_a_level() {
        assert_eq!(Tier::parse("rw"), Some(Tier::ReadWrite));
        assert_eq!(Tier::parse("root"), None);
        assert_eq!(Tier::parse(""), None);
    }

    /// A folder with a file in it, and a sibling folder outside it.
    fn sandbox() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let root = dir.path().join("work");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("notes.md"), "hello").unwrap();
        std::fs::write(outside.join("secret.txt"), "not yours").unwrap();
        (dir, root, outside)
    }

    #[test]
    fn a_path_inside_the_folder_resolves() {
        let (_dir, root, _outside) = sandbox();
        let p = within(&root, "notes.md", true).expect("inside");
        assert!(p.ends_with("notes.md"));
        assert_eq!(within(&root, "", true).unwrap(), std::fs::canonicalize(&root).unwrap());
    }

    #[test]
    fn a_path_that_leaves_the_folder_is_refused() {
        let (_dir, root, _outside) = sandbox();
        for escape in ["../outside/secret.txt", "..\\outside\\secret.txt", "/etc/passwd"] {
            assert!(within(&root, escape, true).is_err(), "{escape} was not refused");
        }
        // A file to be created, named through a climb, is refused on the same
        // ground: the check is on where it would go.
        assert!(within(&root, "../outside/new.txt", false).is_err());
    }

    #[test]
    fn a_new_file_inside_the_folder_is_allowed_before_it_exists() {
        let (_dir, root, _outside) = sandbox();
        let p = within(&root, "fresh.md", false).expect("a file that is not there yet");
        assert!(p.starts_with(std::fs::canonicalize(&root).unwrap()));
        assert!(!p.exists());
    }

    /// The case the string check upstream cannot see: a link inside the folder
    /// that leads out of it. Skipped where the operating system will not let this
    /// process make one (Windows without developer mode), because a test that
    /// silently passes for the wrong reason is worse than one that says it did
    /// not run.
    #[test]
    fn a_link_that_leads_out_of_the_folder_is_refused() {
        let (_dir, root, outside) = sandbox();
        let link = root.join("escape");
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(&outside, &link).is_ok()
            // Making a symbolic link needs a privilege an ordinary account does
            // not have; a directory junction does not, and is followed the same
            // way. Either is the case worth testing: a name inside the folder
            // that leads out of it.
            || std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(&outside)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&outside, &link).is_ok();
        if !made {
            eprintln!("skip: this system does not let this process create a link");
            return;
        }
        assert!(
            within(&root, "escape/secret.txt", true).is_err(),
            "a link out of the folder must be refused where it actually leads"
        );
        // And for a file that would be created through the link (a link part-way
        // up the path).
        assert!(within(&root, "escape/new.txt", false).is_err());
        // And — the leaf-link case that a plain parent-only resolution misses — a
        // write whose FINAL component is itself the link. Before the fix this
        // passed the boundary check and the write followed the link outside.
        assert!(
            within(&root, "escape", false).is_err(),
            "a write whose leaf is a link must be refused, not followed out of the folder"
        );
    }

    #[test]
    fn a_readable_path_loses_the_verbatim_prefix() {
        let (_dir, root, _outside) = sandbox();
        let canonical = std::fs::canonicalize(&root).unwrap();
        assert!(!display(&canonical).starts_with("\\\\?\\"));
    }
}
