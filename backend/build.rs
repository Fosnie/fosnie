// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
fn main() {
    // SQLx migrate! tracks files already embedded in a binary. Directory
    // tracking also rebuilds that binary when a new migration is added.
    println!("cargo:rerun-if-changed=migrations");
}
