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

//! Boot-time seed of the platform's built-in **default** skill *library*.
//!
//! A *default* skill (`skills.is_default = true`) is applied to every Agent —
//! existing and future — without an `agent_skills` binding (see
//! `chat::load_skills`). The library is the checked-in `skills/<slug>/` tree
//! (`storage.skills_library_dir`), shipped with the release. At boot the seeder
//! walks it, parses each `SKILL.md` frontmatter, and reconciles it with the DB +
//! the runtime skill store (`storage.skills_dir`, where `read_skill` reads from).
//!
//! **Edit-preserving updates.** Each library skill carries a `source_hash` (over
//! the files we last wrote). On boot:
//!   * no row              → create (copy files + insert).
//!   * `source_hash` equal → up to date; nothing to do.
//!   * `source_hash` differs (a shipped upgrade, or the legacy pre-library seed
//!     whose hash is NULL) → only overwrite if the *runtime* copy still matches
//!     what we last wrote (i.e. the client has not edited it in Studio). A
//!     locally-edited skill is left untouched and the update logged.
//!
//! Best-effort: a per-skill failure is logged and never aborts boot — agents
//! simply lack that default skill until it is fixed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{AppError, Result};
use crate::state::AppState;

/// UUIDv5 namespace for library-skill ids — unmapped slugs derive a stable id from
/// it so that dropping a new `skills/<slug>/` dir "just works". The Phase-1 skills
/// are pinned below for continuity (`docx-report` keeps the legacy seed's id).
const SKILL_NS: Uuid = Uuid::from_u128(0x5c11_1000_0000_4000_8000_0000_0000_00ff);

/// Stable id for a library skill. Pinned for the built-ins (continuity with the
/// pre-library seed and with any `agent_skills` rows); derived for the rest.
fn skill_id(slug: &str) -> Uuid {
    match slug {
        // `docx-report` inherits the id of the retired "Document drafting" seed.
        "docx-report" => Uuid::from_u128(0x5c11_1000_0000_4000_8000_0000_0000_0001),
        "pdf-report" => Uuid::from_u128(0x5c11_1000_0000_4000_8000_0000_0000_0002),
        "doc-tables" => Uuid::from_u128(0x5c11_1000_0000_4000_8000_0000_0000_0003),
        "dashboard" => Uuid::from_u128(0x5c11_1000_0000_4000_8000_0000_0000_0004),
        "report-to-page" => Uuid::from_u128(0x5c11_1000_0000_4000_8000_0000_0000_0005),
        "web-frontend" => Uuid::from_u128(0x5c11_1000_0000_4000_8000_0000_0000_0006),
        "research-methods" => Uuid::from_u128(0x5c11_1000_0000_4000_8000_0000_0000_0007),
        "xlsx-tables" => Uuid::from_u128(0x5c11_1000_0000_4000_8000_0000_0000_0008),
        other => Uuid::new_v5(&SKILL_NS, other.as_bytes()),
    }
}

// --- The legacy pre-library seed --------------------------------------------
// The single "Document drafting" skill that `skills_seed` wrote before the
// library existed. Kept only so we can recognise an *unedited* legacy install and
// safely upgrade it to `docx-report`; an install that edited it is preserved.
const LEGACY_DOC_NAME: &str = "Document drafting";
const LEGACY_DOC_DESC: &str =
    "How to draft a clean downloadable document (PDF, Word, or Markdown): a real \
     title, plain prose, no Markdown symbols, no placeholders, no meta-commentary.";
const LEGACY_DOC_BODY: &str = "\
Use this skill whenever the user asks you to produce a downloadable document — a \
PDF, a Word (DOCX) document, or a Markdown file. Your reply is captured verbatim as \
the file, so write **only the finished document** — nothing else.

Rules:

- Begin with a single concise title line in Title Case that names the document (for \
  example: Confidentiality Memo — NDA Clauses). Do not prefix it with \"#\", do not \
  make it bold, and do not write \"Subject:\" or \"Title:\".
