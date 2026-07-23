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

//! Putting a file back.
//!
//! Before anything is written or deleted in a connected folder, the file as it
//! stood is copied aside and a line is added to a log of what happened. That is
//! the whole of it, and it is what turns an agent from something to be watched
//! into something to be tried: a change that can be undone in one click is a
//! cheap experiment, and a change that cannot is a decision.
//!
//! What it does not cover, and what the interface says out loud the first time
//! anybody restores anything: files changed by a command the agent ran. A
//! command is an arbitrary program, and the only honest way to describe what it
//! touched is not to claim to know.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// How long copies are kept before the next start sweeps them away.
const RETENTION_DAYS: u64 = 7;

/// What happened to one file, and what it looked like beforehand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    /// Identifies this change to the interface offering to undo it.
    pub id: String,
    /// The run of the client this happened in, so a sweep can drop whole runs.
    pub session: String,
    /// The conversation turn that asked for it, which is how a summary at the
    /// end of a turn knows what to list.
    pub turn: String,
    /// The file, as a person reads it.
    pub path: String,
    /// `write` | `delete`.
    pub op: String,
    /// Was there a file here before? A change that created one is undone by
    /// removing it again.
    pub existed: bool,
    /// Where the previous contents are, when there were any.
    pub backup: Option<String>,
    /// Seconds since the epoch. A plain number: the sweep compares it with the
    /// clock and nothing else reads it.
    pub at: u64,
    /// Has this change already been put back? Restoring twice is not wrong, but
    /// the interface should not keep offering to undo something undone.
    #[serde(default)]
    pub restored: bool,
}

fn root(app: &AppHandle) -> Result<PathBuf> {
    let dir = app.path().app_data_dir().context("this system has no application data directory")?;
    let dir = dir.join("backups");
    std::fs::create_dir_all(&dir).context("could not create the backup directory")?;
    Ok(dir)
}

