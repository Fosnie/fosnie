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

//! The wire protocol both ends of the socket agree on.
//!
//! The server and any client that is not the web application are built and
//! released separately, so the one thing they cannot afford to describe twice is
//! the shape of what they send each other. These types live here, are compiled
//! into both, and any change to them is a change to both at once — a drift that
//! would otherwise surface as a malformed frame at runtime is a compilation
//! failure instead.
//!
//! Frames serialise both ways: the server writes what the client reads and the
//! client writes what the server reads, so the derives are symmetrical even where
//! one side only ever does half of it. The golden fixtures under `tests/` pin the
//! bytes.

mod frames;
mod reasoning;

pub use frames::{
    CitationOut, ClientFrame, EditChangeOut, GroundSpanOut, PROTOCOL_VERSION, ServerFrame, StepOut,
};
pub use reasoning::ReasoningSpec;
