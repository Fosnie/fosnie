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

//! Symmetric at-rest encryption for sensitive fields (direct messages, provider
//! keys, connector tokens, …). AES-256-GCM with a random 96-bit nonce; the stored
//! form is `base64(nonce ‖ ciphertext+tag)`. The key is server-held, so the server
//! can still decrypt for rendering — this protects DB dumps / backups, not against
//! the server itself (the trust perimeter on a single-tenant deployment).
//!
//! ## Key management (BYOK / rotation)
//!
//! The data-encryption key (DEK) is resolved at boot by a [`crate::ext::KeyProvider`]
//! seam: the Core default reads it from config (`env-file`); a private
//! `fosnie-enterprise` crate can unwrap it from an HSM (`pkcs11`). The resolved DEKs
//! are held in a process-global [`Keyring`] (one **active** DEK for new writes plus
//! any **retired** DEKs still needed to read not-yet-re-encrypted rows).
//!
//! **Versioned ciphertext.** So a deployment can rotate its DEK, at-rest values may
//! carry a key-id frame `k<id>:base64(nonce‖ct)`. The legacy format (no frame) is
//! read as the reserved id [`LEGACY_KEY_ID`]; while the active id is that same
//! sentinel, new writes stay frameless, so a default (unrotated) deployment is
//! **byte-identical** to the pre-BYOK behaviour. A `:` never appears in base64, so a
//! frame is unambiguous against any legacy value. All at-rest call sites go through
//! [`encrypt_at_rest`] / [`decrypt_at_rest`], which consult the global keyring.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use aes_gcm::aead::{Aead, AeadCore, OsRng};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;

use crate::error::{AppError, Result};

const NONCE_LEN: usize = 12;

/// The reserved key-id for legacy (frameless) ciphertext. While the active DEK
/// carries this id, new writes are frameless → byte-identical to pre-BYOK output.
pub const LEGACY_KEY_ID: &str = "1";

/// Parse a base64-encoded 32-byte key from config. `None` when empty/invalid —
/// the caller treats that as "encryption disabled" (dev default).
pub fn parse_key(b64: &str) -> Option<[u8; 32]> {
    let t = b64.trim();
    if t.is_empty() {
        return None;
    }
    let bytes = STANDARD.decode(t).ok()?;
    bytes.try_into().ok()
}

/// AES-256-GCM encrypt → `base64(nonce ‖ ciphertext)`.
pub fn encrypt(key: &[u8; 32], plaintext: &str) -> Result<String> {
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| AppError::Other(anyhow::anyhow!("bad message key")))?;
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ct = cipher
        .encrypt(&nonce, plaintext.as_bytes())
        .map_err(|_| AppError::Other(anyhow::anyhow!("message encrypt failed")))?;
    let mut out = nonce.to_vec();
    out.extend_from_slice(&ct);
    Ok(STANDARD.encode(out))
}

/// Inverse of [`encrypt`].
pub fn decrypt(key: &[u8; 32], stored: &str) -> Result<String> {
    let raw = STANDARD
        .decode(stored.trim())
        .map_err(|_| AppError::Other(anyhow::anyhow!("message decode failed")))?;
    if raw.len() < NONCE_LEN {
        return Err(AppError::Other(anyhow::anyhow!("ciphertext too short")));
    }
    let (nonce_bytes, ct) = raw.split_at(NONCE_LEN);
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| AppError::Other(anyhow::anyhow!("bad message key")))?;
    let pt = cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ct)
        .map_err(|_| AppError::Other(anyhow::anyhow!("message decrypt failed")))?;
    String::from_utf8(pt).map_err(|_| AppError::Other(anyhow::anyhow!("decrypted bytes not utf8")))
}

// ---------------------------------------------------------------------------
// Keyring — versioned, rotatable at-rest key management
// ---------------------------------------------------------------------------

/// A named 256-bit data-encryption key.
#[derive(Clone)]
pub struct Dek {
    /// Short key-id (ASCII alphanumeric). [`LEGACY_KEY_ID`] is the frameless legacy key.
    pub id: String,
    pub bytes: [u8; 32],
}

impl Dek {
    /// The legacy/default DEK: id [`LEGACY_KEY_ID`], written frameless.
    pub fn legacy(bytes: [u8; 32]) -> Self {
        Self { id: LEGACY_KEY_ID.to_string(), bytes }
    }
}

/// The DEKs a deployment can decrypt with: one **active** (used for new writes)
/// plus any **retired** keys still needed until a re-encrypt pass completes. An
/// empty ring (`active == None`) means at-rest encryption is disabled (dev default).
#[derive(Clone, Default)]
pub struct Keyring {
    active: Option<Dek>,
    retired: Vec<Dek>,
}

impl Keyring {
    /// A ring with one active DEK (and no retired keys).
    pub fn new(active: Option<Dek>, retired: Vec<Dek>) -> Self {
        Self { active, retired }
    }

