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

//! Device tokens on the native surface.
//!
//! A paired desktop client has no browser session, so the configured
//! [`AuthProvider`](crate::ext::AuthProvider) cannot authenticate it. It
//! presents a device token instead: a `kind='device'` platform key, minted by
//! the pairing flow and anchored to a `devices` row. This module resolves such a
//! token into the owner's [`AuthContext`] and marks the request so the two
//! handlers that care about provenance can read it.
//!
//! A device carries **exactly its owner's rights** — there is no per-device
//! scope. Nothing downstream branches on device-ness for an authorisation
//! decision, which is why the device id rides as a request extension rather than
//! a field on [`AuthContext`]: the two places that read it are about issuance
//! (a device may not mint further pairing codes) and provenance (a chat it
//! starts is marked as desktop), not about what it is allowed to do.
//!
//! **Availability.** The fallback lives inside the `AuthUser` extractor, which
//! runs unlayered in the built-in-accounts mode. When an external identity
//! provider fronts the protected routes with a bearer-validation layer, a
//! request carrying a platform token would be refused by that layer before any
//! extractor runs; the router installs a bypass so the same token still reaches
//! this path.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use std::convert::Infallible;
use uuid::Uuid;

use crate::auth::api_key::{self, KeyKind, ResolvedKey};
use crate::auth::AuthContext;
use crate::error::AppError;
use crate::state::AppState;

/// Placed on a request once a device token has authenticated it, so a handler
/// can tell a device apart from a browser session. Inserted by the `AuthUser`
/// extractor; read through [`MaybeDevice`].
#[derive(Debug, Clone, Copy)]
pub struct DeviceAuth(pub Uuid);

/// Resolve a native-surface device token, or `None` if the header is absent or
/// carries anything but a live one of ours.
///
/// An error from the token itself is folded into `None` on purpose: the caller
/// has already failed session authentication and will return its own
/// unauthorised error, which must not vary with what the header contained.
pub async fn try_device_auth(parts: &Parts, state: &AppState) -> Option<(AuthContext, Uuid)> {
    let token = api_key::bearer_token(parts)?;
    if !token.starts_with(api_key::TOKEN_PREFIX) {
        return None;
    }
    let ResolvedKey { key_id, device_id, ctx } =
        api_key::resolve(state, token, KeyKind::Device).await.ok()?;
    // A device token always carries a device (the table CHECK guarantees it);
    // treat its absence as a failure rather than authenticating without one.
    let device_id = device_id?;
    api_key::touch_used(state, key_id, Some(device_id)).await;
    Some((ctx, device_id))
}

/// The device behind the current request, when it was authenticated by a device
/// token. Reads the marker that the `AuthUser` extractor inserts, so a handler
/// that needs it **must list `AuthUser` before `MaybeDevice`** in its argument
/// list; the reverse order silently reads `None`, which fails open.
pub struct MaybeDevice(pub Option<Uuid>);

impl FromRequestParts<AppState> for MaybeDevice {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(MaybeDevice(parts.extensions.get::<DeviceAuth>().map(|d| d.0)))
    }
}

impl MaybeDevice {
    /// Refuse a request that was authenticated by a device token. A device
    /// carries its owner's rights, but a handful of sensitive writes must
    /// originate from an interactive web session: a stolen device token must not
    /// be able to mint a credential that outlives the device, redirect the
    /// owner's model traffic to another endpoint, delete the account, or reach
    /// across to another user's devices.
    pub fn require_session(&self) -> Result<(), AppError> {
        if self.0.is_some() {
            Err(AppError::Forbidden(
                "this must be done from the web, not from a paired device".into(),
            ))
        } else {
            Ok(())
        }
    }
}