fn manifest_path(app: &AppHandle) -> Result<PathBuf> {
    Ok(root(app)?.join("manifest.jsonl"))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// A short, stable, filesystem-safe name for a path, so two files with the same
/// name in different folders do not land on top of each other.
fn hashed(path: &str) -> String {
    // A small non-cryptographic hash: this names a file in a private directory,
    // it does not protect anything.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{h:016x}")
}

fn read_manifest(app: &AppHandle) -> Vec<Change> {
    let Ok(path) = manifest_path(app) else { return Vec::new() };
    let Ok(raw) = std::fs::read_to_string(path) else { return Vec::new() };
    raw.lines().filter(|l| !l.trim().is_empty()).filter_map(|l| serde_json::from_str(l).ok()).collect()
}

fn write_manifest(app: &AppHandle, changes: &[Change]) -> Result<()> {
    let path = manifest_path(app)?;
    let mut out = String::new();
    for c in changes {
        out.push_str(&serde_json::to_string(c).unwrap_or_default());
        out.push('\n');
    }
    std::fs::write(path, out).context("could not write the record of changes")
}

fn append(app: &AppHandle, change: &Change) -> Result<()> {
    use std::io::Write;
    let path = manifest_path(app)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .context("could not open the record of changes")?;
    writeln!(file, "{}", serde_json::to_string(change).unwrap_or_default())
        .context("could not add to the record of changes")
}

/// Take a copy of a file about to be written or deleted, and record what is
/// about to happen. Called before the change, so a failure to keep a copy stops
/// the change rather than leaving one that cannot be undone.
pub fn keep(
    app: &AppHandle,
    session: &str,
    turn: &str,
    target: &Path,
    op: &str,
) -> Result<Change> {
    let existed = target.exists();
    let display = crate::folders::display(target);
    let id = format!("{}-{}", now(), hashed(&format!("{display}{turn}{op}")));
    let backup = if existed {
        let dir = root(app)?.join(session).join(turn);
        std::fs::create_dir_all(&dir).context("could not create the backup directory")?;
        let copy = dir.join(format!("{}.bak", hashed(&display)));
        std::fs::copy(target, &copy)
            .with_context(|| format!("could not keep a copy of {display}"))?;
        Some(crate::folders::display(&copy))
    } else {
        None
    };
    let change = Change {
        id,
        session: session.to_string(),
        turn: turn.to_string(),
        path: display,
        op: op.to_string(),
        existed,
        backup,
        at: now(),
        restored: false,
    };
    append(app, &change)?;
    Ok(change)
}

/// What was changed in one turn, oldest first.
pub fn for_turn(app: &AppHandle, turn: &str) -> Vec<Change> {
    read_manifest(app).into_iter().filter(|c| c.turn == turn).collect()
}

/// Has the file been touched since the agent changed it? A restore then would put
/// an older version over something the person did afterwards, silently losing it.
/// The recorded `at` is when the copy was taken (the moment before the change), so
/// a file whose modified time is later than that has moved on since. A slightly
/// generous margin covers filesystems with coarse timestamps.
fn changed_since(change: &Change) -> bool {
    let target = PathBuf::from(&change.path);
    let Ok(meta) = std::fs::metadata(&target) else {
        return false; // gone, or unreadable: nothing newer to lose.
    };
    let Ok(modified) = meta.modified() else { return false };
    let modified_secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    modified_secs > change.at + 1
}

/// Put one file back the way it was. Refuses, unless `force`, when the file has
/// been changed since the agent touched it — a restore then would quietly discard
/// that later change.
pub fn restore_one(app: &AppHandle, id: &str, force: bool) -> Result<String> {
    let mut changes = read_manifest(app);
    let Some(change) = changes.iter_mut().find(|c| c.id == id) else {
        bail!("that change is not in the record any more");
    };
    if !force && change.op == "write" && changed_since(change) {
        bail!("changed-since: {} has been edited since; restoring would discard that", change.path);
    }
    apply(change)?;
    change.restored = true;
    let path = change.path.clone();
    write_manifest(app, &changes)?;
    Ok(path)
}

/// Put back everything one turn changed, most recent first so that a file written
/// twice in a turn ends at the state it had before the turn. A file that has been
/// changed since is skipped unless `force`, and counted separately so the caller
/// can say what it left alone.
pub fn restore_turn(app: &AppHandle, turn: &str, force: bool) -> Result<(usize, usize)> {
    let mut changes = read_manifest(app);
    let mut restored = 0usize;
    let mut skipped = 0usize;
    let mut indices: Vec<usize> = changes
        .iter()
        .enumerate()
        .filter(|(_, c)| c.turn == turn && !c.restored)
        .map(|(i, _)| i)
        .collect();
    indices.reverse();
    for i in indices {
        if !force && changes[i].op == "write" && changed_since(&changes[i]) {
            skipped += 1;
            continue;
        }
        match apply(&changes[i]) {
            Ok(()) => {
                changes[i].restored = true;
                restored += 1;
            }
            Err(e) => tracing::warn!(error = %e, path = %changes[i].path, "could not put a file back"),
        }
    }
    write_manifest(app, &changes)?;
    Ok((restored, skipped))
}

/// The undo itself: copy the kept file back, or — when the change created the
/// file — remove it again, which is what undoing a creation means.
fn apply(change: &Change) -> Result<()> {
    let target = PathBuf::from(&change.path);
    match (&change.backup, change.existed) {
        (Some(backup), true) => {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::copy(backup, &target)
                .with_context(|| format!("could not put {} back", change.path))?;
            Ok(())
        }
        (_, false) => {
            match std::fs::remove_file(&target) {
                Ok(()) => Ok(()),
                // Already gone is the state being asked for.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e).with_context(|| format!("could not remove {}", change.path)),
            }
        }
        (None, true) => bail!("there is no copy of {} to put back", change.path),
    }
}

/// Drop copies older than the retention window, and the manifest lines that name
/// them. Run at startup: a machine should not slowly fill with copies of
/// everything an agent has ever touched.
pub fn sweep(app: &AppHandle) {
    let cutoff = now().saturating_sub(RETENTION_DAYS * 24 * 60 * 60);
    let changes = read_manifest(app);
    if changes.is_empty() {
        return;
    }
    let (keep, drop): (Vec<Change>, Vec<Change>) =
        changes.into_iter().partition(|c| c.at >= cutoff);
    if drop.is_empty() {
        return;
    }
    for change in &drop {
        if let Some(backup) = &change.backup {
            let _ = std::fs::remove_file(backup);
        }
    }
    // Whole session directories that are now empty go too.
    if let Ok(dir) = root(app) {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    let _ = remove_if_empty(&entry.path());
                }
            }
        }
    }
    if let Err(e) = write_manifest(app, &keep) {
        tracing::warn!(error = %e, "could not tidy the record of changes");
    } else {
        tracing::info!(dropped = drop.len(), "swept backups past the retention window");
    }
}