    /// The legacy single-key ring: `Some` bytes ⇒ active id [`LEGACY_KEY_ID`];
    /// `None` ⇒ encryption disabled. Reproduces the pre-BYOK config behaviour.
    pub fn from_legacy(key: Option<[u8; 32]>) -> Self {
        Self { active: key.map(Dek::legacy), retired: Vec::new() }
    }

    /// Is at-rest encryption enabled (an active DEK present)?
    pub fn is_enabled(&self) -> bool {
        self.active.is_some()
    }

    /// The active DEK bytes, for the legacy single-key call sites that gate on
    /// `Option<[u8; 32]>` (and for tests). `None` ⇒ encryption disabled.
    pub fn active_key(&self) -> Option<[u8; 32]> {
        self.active.as_ref().map(|d| d.bytes)
    }

    /// The active key-id, if enabled.
    pub fn active_id(&self) -> Option<&str> {
        self.active.as_ref().map(|d| d.id.as_str())
    }

    fn dek(&self, id: &str) -> Option<&Dek> {
        if let Some(a) = &self.active {
            if a.id == id {
                return Some(a);
            }
        }
        self.retired.iter().find(|d| d.id == id)
    }

    /// Encrypt with the active DEK, framing the output with its key-id unless the
    /// active id is [`LEGACY_KEY_ID`] (then frameless → byte-identical to legacy).
    pub fn encrypt(&self, plaintext: &str) -> Result<String> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| AppError::Other(anyhow::anyhow!("at-rest encryption is disabled")))?;
        let payload = encrypt(&active.bytes, plaintext)?;
        if active.id == LEGACY_KEY_ID {
            Ok(payload)
        } else {
            Ok(format!("k{}:{}", active.id, payload))
        }
    }

    /// Decrypt a value written by [`encrypt`](Self::encrypt): parse its key-id
    /// frame (or legacy), look the DEK up in the ring, decrypt. Errors loudly if
    /// the id is unknown (a retired key was dropped too early).
    pub fn decrypt(&self, stored: &str) -> Result<String> {
        let (id, payload) = parse_frame(stored);
        let dek = self.dek(id).ok_or_else(|| {
            AppError::Other(anyhow::anyhow!("no key for ciphertext key-id '{id}' in the keyring"))
        })?;
        decrypt(&dek.bytes, payload)
    }
}

/// Split a stored value into `(key_id, payload)`. A frame is `k<id>:<payload>`
/// where `<id>` is non-empty ASCII alphanumeric; anything else (all legacy values,
/// since base64 never contains `:`) is `([LEGACY_KEY_ID], stored)`.
fn parse_frame(stored: &str) -> (&str, &str) {
    if let Some(rest) = stored.strip_prefix('k') {
        if let Some((id, payload)) = rest.split_once(':') {
            if !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric()) {
                return (id, payload);
            }
        }
    }
    (LEGACY_KEY_ID, stored)
}

/// The key-id a stored value is encrypted under (for idempotent re-encrypt: skip
/// rows already at the active id).
pub fn key_id_of(stored: &str) -> &str {
    parse_frame(stored).0
}

/// The process-global keyring, resolved at boot from the [`crate::ext::KeyProvider`]
/// seam. Mirrors the `audit::init_sink`/`init_signing` global-registration pattern,
/// because at-rest crypto is called from pool-only contexts (no `AppState`). Unlike
/// the audit globals it is an [`RwLock`], so a DEK rotation can swap in a new active
/// ring in-process (the re-encrypt job) without restarting.
static KEYRING: OnceLock<RwLock<Arc<Keyring>>> = OnceLock::new();
static KEYRING_INITIALISED: AtomicBool = AtomicBool::new(false);

fn keyring_cell() -> &'static RwLock<Arc<Keyring>> {
    KEYRING.get_or_init(|| RwLock::new(Arc::new(Keyring::default())))
}

/// Install (or replace) the global keyring. Called at boot with the provider's ring,
/// and by a DEK rotation to swap in the new active ring. Marks the keyring as
/// explicitly initialised so [`ensure_keyring_from_legacy`] won't override it.
pub fn init_keyring(keyring: Keyring) {
    *keyring_cell().write().expect("keyring lock poisoned") = Arc::new(keyring);
    KEYRING_INITIALISED.store(true, Ordering::SeqCst);
}

/// Ensure a global keyring exists, defaulting to the legacy single-key ring parsed
/// from config. No-op if a provider already installed one. Lets tests / Core builds
/// that skip provider installation still resolve at-rest crypto identically.
pub fn ensure_keyring_from_legacy(key_b64: &str) {
    if !KEYRING_INITIALISED.load(Ordering::SeqCst) {
        init_keyring(Keyring::from_legacy(parse_key(key_b64)));
    }
}

