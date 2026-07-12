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

//! Shared upload-format guard. Document and chat-attachment uploads are gated to the
//! formats the ML extractor (`ml/app/extract.py`) can ingest — native document
//! parsers plus images (OCR). Kept in sync with the frontend allow-list
//! (`frontend/src/lib/files.ts`).

use crate::error::{AppError, Result};

/// Filename extensions the ML extractor can read (documents + OCR'd images).
pub const ALLOWED_DOC_EXT: &[&str] = &[
    "pdf", "docx", "xlsx", "xlsm", "pptx", "txt", "md", "text", "png", "jpg", "jpeg", "webp",
    "bmp", "tif", "tiff", "gif",
];

fn extension(filename: &str) -> String {
    filename
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default()
}

/// Reject an upload whose filename extension isn't an ingestible format. The
/// client validates too (immediate feedback); this is the authoritative gate.
pub fn ensure_supported_document(filename: &str) -> Result<()> {
    let ext = extension(filename);
    if ALLOWED_DOC_EXT.contains(&ext.as_str()) {
        Ok(())
    } else {
        let shown = if ext.is_empty() { "(none)".to_string() } else { format!(".{ext}") };
        Err(AppError::Validation(format!(
            "unsupported file type \"{shown}\" — allowed: pdf, docx, xlsx, pptx, txt, md, and images"
        )))
    }
}

/// Defence-in-depth before serving a DB-stored file: canonicalise the path and the
/// configured storage `root`, and refuse (403) when the file resolves OUTSIDE the
/// root — so a poisoned path (e.g. one written via some other vuln) cannot escape
/// the storage tree. Mirrors the skill-subfile guard in `tools::skill_subfile`.
/// Returns the canonical path to read. (Sync `canonicalize` is a single stat —
/// cheap enough to call inline from an async download handler.)
pub fn ensure_within_storage(root: &str, path: &str) -> Result<std::path::PathBuf> {
    let canon_root = std::fs::canonicalize(root)
        .map_err(|e| AppError::Other(anyhow::anyhow!("storage root '{root}': {e}")))?;
    let canon_file = std::fs::canonicalize(path)
        .map_err(|e| AppError::Validation(format!("file not found: {e}")))?;
    if !canon_file.starts_with(&canon_root) {
        return Err(AppError::Forbidden("requested path escapes the storage root".into()));
    }
    Ok(canon_file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_known_doc_and_image_types() {
        for f in ["report.pdf", "Memo.DOCX", "sheet.xlsx", "a.txt", "notes.md", "scan.png", "x.JPEG"] {
            assert!(ensure_supported_document(f).is_ok(), "{f} should be allowed");
        }
    }

    #[test]
    fn rejects_unknown_and_extensionless() {
        for f in ["malware.exe", "archive.zip", "data.bin", "noext"] {
            assert!(ensure_supported_document(f).is_err(), "{f} should be rejected");
        }
    }

    #[test]
    fn ensure_within_storage_allows_inside_rejects_escape() {
        use std::fs;
        let root = std::env::temp_dir().join(format!("pai-store-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let inside = root.join("ok.txt");
        fs::write(&inside, b"x").unwrap();
        // A file genuinely inside the root resolves and is accepted.
        assert!(ensure_within_storage(root.to_str().unwrap(), inside.to_str().unwrap()).is_ok());
        // A file outside the root (in its parent) is rejected as an escape.
        let outside = std::env::temp_dir().join(format!("pai-escape-{}.txt", std::process::id()));
        fs::write(&outside, b"x").unwrap();
        assert!(ensure_within_storage(root.to_str().unwrap(), outside.to_str().unwrap()).is_err());
        let _ = fs::remove_file(&inside);
        let _ = fs::remove_file(&outside);
        let _ = fs::remove_dir(&root);
    }
}