- After the title, leave a blank line, then write the body as plain prose paragraphs \
  separated by blank lines. For a list, put each item on its own line beginning with \
  \"- \".
- Do not use any Markdown symbols (no **, no *, no #, no backticks, no tables). They \
  are rendered literally in the document, not as formatting.
- Never leave bracketed placeholders such as [Insert Date], [Client Name] or [Insert \
  Document ID]. Use concrete values where you can, or omit the line entirely. If a \
  real value is genuinely unknown, write it in prose (for example \"the date of \
  signing\") rather than a bracket.
- Do not refer to the document itself or to the act of producing it. No \"Generated \
  Artefact\", no \"please find attached\", no \"the PDF below\", no notes about \
  formatting or confirmation.
- Do not add conversational preamble or a sign-off unless the document type genuinely \
  needs one (for example, a letter has a salutation and a closing).
- Use British English throughout, in a clear, professional register.";

/// The exact `SKILL.md` bytes the legacy seeder wrote (frontmatter + body).
fn legacy_skill_md() -> String {
    format!("---\nname: {LEGACY_DOC_NAME}\ndescription: {LEGACY_DOC_DESC}\n---\n\n{LEGACY_DOC_BODY}\n")
}

/// Walk the in-repo skill library and reconcile it into the DB + runtime store.
/// Idempotent and edit-preserving (see module docs). Best-effort.
pub async fn ensure_default_skills(state: &AppState) -> Result<()> {
    let lib = match resolve_library_dir(&state.boot.storage.skills_library_dir) {
        Some(d) => d,
        None => {
            tracing::warn!(
                configured = %state.boot.storage.skills_library_dir,
                "skill library directory not found — no default skills seeded"
            );
            return Ok(());
        }
    };
    let runtime_root = resolve_runtime_root(&state.boot.storage.skills_dir)?;

    let rd = std::fs::read_dir(&lib)
        .map_err(|e| AppError::Other(anyhow::anyhow!("read skill library {lib:?}: {e}")))?;
    for entry in rd {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let dir = entry.path();
        if !dir.is_dir() || !dir.join("SKILL.md").is_file() {
            continue;
        }
        let slug = entry.file_name().to_string_lossy().to_string();
        if let Err(e) = seed_one(state, &slug, &dir, &runtime_root).await {
            tracing::warn!(skill = %slug, error = %e, "seeding skill failed");
        }
    }
    Ok(())
}

async fn seed_one(state: &AppState, slug: &str, lib_dir: &Path, runtime_root: &Path) -> Result<()> {
    let md = std::fs::read_to_string(lib_dir.join("SKILL.md"))
        .map_err(|e| AppError::Other(anyhow::anyhow!("read SKILL.md: {e}")))?;
    let fm = parse_frontmatter(&md);
    let name = fm
        .get("name")
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::Validation(format!("skill '{slug}' has no `name`")))?;
    let description = fm
        .get("description")
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::Validation(format!("skill '{slug}' has no `description`")))?;
    let is_default = fm.get("default").map(|v| parse_bool(v)).unwrap_or(false);

    let id = skill_id(slug);
    let lib_hash = hash_dir(lib_dir)
        .map_err(|e| AppError::Other(anyhow::anyhow!("hash skill library dir: {e}")))?;
    // The DB stores a RELATIVE folder name (`<id>`) under `skills_dir`; the runtime
    // dir is the resolved absolute path we read/write on disk. `<id>` is constant,
    // so the earlier cwd/path-drift class of bug cannot recur.
    let rel = id.to_string();
    let rt_dir = runtime_root.join(&rel);
    let rt_abs = rt_dir.to_string_lossy().to_string();

    let row = sqlx::query!(
        r#"SELECT source_hash, disk_path FROM skills WHERE id = $1"#,
        id
    )
    .fetch_optional(&state.pg)
    .await?;

    match row {
        None => {
            write_runtime(lib_dir, &rt_dir)?;
            sqlx::query!(
                "INSERT INTO skills (id, name, description, disk_path, scope, created_by, is_default, slug, source_hash) \
                 VALUES ($1, $2, $3, $4, 'global', NULL, $5, $6, $7) ON CONFLICT (id) DO NOTHING",
                id, name, description, rel, is_default, slug, lib_hash,
            )
            .execute(&state.pg)
            .await?;
            tracing::info!(skill = %slug, %id, "seeded default skill");
        }
        Some(r) => {
            let hash_current = r.source_hash.as_deref() == Some(lib_hash.as_str());
            let unedited = runtime_unedited(&rt_dir, r.source_hash.as_deref(), slug);
            // Authoritative signal: is a non-empty body actually readable where the
            // reader looks (the resolved runtime dir)?
            let current_empty = read_skill_body_or_empty(&rt_abs).await.trim().is_empty();

            if current_empty || (!hash_current && unedited) {
                // Missing/empty body is never an intentional edit → force-restore;
                // an unedited shipped-hash change is a normal upgrade. Both re-copy
                // the library and normalise disk_path to the relative `<id>`.
                write_runtime(lib_dir, &rt_dir)?;
                update_skill_row(state, id, name, description, &rel, is_default, slug, &lib_hash).await?;
                tracing::info!(skill = %slug, %id, "materialised default skill (restore/upgrade)");
            } else if r.disk_path != rel {
                // Body is fine; only the stored path is stale/absolute (legacy). Just
                // normalise it to the relative `<id>` — preserves any local edit.
                sqlx::query!("UPDATE skills SET disk_path = $2 WHERE id = $1", id, rel)
                    .execute(&state.pg)
                    .await?;
                tracing::info!(skill = %slug, %id, "normalised skill disk_path to relative id");
            } else if !hash_current {
                tracing::info!(skill = %slug, %id, "skill locally edited — shipped update skipped");
            }
        }
    }
    Ok(())
}

