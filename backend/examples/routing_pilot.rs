// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Private Core entry; shared fixture permits an installed edition consumer.
#[path="support/routing_pilot_shared.rs"]
mod pilot;
#[tokio::main]
async fn main() {
    let migrations=sqlx::migrate!("./migrations");
    if pilot::run_with_edition(std::sync::Arc::new(pilot::CoreEdition),&migrations).await.is_err() {
        eprintln!("Isolated pilot setup stopped; details withheld to protect credentials.");
        std::process::exit(1);
    }
}
