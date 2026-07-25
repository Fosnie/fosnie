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

//! The desktop client an installation hands to its own users.
//!
//! An organisation that will not let its people fetch software from the internet
//! can put the installer here instead: a super-admin uploads it once, everybody
//! downloads it from their own server, and the installed clients look here first
//! for updates. Nothing in that loop leaves the customer's network.
//!
//! One current version is kept, not a history — the installer, its update
//! signature and a small metadata sidecar, all under `storage.desktop_installer_dir`.
//! The sidecar rather than a table so that the bytes and the facts about them
//! restore together as a single directory copy, which is how an air-gapped
//! installation is actually moved.
//!
//! Two things here are less obvious than they look:
//!
//! * The update manifest is **synthesised**, never served back as uploaded. The
//!   manifest published on the public channel points at a public URL; handing
//!   that to a client on a closed network would send it straight off the network
//!   it is not allowed to leave. Only the version, notes and signature are taken
//!   from an uploaded manifest — the location is always this installation's own.
//! * These reads are deliberately **not** fenced to browser sessions. The updater
//!   in an installed client carries a device token and no cookie, and it is the
//!   principal these two endpoints exist for.

use axum::body::{Body, Bytes};
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::audit::{self, AuditEvent};
use crate::auth::breakglass::SuperAdmin;
use crate::auth::keycloak::AuthUser;
use crate::error::{AppError, Result};
use crate::state::AppState;

/// The metadata sidecar, written last so a half-finished upload never presents
/// itself as available.
const META_FILE: &str = "metadata.json";

/// The one platform published today. macOS artefacts are not distributed yet; a
/// second entry here is all that is needed when they are.
const PLATFORM_KEY: &str = "windows-x86_64";

/// Where a user is sent when this installation serves no installer of its own.
const DEFAULT_DOWNLOAD_URL: &str = "https://get.fosnie.dev/desktop/";

/// A generous ceiling on a filename, well past any real installer name.
const NAME_MAX: usize = 200;

/// What is being uploaded. Each kind accepts exactly one extension: this is a
/// deliberate allow-list rather than a reuse of the document-ingest one, which
/// rejects all three of these on purpose and must stay that way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// The installer itself.
    Installer,
    /// The detached update signature produced beside it.
    Signature,
    /// A published update manifest, read for its version, notes and signature.
    Manifest,
}

impl Kind {
    fn extension(self) -> &'static str {
        match self {
            Kind::Installer => "msi",
            Kind::Signature => "sig",
            Kind::Manifest => "json",
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Kind::Installer => "installer",
            Kind::Signature => "signature",
            Kind::Manifest => "manifest",
        }
    }
}

/// What is known about the installer this installation is currently serving.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstallerMeta {
    version: String,
    filename: String,
    size: u64,
    /// Lower-case hex. Shown to the admin because vetting pipelines check it.
    sha256: String,
    uploaded_at: String,
    /// The detached update signature, verbatim. Without it the manifest is
    /// unusable and is not offered at all.
    #[serde(default)]
    signature: Option<String>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    pub_date: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UploadQuery {
    pub filename: String,
    pub kind: Kind,
    /// The version these bytes belong to. A different version replaces whatever
    /// is stored: one current installer, never a library of them.
    pub version: String,
}

fn installer_dir(state: &AppState) -> PathBuf {
    crate::storage::resolve_dir(&state.boot.storage.desktop_installer_dir)
}

/// Reject a supplied filename outright rather than sanitise it. Sanitising
/// invites the question of what the sanitised form of `..%2f..%2fx` is; refusing
/// anything that is not a plain name does not.
fn safe_name(filename: &str) -> Result<String> {
    let name = filename.trim();
    if name.is_empty() || name.len() > NAME_MAX {
        return Err(AppError::Validation("filename is empty or too long".into()));
    }
    if name.contains(['/', '\\', ':', '\0']) || name == "." || name == ".." {
        return Err(AppError::Validation(
            "filename must be a plain name, without a path".into(),
        ));
    }
    if name.starts_with('.') {
        return Err(AppError::Validation("filename must not start with a dot".into()));
    }
    Ok(name.to_string())
}