/// Whether the runtime copy still matches what we last wrote (so a shipped update
/// is safe). `stored_hash` is the DB `source_hash`; NULL means the legacy seed,
/// recognised by its known bytes. A missing runtime dir counts as unedited (we
/// simply re-materialise it).
fn runtime_unedited(rt_dir: &Path, stored_hash: Option<&str>, slug: &str) -> bool {
    if !rt_dir.join("SKILL.md").is_file() {
        return true;
    }
    let rt_hash = match hash_dir(rt_dir) {
        Ok(h) => h,
        Err(_) => return false, // unreadable → do not clobber
    };
    match stored_hash {
        Some(s) => rt_hash == s,
        // Legacy NULL: only `docx-report` inherits a pre-library install, and only
        // if its on-disk bytes are exactly the legacy seed (i.e. never edited).
        None => slug == "docx-report" && rt_hash == hash_one("SKILL.md", legacy_skill_md().as_bytes()),
    }
}

/// Body under `disk_path/SKILL.md` (frontmatter stripped) via the same reader the
/// `get_skill` endpoint uses, or `""` if the file is missing/unreadable.
async fn read_skill_body_or_empty(disk_path: &str) -> String {
    crate::http::skills::read_skill_body(disk_path)
        .await
        .unwrap_or_default()
}

/// Update a built-in skill's row to the shipped library metadata + hash.
async fn update_skill_row(
    state: &AppState,
    id: Uuid,
    name: &str,
    description: &str,
    disk_path: &str,
    is_default: bool,
    slug: &str,
    source_hash: &str,
) -> Result<()> {
    sqlx::query!(
        "UPDATE skills SET name = $2, description = $3, disk_path = $4, \
         is_default = $5, slug = $6, source_hash = $7 WHERE id = $1",
        id, name, description, disk_path, is_default, slug, source_hash,
    )
    .execute(&state.pg)
    .await?;
    Ok(())
}

