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

//! WebSocket wire protocol — JSON envelope `{ version, type, … }` with a `type`
//! discriminator. One multiplexed socket per user.
//!
//! The frame types themselves live in the shared protocol crate, because the
//! server is no longer the only thing that has to know them: a client released
//! on its own schedule compiles the same definitions, so the two cannot drift
//! apart without the build saying so. This module is the server's name for them
//! and stays the path the rest of the backend uses.

pub use fosnie_protocol::{
    CitationOut, ClientFrame, EditChangeOut, GroundSpanOut, PROTOCOL_VERSION, ServerFrame, StepOut,
};