/// Remove a directory tree if nothing is left in it, one level down included.
fn remove_if_empty(dir: &Path) -> std::io::Result<()> {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                let _ = remove_if_empty(&entry.path());
            }
        }
    }
    std::fs::remove_dir(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_names_its_copy_the_same_way_every_time() {
        assert_eq!(hashed("C:\\work\\a.txt"), hashed("C:\\work\\a.txt"));
        assert_ne!(hashed("C:\\work\\a.txt"), hashed("C:\\other\\a.txt"));
        assert_eq!(hashed("C:\\work\\a.txt").len(), 16);
    }

    /// The undo itself, without an application handle: `apply` is the part that
    /// touches the disk, and it is the part worth pinning.
    #[test]
    fn putting_a_changed_file_back_restores_its_previous_contents() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("notes.md");
        let backup = dir.path().join("notes.bak");
        std::fs::write(&backup, "before").unwrap();
        std::fs::write(&target, "after").unwrap();

        let change = Change {
            id: "1".into(),
            session: "s".into(),
            turn: "t".into(),
            path: target.to_string_lossy().to_string(),
            op: "write".into(),
            existed: true,
            backup: Some(backup.to_string_lossy().to_string()),
            at: now(),
            restored: false,
        };
        apply(&change).expect("put back");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "before");
    }

    #[test]
    fn undoing_a_created_file_removes_it_again() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("fresh.md");
        std::fs::write(&target, "new").unwrap();

        let change = Change {
            id: "1".into(),
            session: "s".into(),
            turn: "t".into(),
            path: target.to_string_lossy().to_string(),
            op: "write".into(),
            existed: false,
            backup: None,
            at: now(),
            restored: false,
        };
        apply(&change).expect("removed");
        assert!(!target.exists(), "a file that was created is undone by removing it");
        // Doing it twice is the state being asked for, not an error.
        apply(&change).expect("idempotent");
    }

    #[test]
    fn undoing_a_deletion_brings_the_file_back() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("gone.md");
        let backup = dir.path().join("gone.bak");
        std::fs::write(&backup, "the contents").unwrap();
        assert!(!target.exists());

        let change = Change {
            id: "1".into(),
            session: "s".into(),
            turn: "t".into(),
            path: target.to_string_lossy().to_string(),
            op: "delete".into(),
            existed: true,
            backup: Some(backup.to_string_lossy().to_string()),
            at: now(),
            restored: false,
        };
        apply(&change).expect("brought back");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "the contents");
    }

    #[test]
    fn a_file_edited_since_the_agent_touched_it_reads_as_changed() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("notes.md");
        std::fs::write(&target, "later edit").unwrap();
        let path = target.to_string_lossy().to_string();

        // Backed up "a while ago" → the file's mtime (now) is newer → changed.
        let old = Change {
            id: "1".into(), session: "s".into(), turn: "t".into(), path: path.clone(),
            op: "write".into(), existed: true, backup: None, at: now().saturating_sub(3600),
            restored: false,
        };
        assert!(changed_since(&old), "a file modified after its backup is stale");

        // Backed up "in the future" (or this instant) → not newer → not stale.
        let fresh = Change { at: now() + 3600, ..old.clone() };
        assert!(!changed_since(&fresh));

        // A file that is gone has nothing newer to lose.
        std::fs::remove_file(&target).unwrap();
        assert!(!changed_since(&old));
    }

    #[test]
    fn a_change_with_no_copy_to_put_back_says_so() {
        let change = Change {
            id: "1".into(),
            session: "s".into(),
            turn: "t".into(),
            path: "C:\\work\\a.txt".into(),
            op: "write".into(),
            existed: true,
            backup: None,
            at: now(),
            restored: false,
        };
        assert!(apply(&change).is_err());
    }
}