/// The extension must match the kind, so a renamed executable cannot arrive as
/// an installer and a manifest cannot arrive as one either.
fn ensure_supported(kind: Kind, filename: &str) -> Result<()> {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some(e) if e == kind.extension() => Ok(()),
        _ => Err(AppError::Validation(format!(
            "a {} must be a .{} file",
            kind.as_str(),
            kind.extension()
        ))),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

async fn read_meta(dir: &Path) -> Option<InstallerMeta> {
    let raw = tokio::fs::read(dir.join(META_FILE)).await.ok()?;
    serde_json::from_slice(&raw).ok()
}

async fn write_meta(dir: &Path, meta: &InstallerMeta) -> Result<()> {
    let body = serde_json::to_vec_pretty(meta)
        .map_err(|e| AppError::Other(anyhow::anyhow!("encode installer metadata: {e}")))?;
    tokio::fs::write(dir.join(META_FILE), body)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("write installer metadata: {e}")))
}

/// Empty the directory. Used when the version changes and when an admin clears
/// the installer entirely.
async fn clear_dir(dir: &Path) -> Result<()> {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry.path().is_file() {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
    Ok(())
}

/// Where this installation is reached, from the request that got here. The
/// client resolved it a moment ago, so it is the address that demonstrably
/// works, which a configured value is not always.
fn request_origin(state: &AppState, headers: &HeaderMap) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .filter(|h| !h.is_empty());
    match host {
        Some(h) => {
            let scheme = headers
                .get("x-forwarded-proto")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.split(',').next().unwrap_or(s).trim().to_string())
                .unwrap_or_else(|| {
                    if state.boot.server.public_url.starts_with("https://") {
                        "https".into()
                    } else {
                        "http".into()
                    }
                });
            format!("{scheme}://{h}")
        }
        None => state.boot.server.public_url.trim_end_matches('/').to_string(),
    }
}

/// Build the update manifest a client reads. Its download location is always
/// this installation's own endpoint, whatever an uploaded manifest said.
fn manifest_for(meta: &InstallerMeta, origin: &str, signature: &str) -> serde_json::Value {
    json!({
        "version": meta.version,
        "notes": meta.notes.clone().unwrap_or_default(),
        "pub_date": meta.pub_date.clone().unwrap_or_else(|| meta.uploaded_at.clone()),
        "platforms": {
            PLATFORM_KEY: {
                "signature": signature,
                "url": format!("{}/api/desktop/installer", origin.trim_end_matches('/')),
            }
        }
    })
}

/// Pull the parts of a published manifest that are still true here: what the
/// release is and how it is signed. Its `url` is deliberately discarded.
fn harvest_manifest(bytes: &[u8]) -> Result<(Option<String>, Option<String>, Option<String>)> {
    let v: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| AppError::Validation("that file is not a valid update manifest".into()))?;
    let notes = v.get("notes").and_then(|n| n.as_str()).map(str::to_string);
    let pub_date = v.get("pub_date").and_then(|d| d.as_str()).map(str::to_string);
    let signature = v
        .get("platforms")
        .and_then(|p| p.get(PLATFORM_KEY))
        .and_then(|p| p.get("signature"))
        .and_then(|s| s.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Ok((notes, pub_date, signature))
}

async fn download_url(state: &AppState) -> String {
    crate::config::runtime::get(&state.pg, "desktop.download_url")
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_DOWNLOAD_URL.to_string())
}

// --- Super-admin: put an installer here, or take it away ----------------------