// --- Filesystem + hashing helpers -------------------------------------------

/// Resolve the read-only library dir, trying the configured value then the usual
/// repo-relative fallbacks (works from the repo root or from `backend/`).
fn resolve_library_dir(configured: &str) -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok();
    let abs = |p: &str| -> PathBuf {
        let base = Path::new(p);
        if base.is_absolute() {
            base.to_path_buf()
        } else if let Some(c) = &cwd {
            c.join(base)
        } else {
            base.to_path_buf()
        }
    };
    for cand in [configured, "./skills", "../skills"] {
        let dir = abs(cand);
        if has_any_skill(&dir) {
            return Some(dir);
        }
    }
    None
}

/// True if `dir` holds at least one `<slug>/SKILL.md`.
fn has_any_skill(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() && p.join("SKILL.md").is_file() {
            return true;
        }
    }
    false
}

/// The runtime skill store root (relative → cwd-joined, matching `http::skills`).
fn resolve_runtime_root(skills_dir: &str) -> Result<PathBuf> {
    Ok(crate::storage::resolve_dir(skills_dir))
}

/// Replace `dst` with a fresh copy of `src` (so a shrunk shipped file set leaves no
/// stale files behind).
fn write_runtime(src: &Path, dst: &Path) -> Result<()> {
    let _ = std::fs::remove_dir_all(dst);
    copy_tree(src, dst).map_err(|e| AppError::Other(anyhow::anyhow!("copy skill files: {e}")))
}

fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Stable content hash of every file under `dir` (relative path + bytes), with
/// forward-slash relative paths so the hash is identical across platforms.
fn hash_dir(dir: &Path) -> std::io::Result<String> {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    collect_files(dir, dir, &mut files)?;
    Ok(hash_files(&files))
}

fn collect_files(root: &Path, cur: &Path, out: &mut Vec<(String, Vec<u8>)>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(cur)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_dir() {
            collect_files(root, &p, out)?;
        } else {
            let rel = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, std::fs::read(&p)?));
        }
    }
    Ok(())
}

fn hash_files(files: &[(String, Vec<u8>)]) -> String {
    let mut sorted: Vec<&(String, Vec<u8>)> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut h = Sha256::new();
    for (rel, bytes) in sorted {
        h.update(rel.as_bytes());
        h.update([0u8]);
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    }
    hex::encode(h.finalize())
}

fn hash_one(rel: &str, bytes: &[u8]) -> String {
    hash_files(&[(rel.to_string(), bytes.to_vec())])
}

// --- Frontmatter -------------------------------------------------------------

/// Minimal YAML-frontmatter reader: top-level `key: value` scalars only (the
/// codebase carries no YAML dependency, and the fields we need are flat).
fn parse_frontmatter(md: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    // Normalise CRLF→LF so a Windows-authored SKILL.md still yields its keys.
    let normalised = md.replace("\r\n", "\n");
    let s = normalised.trim_start();
    let Some(rest) = s.strip_prefix("---") else {
        return out;
    };
    let Some(end) = rest.find("\n---") else {
        return out;
    };
    for line in rest[..end].lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim();
            if k.is_empty() {
                continue;
            }
            let mut v = v.trim();
            if v.len() >= 2
                && ((v.starts_with('"') && v.ends_with('"'))
                    || (v.starts_with('\'') && v.ends_with('\'')))
            {
                v = &v[1..v.len() - 1];
            }
            out.insert(k.to_string(), v.to_string());
        }
    }
    out
}

