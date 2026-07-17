//! Guard against a class of regression: a scaffolding call that inherits the
//! user's reasoning effort. A reasoning-capable model at high effort with a small
//! output cap spends the budget on reasoning tokens and emits nothing (empty
//! output, silently broken feature) or, on a generous cap, burns cost for no gain.
//!
//! Every `ml::chat_step` call outside `src/ml/` must therefore be either the user's
//! turn (full effort, threaded through `with_reasoning`) or a scaffolding call that
//! pins its effort (`reasoning_effort`). This test reads the source tree and fails
//! if a call site does neither, catching the next forgotten site at test time
//! rather than as a blank box in production months later.

use std::fs;
use std::path::Path;

#[test]
fn every_chat_step_site_pins_effort_or_is_the_turn() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sites = 0usize;
    let mut offenders: Vec<String> = Vec::new();

    visit(&src, &mut |path, text| {
        // `ml/` owns `chat_step` itself (definition + internal callers); the
        // discipline is about its OUTSIDE callers, so skip that module.
        if path.components().any(|c| c.as_os_str() == "ml") {
            return;
        }
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            // The definition lives in ml/ (skipped); every hit here is a call.
            if !line.contains("chat_step(") {
                continue;
            }
            sites += 1;
            // The sampling is constructed just before the call. Look back a small
            // window for the pin (`reasoning_effort`) or the turn seam
            // (`with_reasoning`, which threads the user's full effort deliberately).
            let start = i.saturating_sub(25);
            let window = lines[start..=i].join("\n");
            let is_turn = window.contains("with_reasoning");
            let is_pinned = window.contains("reasoning_effort");
            if !is_turn && !is_pinned {
                offenders.push(format!("{}:{}", path.display(), i + 1));
            }
        }
    });

    assert!(sites >= 5, "expected to find the known chat_step call sites; found only {sites}");
    assert!(
        offenders.is_empty(),
        "these chat_step call sites neither pin a scaffolding effort \
         (`reasoning_effort: Some(\"minimal\")`) nor thread the user's turn effort \
         (`with_reasoning`):\n{}",
        offenders.join("\n")
    );
}

fn visit(dir: &Path, f: &mut impl FnMut(&Path, &str)) {
    for entry in fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            visit(&p, f);
        } else if p.extension().map(|e| e == "rs").unwrap_or(false) {
            let text = fs::read_to_string(&p).unwrap();
            f(&p, &text);
        }
    }
}
