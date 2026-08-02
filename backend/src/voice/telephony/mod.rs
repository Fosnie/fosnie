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

//! Carrying a live-voice session over a telephone line.
//!
//! A telephone call is narrowband and paced: the media is 8 kHz mono G.711, in
//! frames of 20 ms, and it arrives and departs in real time whatever the engines
//! at either end are doing. The engines are neither: recognition wants 16 kHz and
//! synthesis produces 24 kHz, both in bursts as fast as they can manage.
//!
//! This module is the translation between the two, and only that. It converts and
//! it paces; it knows nothing about any particular carrier, about who is calling,
//! or about what is being said.

pub mod codec;
pub mod pace;
pub mod record;
pub mod sink;

pub use sink::{Control, TelephonySink, Wire};