fn parse_bool(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "yes" | "1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_parses_scalars_and_quotes() {
        let md = "---\nname: DOCX documents\ndescription: \"Make a doc: nicely\"\ndefault: true\n---\n\nbody here\n";
        let fm = parse_frontmatter(md);
        assert_eq!(fm.get("name").unwrap(), "DOCX documents");
        // Value may itself contain a colon — only the first one splits.
        assert_eq!(fm.get("description").unwrap(), "Make a doc: nicely");
        assert!(parse_bool(fm.get("default").unwrap()));
    }

    #[test]
    fn frontmatter_absent_is_empty() {
        assert!(parse_frontmatter("no frontmatter here").is_empty());
    }

    #[test]
    fn pinned_ids_are_stable() {
        assert_eq!(
            skill_id("docx-report").to_string(),
            "5c111000-0000-4000-8000-000000000001"
        );
        assert_eq!(
            skill_id("dashboard").to_string(),
            "5c111000-0000-4000-8000-000000000004"
        );
        assert_eq!(
            skill_id("report-to-page").to_string(),
            "5c111000-0000-4000-8000-000000000005"
        );
        // All pinned ids are distinct.
        let pinned: Vec<_> = [
            "docx-report", "pdf-report", "doc-tables", "dashboard", "report-to-page",
            "web-frontend", "research-methods", "xlsx-tables",
        ]
        .iter()
        .map(|s| skill_id(s))
        .collect();
        let mut uniq = pinned.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), pinned.len(), "pinned skill ids must be unique");
        // Unmapped slugs derive a deterministic v5 id (stable across runs).
        assert_eq!(skill_id("web-frontend"), skill_id("web-frontend"));
        assert_ne!(skill_id("web-frontend"), skill_id("research-methods"));
    }

    #[test]
    fn hash_is_order_independent_and_path_stable() {
        let a = vec![
            ("SKILL.md".to_string(), b"hello".to_vec()),
            ("references/x.md".to_string(), b"world".to_vec()),
        ];
        let mut b = a.clone();
        b.reverse();
        assert_eq!(hash_files(&a), hash_files(&b));
        // A content change moves the hash.
        let c = vec![("SKILL.md".to_string(), b"hELLO".to_vec())];
        assert_ne!(hash_files(&c), hash_one("SKILL.md", b"hello"));
    }

    #[test]
    fn edit_detection_via_hash() {
        // Unedited runtime (hash matches stored) is upgradeable; an edit is not.
        let stored = hash_one("SKILL.md", b"shipped v1");
        assert_ne!(stored, hash_one("SKILL.md", b"client edited"));
    }

    #[test]
    fn frontmatter_parses_crlf() {
        // Regression: a CRLF SKILL.md must still yield its keys (was empty before).
        let md = "---\r\nname: DOCX\r\ndescription: make a doc\r\n---\r\n\r\nbody\r\n";
        let fm = parse_frontmatter(md);
        assert_eq!(fm.get("name").unwrap(), "DOCX");
        assert_eq!(fm.get("description").unwrap(), "make a doc");
    }

    /// Materialise the real dashboard library skill into a temp runtime dir and read
    /// it back through the same reader `get_skill` uses — proves the copy→read chain
    /// delivers a non-empty body containing the ECharts marker. Skips if the library
    /// tree isn't reachable from the test cwd.
    #[tokio::test]
    async fn write_runtime_then_read_delivers_body_with_marker() {
        let lib = Path::new("../skills/dashboard");
        if !lib.join("SKILL.md").is_file() {
            eprintln!("skipping: {lib:?}/SKILL.md not found from cwd");
            return;
        }
        let dst = std::env::temp_dir().join(format!("pai-skill-test-{}", Uuid::new_v4()));
        write_runtime(lib, &dst).expect("materialise runtime copy");

        let body = crate::http::skills::read_skill_body(&dst.to_string_lossy())
            .await
            .expect("read materialised body");
        let _ = std::fs::remove_dir_all(&dst);

        assert!(!body.trim().is_empty(), "dashboard body must not be empty");
        assert!(body.contains("pai:echarts"), "dashboard body must carry the ECharts marker");
    }
}
