//! Sanity test: every WIT file in the crate parses cleanly with `wit-parser`,
//! and the canonical interfaces (`types`, `spp`) plus the three world
//! skeletons (`plugin-static`, `plugin-interactive`, `plugin-provider`)
//! resolve in a single `savvagent:plugin@0.1.0` package.
//!
//! This guards against (a) accidental syntactic regressions in any `.wit`
//! file and (b) the multi-file directory drifting out of single-package
//! shape — Task 2 generates host bindings off the same `wit/` tree via
//! `wasmtime::component::bindgen!`, which requires the parse to succeed.

use std::path::PathBuf;

#[test]
fn every_wit_file_parses() {
    let wit_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("wit");
    let mut resolve = wit_parser::Resolve::default();
    let (pkg_id, _source_map) = resolve
        .push_dir(&wit_dir)
        .expect("wit/ must contain parseable .wit files");

    let pkg = &resolve.packages[pkg_id];
    assert_eq!(
        pkg.name.namespace, "savvagent",
        "package namespace must stay `savvagent`",
    );
    assert_eq!(
        pkg.name.name, "plugin",
        "package name must stay `plugin` (spp interface inlined per spp.wit's reconciliation note)",
    );

    // Both top-level interfaces must be present and resolvable.
    assert!(
        pkg.interfaces.contains_key("types"),
        "shared.wit must declare interface `types`",
    );
    assert!(
        pkg.interfaces.contains_key("spp"),
        "spp.wit must declare interface `spp`",
    );

    // All three world skeletons must resolve.
    for world in ["plugin-static", "plugin-interactive", "plugin-provider"] {
        assert!(
            pkg.worlds.contains_key(world),
            "world `{world}` must resolve from its skeleton file",
        );
    }
}
