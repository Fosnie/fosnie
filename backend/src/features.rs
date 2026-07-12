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

//! Per-user-group feature resolution (Tier-2 #8). Thin delegators over the
//! [`FeatureResolver`](crate::ext::FeatureResolver) seam: the resolution logic
//! lives once in [`HostFeatureResolver`](crate::ext::HostFeatureResolver) and is
//! reached through `state.features` so an Enterprise override is honoured here too.

use crate::auth::AuthContext;
use crate::state::AppState;

/// Is `feature` enabled for this caller? Delegates to the configured resolver
/// (`state.features`).
pub async fn enabled_for(state: &AppState, ctx: &AuthContext, feature: &str) -> bool {
    state.features.enabled_for(state, ctx, feature).await
}

/// As [`enabled_for`], keyed by a raw user id (for the WebSocket path).
pub async fn enabled_for_user(state: &AppState, user_id: Option<uuid::Uuid>, feature: &str) -> bool {
    state.features.enabled_for_user(state, user_id, feature).await
}
