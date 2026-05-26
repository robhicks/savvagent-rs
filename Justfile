# Build the wasm fixtures used by `cargo test -p savvagent-plugin-wasm`.
#
# Requirements:
#   - rustup target add wasm32-unknown-unknown
#   - cargo install cargo-component --locked
#
# Each fixture is its own component-model Rust crate that targets one of
# the three WIT worlds. Built fixtures are copied to
# `crates/savvagent-plugin-wasm/tests/fixtures/*.wasm` and committed to the
# repo so day-to-day `cargo test` doesn't need the wasm toolchain.

# Build all wasm fixtures and copy them into the test fixtures dir.
build-fixtures: build-fixture-static build-fixture-interactive build-fixture-provider

# Build the `plugin-static` world fixture and copy to tests/fixtures/static.wasm.
#
# We build for `wasm32-unknown-unknown` rather than `wasm32-wasip1` /
# `wasm32-wasip2` to avoid pulling in the WASI preview1-adapter import
# stubs (`wasi:cli/environment`, …). Our static-world plugin only needs
# `log` + `current-theme` host imports; the WASI stubs would force the
# host adapter to wire up a wasi-cli backend that pure-data plugins don't
# actually need.
build-fixture-static:
    cd crates/savvagent-plugin-wasm/tests/fixtures-src/static && \
        cargo component build --target wasm32-unknown-unknown --release
    cp crates/savvagent-plugin-wasm/tests/fixtures-src/static/target/wasm32-unknown-unknown/release/fixture_static.wasm \
       crates/savvagent-plugin-wasm/tests/fixtures/static.wasm

# Build the `plugin-interactive` world fixture and copy to
# tests/fixtures/interactive.wasm. Same `wasm32-unknown-unknown` target as
# the static fixture for the same WASI-import-avoidance reasons.
build-fixture-interactive:
    cd crates/savvagent-plugin-wasm/tests/fixtures-src/interactive && \
        cargo component build --target wasm32-unknown-unknown --release
    cp crates/savvagent-plugin-wasm/tests/fixtures-src/interactive/target/wasm32-unknown-unknown/release/fixture_interactive.wasm \
       crates/savvagent-plugin-wasm/tests/fixtures/interactive.wasm

# Build the `plugin-provider` world fixture and copy to
# tests/fixtures/provider.wasm. Same `wasm32-unknown-unknown` target as the
# static / interactive fixtures.
build-fixture-provider:
    cd crates/savvagent-plugin-wasm/tests/fixtures-src/provider && \
        cargo component build --target wasm32-unknown-unknown --release
    cp crates/savvagent-plugin-wasm/tests/fixtures-src/provider/target/wasm32-unknown-unknown/release/fixture_provider.wasm \
       crates/savvagent-plugin-wasm/tests/fixtures/provider.wasm