/// `POST /api/admin/desktop-installer` — upload the installer, its signature or a
/// published manifest. Raw bytes in the body; the body size is capped at the
/// route layer. Break-glass gated: this decides what software an entire estate
/// will be offered and then install.
pub async fn upload(
    State(state): State<AppState>,
    SuperAdmin(ctx): SuperAdmin,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> Result<Json<serde_json::Value>> {
    if body.is_empty() {
        return Err(AppError::Validation("empty upload".into()));
    }
    let version = q.version.trim().to_string();
    if version.is_empty() || version.len() > 64 {
        return Err(AppError::Validation("a version is required".into()));
    }
    let name = safe_name(&q.filename)?;
    ensure_supported(q.kind, &name)?;

    let dir = installer_dir(&state);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("create desktop installer dir: {e}")))?;

    // A different version supersedes what is stored: one current installer, so
    // the old bytes go rather than linger beside the new ones.
    let existing = read_meta(&dir).await;
    if existing.as_ref().is_some_and(|m| m.version != version) {
        clear_dir(&dir).await?;
    }
    let mut meta = match read_meta(&dir).await {
        Some(m) => m,
        None => InstallerMeta {
            version: version.clone(),
            filename: String::new(),
            size: 0,
            sha256: String::new(),
            uploaded_at: now_rfc3339(),
            signature: None,
            notes: None,
            pub_date: None,
        },
    };
    meta.version = version.clone();
    meta.uploaded_at = now_rfc3339();

    match q.kind {
        Kind::Installer => {
            // Written under a temporary name and renamed, so a download that
            // arrives mid-upload cannot be served a truncated installer.
            let part = dir.join(format!("{name}.part"));
            tokio::fs::write(&part, &body)
                .await
                .map_err(|e| AppError::Other(anyhow::anyhow!("write installer: {e}")))?;
            if !meta.filename.is_empty() && meta.filename != name {
                let _ = tokio::fs::remove_file(dir.join(&meta.filename)).await;
            }
            tokio::fs::rename(&part, dir.join(&name))
                .await
                .map_err(|e| AppError::Other(anyhow::anyhow!("store installer: {e}")))?;
            meta.filename = name.clone();
            meta.size = body.len() as u64;
            meta.sha256 = sha256_hex(&body);
        }
        Kind::Signature => {
            let text = String::from_utf8_lossy(&body).trim().to_string();
            if text.is_empty() {
                return Err(AppError::Validation("that signature file is empty".into()));
            }
            tokio::fs::write(dir.join(&name), &body)
                .await
                .map_err(|e| AppError::Other(anyhow::anyhow!("write signature: {e}")))?;
            meta.signature = Some(text);
        }
        Kind::Manifest => {
            let (notes, pub_date, signature) = harvest_manifest(&body)?;
            meta.notes = notes;
            meta.pub_date = pub_date;
            // A signature uploaded on its own is the more direct statement of
            // the two, so it wins.
            if meta.signature.is_none() {
                meta.signature = signature;
            }
        }
    }

    write_meta(&dir, &meta).await?;

    let mut ev = AuditEvent::action("desktop.installer_uploaded", ctx.role.as_str());
    ev.resource_type = Some("desktop_installer".into());
    ev.payload = Some(json!({
        "kind": q.kind.as_str(),
        "version": meta.version,
        "filename": name,
        "sha256": meta.sha256,
    }));
    let _ = audit::append(&state.pg, &ev).await;

    Ok(Json(meta_json(Some(&meta), &download_url(&state).await)))
}

/// `GET /api/admin/desktop-installer` — the same facts as the user-facing meta
/// route, for the panel that uploads them. Its own route because the super-admin
/// panel holds a break-glass grant and no session, so it cannot read the other one.
pub async fn admin_meta(
    State(state): State<AppState>,
    SuperAdmin(_ctx): SuperAdmin,
) -> Result<Json<serde_json::Value>> {
    let stored = read_meta(&installer_dir(&state)).await;
    Ok(Json(meta_json(stored.as_ref(), &download_url(&state).await)))
}

