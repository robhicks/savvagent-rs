//! Stub library target so Cargo accepts this fixture as a path
//! dev-dependency from `savvagent-host`. Adding a `[lib]` target lets
//! Cargo include the package in the dependency graph (binary-only path
//! deps are silently dropped — see the "ignoring invalid dependency"
//! warning); that inclusion in turn causes Cargo to build the
//! `resource-tool` binary and export `CARGO_BIN_EXE_resource-tool` to
//! the test harness in `savvagent-host`.
//!
//! No real code lives here.
