//! Guard: `test-mocks` (a test-only extra-root-cert hook in the MCP hardened HTTP
//! client) must never become a default feature. Shipping it on silently would leave
//! an installed root able to be trusted in production. This reads the manifest and
//! fails if any `default` feature array lists it. Always runs (not feature-gated).

use std::fs;
use std::path::Path;

#[test]
fn test_mocks_is_declared_and_not_default() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let text = fs::read_to_string(&manifest).expect("read Cargo.toml");

    // The feature must exist, or this guard is meaningless.
    assert!(
        text.contains("test-mocks = ["),
        "the `test-mocks` feature declaration is missing from Cargo.toml"
    );

    // No `default = [ ... ]` feature list may enable it. Core has no default feature
    // today; if one is ever added it must not pull in test-mocks.
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("default") && t.contains('=') {
            assert!(
                !line.contains("test-mocks"),
                "`test-mocks` must not be enabled by a default feature: {line}"
            );
        }
    }
}
