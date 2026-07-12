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

//! Storage path resolution.
//!
//! Configured storage dirs (`storage.*`) are relative by default (`./data/...`) and
//! resolved to an absolute path against the process cwd at runtime. **DB path
//! columns store a RELATIVE suffix under their category dir**, resolved to absolute
//! only on read. This keeps the database free of absolute/local paths so a fresh
//! deploy into any install directory (or a different machine) just works — where a
//! stored absolute path silently breaks ("file not found → empty/default").
//!
//! * write: build the category-relative suffix, `resolve_file` it for the FS/ML
//!   call, and store the suffix.
//! * read: `resolve_file(category_dir, stored)` — with a legacy-guard so a
//!   pre-backfill absolute row still resolves.
//! * boot: [`backfill_paths`] normalises any remaining absolute rows to relative.

use std::path::{Component, Path, PathBuf};

use crate::state::AppState;

/// Resolve a configured storage dir (possibly relative) to an absolute path.
/// Infallible: a `current_dir` failure falls back to the raw path.
pub fn resolve_dir(dir: &str) -> PathBuf {
    let base = Path::new(dir);
    if base.is_absolute() {
        base.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|c| c.join(base))
            .unwrap_or_else(|_| base.to_path_buf())
    }
}

/// Resolve a DB-stored path against its category dir. **Legacy-guard:** a `stored`
/// value that is already absolute (a pre-backfill row) is returned unchanged, so
/// reads keep working until the boot backfill normalises it.
pub fn resolve_file(category_dir: &str, stored: &str) -> PathBuf {
    let p = Path::new(stored);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        resolve_dir(category_dir).join(p)
    }
}

/// The category-relative suffix to STORE for an absolute file path: strip the
/// resolved category prefix, forward-slash normalised. If `abs` is not under the
/// resolved category (e.g. it was written at an older install location), recover
/// the tail after the category dir's final component; failing that, keep the file
/// name. Never returns an absolute path.
pub fn relativise(category_dir: &str, abs: &Path) -> String {
    let root = resolve_dir(category_dir);
    if let Ok(rel) = abs.strip_prefix(&root) {
        return norm(rel);
    }
    // Different root (older deploy): find the category dir's final component in the
    // path and take everything after it — recovers e.g. `<chat_id>/<id>.<kind>`.
    if let Some(marker) = root.file_name() {
        let comps: Vec<Component> = abs.components().collect();
        if let Some(pos) = comps.iter().rposition(|c| c.as_os_str() == marker) {
            let tail: PathBuf = comps[pos + 1..].iter().collect();
            if tail.as_os_str().is_empty() {
                // marker was the last component — nothing after it.
            } else {
                return norm(&tail);
            }
        }
    }
    // Last resort: the bare file name.
    abs.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| norm(abs))
}

/// Forward-slash relative string with no leading separators.
fn norm(p: &Path) -> String {
    p.to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches('/')
        .to_string()
}

// --- Boot-time backfill: absolute → relative for the seven path columns ---------

/// Normalise any absolute DB path values to category-relative on boot. Best-effort
/// and idempotent (already-relative rows are skipped); a per-table failure is logged
/// and never aborts boot. Generalises the earlier skills-only repoint to every
/// stored path column so the whole DB becomes install-location independent. (Skills
/// are normalised by the seeder to the constant `<id>` folder.)
pub async fn backfill_paths(state: &AppState) {
    let s = &state.boot.storage;
    let mut total = 0usize;
    macro_rules! run {
        ($name:literal, $f:expr) => {
            match $f.await {
                Ok(n) => total += n,
                Err(e) => tracing::warn!(table = $name, error = %e, "path backfill failed"),
            }
        };
    }
    run!("skills", bf_skills(state, &s.skills_dir));
    run!("generated_artefacts", bf_artefacts(state, &s.artefacts_dir));
    run!("kb_documents", bf_kb_documents(state, &s.documents_dir));
    run!("document_versions", bf_document_versions(state, &s.workspace_dir));
    run!("chat_attachments", bf_chat_attachments(state, &s.chat_attachments_dir));
    run!("message_attachments", bf_message_attachments(state, &s.message_attachments_dir));
    run!("branding_assets", bf_branding(state, &s.branding_dir));
    run!("exports", bf_exports(state, &s.exports_dir));
    run!("users", bf_avatars(state, &s.avatars_dir));
    if total > 0 {
        tracing::info!(rows = total, "normalised absolute DB paths to relative");
    }
}

/// Rewrite `path` to relative when it is absolute; `None` when already relative.
fn to_relative(dir: &str, path: &str) -> Option<String> {
    if Path::new(path).is_absolute() {
        Some(relativise(dir, Path::new(path)))
    } else {
        None // already relative — idempotent skip
    }
}