/// The active global keyring (an empty, disabled ring if never initialised).
pub fn keyring() -> Arc<Keyring> {
    keyring_cell().read().expect("keyring lock poisoned").clone()
}

/// Is at-rest encryption enabled for this deployment? (global keyring has an active DEK)
pub fn at_rest_enabled() -> bool {
    keyring().is_enabled()
}

/// Encrypt a sensitive field for storage, using the global keyring's active DEK.
/// This is the single entry point every `*_enc` column write should use.
pub fn encrypt_at_rest(plaintext: &str) -> Result<String> {
    keyring().encrypt(plaintext)
}

/// Decrypt a value written by [`encrypt_at_rest`], selecting the DEK by its key-id
/// frame (reads both legacy and rotated formats). The single entry point every
/// `*_enc` column read should use.
pub fn decrypt_at_rest(stored: &str) -> Result<String> {
    keyring().decrypt(stored)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let key = [7u8; 32];
        let ct = encrypt(&key, "secret café ☕").unwrap();
        assert_ne!(ct, "secret café ☕");
        assert_eq!(decrypt(&key, &ct).unwrap(), "secret café ☕");
    }

    #[test]
    fn wrong_key_fails() {
        let ct = encrypt(&[1u8; 32], "hi").unwrap();
        assert!(decrypt(&[2u8; 32], &ct).is_err());
    }

    #[test]
    fn legacy_active_key_is_frameless_and_byte_compatible() {
        // A ring whose active DEK is the legacy id must produce frameless output
        // that the raw legacy `decrypt` reads — proving byte-format compatibility.
        let key = [9u8; 32];
        let kr = Keyring::from_legacy(Some(key));
        let ct = kr.encrypt("hello").unwrap();
        assert!(!ct.starts_with("k1:"), "legacy active id must not frame");
        assert_eq!(decrypt(&key, &ct).unwrap(), "hello"); // raw legacy path reads it
        assert_eq!(kr.decrypt(&ct).unwrap(), "hello");
        assert_eq!(key_id_of(&ct), LEGACY_KEY_ID);
    }

    #[test]
    fn rotated_active_frames_and_round_trips() {
        let old = Dek::legacy([1u8; 32]);
        let new = Dek { id: "2".to_string(), bytes: [2u8; 32] };
        let kr = Keyring::new(Some(new), vec![old]);
        let ct = kr.encrypt("world").unwrap();
        assert!(ct.starts_with("k2:"));
        assert_eq!(key_id_of(&ct), "2");
        assert_eq!(kr.decrypt(&ct).unwrap(), "world");
    }

    #[test]
    fn retired_key_decrypts_legacy_row_after_rotation() {
        // Value written under the old legacy key, read by a rotated ring whose
        // active is a new key but which still holds the old key as retired.
        let old_bytes = [3u8; 32];
        let legacy_ct = encrypt(&old_bytes, "aged").unwrap(); // frameless
        let kr = Keyring::new(
            Some(Dek { id: "7".into(), bytes: [4u8; 32] }),
            vec![Dek::legacy(old_bytes)],
        );
        assert_eq!(kr.decrypt(&legacy_ct).unwrap(), "aged");
    }

    #[test]
    fn unknown_key_id_fails_loudly() {
        let kr = Keyring::from_legacy(Some([5u8; 32]));
        assert!(kr.decrypt("k9:AAAAAAAAAAAAAAAA").is_err());
    }

    #[test]
    fn parse_frame_treats_colonless_legacy_as_legacy() {
        // Legacy base64 can start with 'k' but never contains ':'.
        assert_eq!(parse_frame("kABCDEF"), (LEGACY_KEY_ID, "kABCDEF"));
        assert_eq!(parse_frame("k2:xyz"), ("2", "xyz"));
    }

    #[test]
    fn global_keyring_install_and_rotate_swap() {
        // Install a legacy ring, write via the global at-rest entry points.
        let old = [11u8; 32];
        init_keyring(Keyring::from_legacy(Some(old)));
        assert!(at_rest_enabled());
        let ct1 = encrypt_at_rest("v1").unwrap();
        assert_eq!(key_id_of(&ct1), LEGACY_KEY_ID);
        assert_eq!(decrypt_at_rest(&ct1).unwrap(), "v1");

        // Rotate: swap in a ring with a new active DEK, retaining the old one.
        init_keyring(Keyring::new(
            Some(Dek { id: "2".into(), bytes: [12u8; 32] }),
            vec![Dek::legacy(old)],
        ));
        // Old ciphertext still reads (retired key); new writes carry the new frame.
        assert_eq!(decrypt_at_rest(&ct1).unwrap(), "v1");
        let ct2 = encrypt_at_rest("v2").unwrap();
        assert_eq!(key_id_of(&ct2), "2");
        assert_eq!(decrypt_at_rest(&ct2).unwrap(), "v2");
    }
}