/// `DELETE /api/admin/desktop-installer` — stop serving an installer from here.
/// Clients fall back to the public channel on their next check.
pub async fn clear(
    State(state): State<AppState>,
    SuperAdmin(ctx): SuperAdmin,
) -> Result<StatusCode> {
    let dir = installer_dir(&state);
    let had = read_meta(&dir).await;
    clear_dir(&dir).await?;

    if let Some(m) = had {
        let mut ev = AuditEvent::action("desktop.installer_removed", ctx.role.as_str());
        ev.resource_type = Some("desktop_installer".into());
        ev.payload = Some(json!({ "version": m.version, "sha256": m.sha256 }));
        let _ = audit::append(&state.pg, &ev).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

// --- Anyone signed in: get the client, and keep it up to date -----------------

fn meta_json(meta: Option<&InstallerMeta>, download_url: &str) -> serde_json::Value {
    match meta.filter(|m| !m.filename.is_empty()) {
        Some(m) => json!({
            "available": true,
            "version": m.version,
            "filename": m.filename,
            "size": m.size,
            "sha256": m.sha256,
            "uploaded_at": m.uploaded_at,
            "has_signature": m.signature.is_some(),
            "download_url": download_url,
        }),
        None => json!({
            "available": false,
            "version": null,
            "filename": null,
            "size": null,
            "sha256": null,
            "uploaded_at": null,
            "has_signature": false,
            "download_url": download_url,
        }),
    }
}

/// `GET /api/desktop/installer/meta` — what this installation has, for the
/// profile page and the admin panel. Always answers; `available` says the rest.
pub async fn meta(
    State(state): State<AppState>,
    AuthUser(_ctx): AuthUser,
) -> Result<Json<serde_json::Value>> {
    let stored = read_meta(&installer_dir(&state)).await;
    Ok(Json(meta_json(stored.as_ref(), &download_url(&state).await)))
}

/// `GET /api/desktop/installer` — the installer bytes. Reached both by a browser
/// on the profile page and by an installed client applying an update, so it must
/// accept a device token as readily as a session.
pub async fn download(
    State(state): State<AppState>,
    AuthUser(_ctx): AuthUser,
) -> Result<Response> {
    let dir = installer_dir(&state);
    let meta = read_meta(&dir)
        .await
        .filter(|m| !m.filename.is_empty())
        .ok_or_else(|| AppError::NotFound("no desktop installer is published here".into()))?;

    let path = dir.join(&meta.filename);
    let safe = crate::upload::ensure_within_storage(
        &state.boot.storage.desktop_installer_dir,
        &path.to_string_lossy(),
    )?;
    let bytes = tokio::fs::read(&safe)
        .await
        .map_err(|_| AppError::NotFound("no desktop installer is published here".into()))?;

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", meta.filename),
            ),
        ],
        Body::from(bytes),
    )
        .into_response())
}