async fn bf_skills(state: &AppState, dir: &str) -> crate::error::Result<usize> {
    // Built-in skills are normalised to `<id>` by the seeder; this catches
    // user-authored skills whose disk_path was written absolute.
    let rows = sqlx::query!(r#"SELECT id, disk_path AS "disk_path!" FROM skills"#)
        .fetch_all(&state.pg)
        .await?;
    let mut n = 0;
    for r in rows {
        if let Some(rel) = to_relative(dir, &r.disk_path) {
            sqlx::query!("UPDATE skills SET disk_path = $2 WHERE id = $1", r.id, rel)
                .execute(&state.pg).await?;
            n += 1;
        }
    }
    Ok(n)
}

async fn bf_artefacts(state: &AppState, dir: &str) -> crate::error::Result<usize> {
    let rows = sqlx::query!(r#"SELECT id, disk_path AS "disk_path!" FROM generated_artefacts"#)
        .fetch_all(&state.pg)
        .await?;
    let mut n = 0;
    for r in rows {
        if let Some(rel) = to_relative(dir, &r.disk_path) {
            sqlx::query!("UPDATE generated_artefacts SET disk_path = $2 WHERE id = $1", r.id, rel)
                .execute(&state.pg).await?;
            n += 1;
        }
    }
    Ok(n)
}

async fn bf_kb_documents(state: &AppState, dir: &str) -> crate::error::Result<usize> {
    let rows = sqlx::query!(r#"SELECT id, bytes_path AS "bytes_path!" FROM kb_documents"#)
        .fetch_all(&state.pg)
        .await?;
    let mut n = 0;
    for r in rows {
        if let Some(rel) = to_relative(dir, &r.bytes_path) {
            sqlx::query!("UPDATE kb_documents SET bytes_path = $2 WHERE id = $1", r.id, rel)
                .execute(&state.pg).await?;
            n += 1;
        }
    }
    Ok(n)
}

async fn bf_document_versions(state: &AppState, dir: &str) -> crate::error::Result<usize> {
    let rows = sqlx::query!(r#"SELECT id, bytes_path AS "bytes_path!" FROM document_versions"#)
        .fetch_all(&state.pg)
        .await?;
    let mut n = 0;
    for r in rows {
        if let Some(rel) = to_relative(dir, &r.bytes_path) {
            sqlx::query!("UPDATE document_versions SET bytes_path = $2 WHERE id = $1", r.id, rel)
                .execute(&state.pg).await?;
            n += 1;
        }
    }
    Ok(n)
}

async fn bf_chat_attachments(state: &AppState, dir: &str) -> crate::error::Result<usize> {
    let rows = sqlx::query!(r#"SELECT id, disk_path AS "disk_path!" FROM chat_attachments"#)
        .fetch_all(&state.pg)
        .await?;
    let mut n = 0;
    for r in rows {
        if let Some(rel) = to_relative(dir, &r.disk_path) {
            sqlx::query!("UPDATE chat_attachments SET disk_path = $2 WHERE id = $1", r.id, rel)
                .execute(&state.pg).await?;
            n += 1;
        }
    }
    Ok(n)
}

async fn bf_message_attachments(state: &AppState, dir: &str) -> crate::error::Result<usize> {
    let rows = sqlx::query!(r#"SELECT id, disk_path AS "disk_path!" FROM message_attachments"#)
        .fetch_all(&state.pg)
        .await?;
    let mut n = 0;
    for r in rows {
        if let Some(rel) = to_relative(dir, &r.disk_path) {
            sqlx::query!("UPDATE message_attachments SET disk_path = $2 WHERE id = $1", r.id, rel)
                .execute(&state.pg).await?;
            n += 1;
        }
    }
    Ok(n)
}

async fn bf_branding(state: &AppState, dir: &str) -> crate::error::Result<usize> {
    let rows = sqlx::query!(r#"SELECT id, disk_path AS "disk_path!" FROM branding_assets"#)
        .fetch_all(&state.pg)
        .await?;
    let mut n = 0;
    for r in rows {
        if let Some(rel) = to_relative(dir, &r.disk_path) {
            sqlx::query!("UPDATE branding_assets SET disk_path = $2 WHERE id = $1", r.id, rel)
                .execute(&state.pg).await?;
            n += 1;
        }
    }
    Ok(n)
}

async fn bf_exports(state: &AppState, dir: &str) -> crate::error::Result<usize> {
    let rows = sqlx::query!("SELECT id, disk_path FROM exports WHERE disk_path IS NOT NULL")
        .fetch_all(&state.pg)
        .await?;
    let mut n = 0;
    for r in rows {
        if let Some(rel) = r.disk_path.as_deref().and_then(|p| to_relative(dir, p)) {
            sqlx::query!("UPDATE exports SET disk_path = $2 WHERE id = $1", r.id, rel)
                .execute(&state.pg).await?;
            n += 1;
        }
    }
    Ok(n)
}

async fn bf_avatars(state: &AppState, dir: &str) -> crate::error::Result<usize> {
    let rows = sqlx::query!("SELECT id, avatar_path FROM users WHERE avatar_path IS NOT NULL")
        .fetch_all(&state.pg)
        .await?;
    let mut n = 0;
    for r in rows {
        if let Some(rel) = r.avatar_path.as_deref().and_then(|p| to_relative(dir, p)) {
            sqlx::query!("UPDATE users SET avatar_path = $2 WHERE id = $1", r.id, rel)
                .execute(&state.pg).await?;
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_dir_absolute_passthrough_relative_joins_cwd() {
        // Absolute stays put.
        let abs = if cfg!(windows) { r"C:\srv\data\artefacts" } else { "/srv/data/artefacts" };
        assert_eq!(resolve_dir(abs), PathBuf::from(abs));
        // Relative is joined onto cwd.
        let r = resolve_dir("./data/artefacts");
        assert!(r.is_absolute());
        assert!(r.ends_with("data/artefacts"));
    }

    #[test]
    fn resolve_file_legacy_absolute_passthrough() {
        // A pre-backfill absolute row is used as-is (legacy-guard).
        let abs = if cfg!(windows) { r"C:\old\data\exports\e.pdf" } else { "/old/data/exports/e.pdf" };
        assert_eq!(resolve_file("./data/exports", abs), PathBuf::from(abs));
    }

    #[test]
    fn relocation_relative_resolves_under_any_root() {
        // THE point: a suffix stored under root A resolves under a DIFFERENT root B
        // (a different deploy / machine) — the file is found because the DB is relative.
        let (root_a, root_b) = if cfg!(windows) {
            (r"C:\deployA\data\artefacts", r"D:\deployB\data\artefacts")
        } else {
            ("/deployA/data/artefacts", "/deployB/data/artefacts")
        };
        let abs_a = Path::new(root_a).join("chat123").join("art.md");
        let rel = relativise(root_a, &abs_a);
        assert_eq!(rel, "chat123/art.md");
        assert!(!Path::new(&rel).is_absolute(), "stored value must be relative");
        let resolved_b = resolve_file(root_b, &rel);
        assert!(resolved_b.ends_with("chat123/art.md"));
        assert!(resolved_b.starts_with(root_b));
    }

    #[test]
    fn relocation_workspace_version_resolves_under_any_root() {
        // document_versions.bytes_path: `<doc_id>/<version_id>.<ext>` stored under
        // workspace root A must resolve under a DIFFERENT root B (the "other deploy").
        let (root_a, root_b) = if cfg!(windows) {
            (r"C:\deployA\data\workspace", r"D:\deployB\data\workspace")
        } else {
            ("/deployA/data/workspace", "/deployB/data/workspace")
        };
        let abs_a = Path::new(root_a).join("doc42").join("ver7.docx");
        let rel = relativise(root_a, &abs_a);
        assert_eq!(rel, "doc42/ver7.docx");
        assert!(!Path::new(&rel).is_absolute(), "stored value must be relative");
        let resolved_b = resolve_file(root_b, &rel);
        assert!(resolved_b.ends_with("doc42/ver7.docx"));
        assert!(resolved_b.starts_with(root_b));
    }

    #[test]
    fn relativise_recovers_tail_from_older_root() {
        // Path written at an older install root; category final component is "artefacts".
        let old = if cfg!(windows) {
            r"C:\srv\fosnie\backend\data\artefacts\chatX\a.pdf"
        } else {
            "/srv/fosnie/backend/data/artefacts/chatX/a.pdf"
        };
        // Current category dir resolves elsewhere, so strip_prefix fails → marker tail.
        assert_eq!(relativise("./data/artefacts", Path::new(old)), "chatX/a.pdf");
    }

    #[test]
    fn round_trip_relativise_resolve_relativise() {
        // relativise(absolute) → store → resolve_file → relativise again is stable.
        let cat = "./data/documents";
        let abs = resolve_dir(cat).join("doc9__report.pdf");
        let rel = relativise(cat, &abs);
        assert_eq!(rel, "doc9__report.pdf");
        let resolved = resolve_file(cat, &rel);
        assert_eq!(relativise(cat, &resolved), rel);
    }
}
