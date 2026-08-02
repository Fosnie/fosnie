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

//! A telephone line can be tuned separately from a browser tab, and doing so must not
//! move the tab.
//!
//! The resolution rules themselves are pinned in unit tests, which is where the four
//! combinations belong. What only a real database can show is that the settings are
//! actually found: they are read in one pass with a key pattern, and a pattern that
//! stopped matching would resolve every dial to its default and look exactly like an
//! instance nobody had tuned.
//!
//! Needs a reachable Postgres; skips when `DATABASE_URL` is unset.

use sqlx::PgPool;

use fosnie_backend::config::runtime::{self, ConfigValueType};
use fosnie_backend::db;
use fosnie_backend::voice::{VoiceKnobs, VoiceProfile};

async fn pool() -> Option<PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    // Skipping is for "no database configured". A database that IS configured but cannot
    // be reached is an environment fault, and quietly reporting a pass for it is how an
    // untested change looks tested.
    Some(db::connect(&url, 5).await.unwrap_or_else(|e| {
        panic!("DATABASE_URL is set but unreachable, so nothing here was tested: {e}")
    }))
}

/// The dials this test writes. They are deployment-wide rows, so whatever was there is
/// noted and put back: removing them instead would take a developer's own tuning with it.
const TOUCHED: [(&str, ConfigValueType); 4] = [
    ("voice.silence_threshold_ms", ConfigValueType::Int),
    ("voice.phone.silence_threshold_ms", ConfigValueType::Int),
    ("voice.barge_min_ms", ConfigValueType::Int),
    ("voice.phone.barge_min_ms", ConfigValueType::Int),
];

async fn borrow(pg: &PgPool) -> Vec<(&'static str, ConfigValueType, Option<String>)> {
    let mut was = Vec::new();
    for (key, kind) in TOUCHED {
        let existing = runtime::get(pg, key).await.ok().flatten().map(|e| e.value);
        was.push((key, kind, existing));
    }
    was
}

async fn restore(pg: &PgPool, was: &[(&'static str, ConfigValueType, Option<String>)]) {
    for (key, kind, value) in was {
        match value {
            Some(v) => {
                let _ = runtime::set(pg, key, v, *kind, "global", None, "system").await;
            }
            None => {
                let _ = runtime::unset(pg, key, "system").await;
            }
        }
    }
}

async fn set(pg: &PgPool, key: &str, value: &str, kind: ConfigValueType) {
    runtime::set(pg, key, value, kind, "global", None, "system").await.expect("write a dial");
}

async fn clear(pg: &PgPool, key: &str) {
    let _ = runtime::unset(pg, key, "system").await;
}

#[tokio::test]
async fn a_line_and_a_tab_are_tuned_separately() {
    let Some(pg) = pool().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let was = borrow(&pg).await;
    let outcome = check(&pg).await;
    restore(&pg, &was).await;
    outcome.expect("the profile resolved wrongly");
}

async fn check(pg: &PgPool) -> Result<(), String> {
    // ---- Nothing set: each transport gets its own compiled defaults. ----
    for (key, _) in TOUCHED {
        clear(pg, key).await;
    }
    let tab = VoiceKnobs::load_for(pg, VoiceProfile::Browser).await;
    let line = VoiceKnobs::load_for(pg, VoiceProfile::Phone).await;
    if tab.barge_min_ms != VoiceKnobs::default().barge_min_ms {
        return Err(format!("an untuned tab resolved to {}", tab.barge_min_ms));
    }
    if line.barge_min_ms != VoiceKnobs::phone().barge_min_ms {
        return Err(format!("an untuned line resolved to {}", line.barge_min_ms));
    }
    if line.barge_min_ms == tab.barge_min_ms {
        return Err("a line and a tab resolved the same, so nothing is being tuned".into());
    }

    // ---- Only the shared dial set: the change reaches the line too. ----
    // The rule most likely to surprise, so it is proved rather than assumed.
    set(pg, "voice.silence_threshold_ms", "700", ConfigValueType::Int).await;
    let tab = VoiceKnobs::load_for(pg, VoiceProfile::Browser).await;
    let line = VoiceKnobs::load_for(pg, VoiceProfile::Phone).await;
    if tab.silence_threshold_ms != 700 || line.silence_threshold_ms != 700 {
        return Err(format!(
            "a shared change gave tab {} and line {}",
            tab.silence_threshold_ms, line.silence_threshold_ms
        ));
    }

    // ---- Both set: the line's own value wins, and only for the line. ----
    set(pg, "voice.phone.silence_threshold_ms", "400", ConfigValueType::Int).await;
    let tab = VoiceKnobs::load_for(pg, VoiceProfile::Browser).await;
    let line = VoiceKnobs::load_for(pg, VoiceProfile::Phone).await;
    if tab.silence_threshold_ms != 700 {
        return Err(format!("tuning the line moved the tab to {}", tab.silence_threshold_ms));
    }
    if line.silence_threshold_ms != 400 {
        return Err(format!("the line ignored its own dial and used {}", line.silence_threshold_ms));
    }

    // ---- Only the line's dial set: the tab keeps its default. ----
    clear(pg, "voice.silence_threshold_ms").await;
    let tab = VoiceKnobs::load_for(pg, VoiceProfile::Browser).await;
    let line = VoiceKnobs::load_for(pg, VoiceProfile::Phone).await;
    if tab.silence_threshold_ms != VoiceKnobs::default().silence_threshold_ms {
        return Err(format!("a line-only dial moved the tab to {}", tab.silence_threshold_ms));
    }
    if line.silence_threshold_ms != 400 {
        return Err(format!("the line lost its own dial: {}", line.silence_threshold_ms));
    }

    // ---- And the whole set arrives, not just the first dial. ----
    // One read fetches every dial, so a pattern that matched too narrowly would show up
    // here as a second dial quietly falling back.
    set(pg, "voice.phone.barge_min_ms", "180", ConfigValueType::Int).await;
    let line = VoiceKnobs::load_for(pg, VoiceProfile::Phone).await;
    if line.silence_threshold_ms != 400 || line.barge_min_ms != 180 {
        return Err(format!(
            "only some dials arrived: silence {}, talk-over {}",
            line.silence_threshold_ms, line.barge_min_ms
        ));
    }

    Ok(())
}
