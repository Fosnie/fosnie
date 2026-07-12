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

//! PAI Platform backend — library surface.
//!
//! The binary (`main.rs`) wires these modules together; the integration tests
//! in `tests/` drive them directly. Only the cross-cutting skeleton is present
//! so far: typed config, the audit hash-chain, datastore pools, health
//! transport and the durable background scheduler. Auth/RBAC and the feature
//! modules land in their own slices.

pub mod agent;
pub mod audit;
pub mod auth;
pub mod automations;
pub mod cache;
pub mod chat;
pub mod code_interpreter;
pub mod config;
pub mod crypto;
pub mod db;
pub mod documents;
pub mod embedding_index;
pub mod error;
pub mod events;
pub mod ext;
pub mod features;
pub mod groundedness;
pub mod http;
pub mod integrations;
pub mod kb;
pub mod mcp;
pub mod metrics;
pub mod ml;
pub mod provider_cli;
pub mod providers;
pub mod providers_seed;
pub mod reasoning;
pub mod scheduler;
pub mod server;
pub mod skills_seed;
pub mod state;
pub mod storage;
pub mod telemetry;
pub mod tools;
pub mod vision;
pub mod research;
pub mod upload;
pub mod voice;
pub mod web_search;
pub mod workflows;
pub mod ws;

pub use error::{AppError, Result};
pub use state::AppState;