/// `GET /api/desktop/latest.json` — the update manifest for clients paired with
/// this installation. A 404 is a normal answer: the client then asks the public
/// channel instead. That is why an unsigned installer produces one rather than a
/// manifest no client could verify.
pub async fn latest_manifest(
    State(state): State<AppState>,
    AuthUser(_ctx): AuthUser,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let stored = read_meta(&installer_dir(&state))
        .await
        .filter(|m| !m.filename.is_empty())
        .ok_or_else(|| AppError::NotFound("no desktop update is published here".into()))?;
    let signature = stored
        .signature
        .clone()
        .ok_or_else(|| AppError::NotFound("no signed desktop update is published here".into()))?;

    let origin = request_origin(&state, &headers);
    Ok(Json(manifest_for(&stored, &origin, &signature)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta_fixture() -> InstallerMeta {
        InstallerMeta {
            version: "0.1.1".into(),
            filename: "Fosnie_0.1.1_x64_en-US.msi".into(),
            size: 42,
            sha256: "abc".into(),
            uploaded_at: "2026-07-25T10:00:00Z".into(),
            signature: Some("sig".into()),
            notes: Some("What changed.".into()),
            pub_date: Some("2026-07-24T09:00:00Z".into()),
        }
    }

    #[test]
    fn extension_must_match_the_kind() {
        assert!(ensure_supported(Kind::Installer, "Fosnie_0.1.1_x64_en-US.msi").is_ok());
        assert!(ensure_supported(Kind::Installer, "Fosnie.MSI").is_ok());
        assert!(ensure_supported(Kind::Signature, "Fosnie.msi.sig").is_ok());
        assert!(ensure_supported(Kind::Manifest, "latest.json").is_ok());

        // Cross-kind and everything else is refused.
        assert!(ensure_supported(Kind::Installer, "latest.json").is_err());
        assert!(ensure_supported(Kind::Manifest, "Fosnie.msi").is_err());
        assert!(ensure_supported(Kind::Installer, "payload.exe").is_err());
        assert!(ensure_supported(Kind::Installer, "bundle.zip").is_err());
        assert!(ensure_supported(Kind::Installer, "installer").is_err());
    }

    #[test]
    fn a_filename_is_a_plain_name_or_it_is_refused() {
        assert_eq!(safe_name(" Fosnie.msi ").unwrap(), "Fosnie.msi");

        for bad in [
            "",
            "..",
            ".",
            "../escape.msi",
            "a/b.msi",
            "a\\b.msi",
            "C:\\x.msi",
            ".hidden.msi",
        ] {
            assert!(safe_name(bad).is_err(), "{bad:?} should be refused");
        }
        assert!(safe_name(&format!("{}.msi", "x".repeat(NAME_MAX))).is_err());
    }

    #[test]
    fn the_manifest_points_at_this_installation_not_the_public_channel() {
        let m = manifest_for(&meta_fixture(), "https://ai.example.org/", "SIGNATURE");
        assert_eq!(m["version"], "0.1.1");
        assert_eq!(m["notes"], "What changed.");
        assert_eq!(m["pub_date"], "2026-07-24T09:00:00Z");
        assert_eq!(m["platforms"][PLATFORM_KEY]["signature"], "SIGNATURE");
        assert_eq!(
            m["platforms"][PLATFORM_KEY]["url"],
            "https://ai.example.org/api/desktop/installer"
        );
    }

    #[test]
    fn an_uploaded_manifest_gives_up_its_version_facts_but_not_its_location() {
        let raw = br#"{
            "version": "0.1.1",
            "notes": "Two fixes.",
            "pub_date": "2026-07-24T09:00:00Z",
            "platforms": { "windows-x86_64": {
                "signature": " SIG ",
                "url": "https://get.fosnie.dev/desktop/Fosnie_0.1.1_x64_en-US.msi"
            }}
        }"#;
        let (notes, pub_date, signature) = harvest_manifest(raw).unwrap();
        assert_eq!(notes.as_deref(), Some("Two fixes."));
        assert_eq!(pub_date.as_deref(), Some("2026-07-24T09:00:00Z"));
        assert_eq!(signature.as_deref(), Some("SIG"));

        assert!(harvest_manifest(b"not json").is_err());
    }

    #[test]
    fn nothing_uploaded_is_reported_as_nothing_rather_than_an_error() {
        let j = meta_json(None, DEFAULT_DOWNLOAD_URL);
        assert_eq!(j["available"], false);
        assert_eq!(j["has_signature"], false);
        assert_eq!(j["download_url"], DEFAULT_DOWNLOAD_URL);

        let m = meta_fixture();
        let j = meta_json(Some(&m), DEFAULT_DOWNLOAD_URL);
        assert_eq!(j["available"], true);
        assert_eq!(j["version"], "0.1.1");
        assert_eq!(j["has_signature"], true);
    }
}
