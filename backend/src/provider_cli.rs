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

//! Provider verify-harness. `fosnie-backend providers test
//! --config <file>` reads a set of provider configs and prints a provider × role
//! matrix of OK/latency/error, reusing the SAME [`crate::ml::test_provider`] path
//! as the HTTP endpoints (single source of truth). No DB — these are explicit
//! configs, not stored rows. Secrets come from the environment (`api_key =
//! "env:NAME"`), never from the config file itself.

use std::path::PathBuf;

use anyhow::Context;
use serde::Deserialize;

#[derive(clap::Subcommand)]
pub enum ProvidersCmd {
    /// Probe each provider in a JSON config and print a role × provider matrix.
    Test {
        /// JSON file: `{"providers":[{name, role, base_url?, model?, api_key?}]}`.
        /// `api_key` may be `"env:NAME"` to read a secret from the environment.
        #[arg(long)]
        config: PathBuf,
    },
}

#[derive(Deserialize)]
struct ProviderEntry {
    name: String,
    role: String,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
}

#[derive(Deserialize)]
struct HarnessConfig {
    #[serde(default)]
    providers: Vec<ProviderEntry>,
}

/// `env:NAME` → the environment value; any other string is used verbatim.
fn resolve_secret(v: &str) -> anyhow::Result<String> {
    if let Some(name) = v.strip_prefix("env:") {
        std::env::var(name).with_context(|| format!("env var {name} (api_key reference) is unset"))
    } else {
        Ok(v.to_string())
    }
}

pub async fn run_cli(boot: crate::config::BootConfig, action: ProvidersCmd) -> anyhow::Result<()> {
    match action {
        ProvidersCmd::Test { config } => {
            let raw = std::fs::read_to_string(&config)
                .with_context(|| format!("reading {}", config.display()))?;
            let cfg: HarnessConfig =
                serde_json::from_str(&raw).context("parsing provider config JSON")?;
            let http = crate::state::build_ml_client(&boot.ml.shared_secret);
            let base = &boot.ml.base_url;

            println!("{:<22} {:<8} result", "provider", "role");
            println!("{}", "-".repeat(60));
            let mut failures = 0u32;
            for e in &cfg.providers {
                if !crate::providers::ROLES.contains(&e.role.as_str()) {
                    println!("{:<22} {:<8} ✗ unknown role", e.name, e.role);
                    failures += 1;
                    continue;
                }
                let mut ov = crate::ml::ProviderOverrides::new();
                let put = |ov: &mut crate::ml::ProviderOverrides, suffix: &str, val: &Option<String>| {
                    if let Some(v) = val.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                        ov.insert(format!("{}_{suffix}", e.role), v.to_string().into());
                    }
                };
                put(&mut ov, "base_url", &e.base_url);
                put(&mut ov, "model", &e.model);
                if let Some(raw_key) = e.api_key.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    ov.insert(format!("{}_api_key", e.role), resolve_secret(raw_key)?.into());
                }

                let line = match crate::ml::test_provider(&http, base, &e.role, ov).await {
                    Ok(r) if r.ok => {
                        let extra = r.detail.map(|d| format!(" ({d})")).unwrap_or_default();
                        format!("✓ {:.0} ms{extra}", r.latency_ms)
                    }
                    Ok(r) => {
                        failures += 1;
                        format!("✗ {}", r.error.unwrap_or_else(|| "failed".into()))
                    }
                    Err(err) => {
                        failures += 1;
                        format!("✗ {err}")
                    }
                };
                println!("{:<22} {:<8} {line}", e.name, e.role);
            }
            if failures > 0 {
                anyhow::bail!("{failures} provider probe(s) failed");
            }
        }
    }
    Ok(())
}
