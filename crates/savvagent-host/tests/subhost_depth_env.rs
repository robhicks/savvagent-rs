//! Env-var tests for `max_depth_from_env`. Lives as an integration test
//! (not a unit test) because `savvagent-host` declares
//! `#![forbid(unsafe_code)]` at the crate root, and the Rust 2024 edition
//! requires `unsafe` blocks around `std::env::set_var` /
//! `std::env::remove_var`. Integration tests are separate binaries and
//! do not inherit the lib crate's lint level.
//!
//! Each test saves and restores the previous value of
//! `SAVVAGENT_AGENT_MAX_DEPTH` so it does not leak into sibling tests in
//! this binary.

use savvagent_host::max_depth_from_env;

#[test]
fn depth_limit_env_default_is_three() {
    // SAFETY: this integration test binary is the only writer of
    // SAVVAGENT_AGENT_MAX_DEPTH, and each test restores the previous
    // value before returning.
    let prev = std::env::var("SAVVAGENT_AGENT_MAX_DEPTH").ok();
    unsafe {
        std::env::remove_var("SAVVAGENT_AGENT_MAX_DEPTH");
    }
    assert_eq!(max_depth_from_env(), 3);
    if let Some(v) = prev {
        unsafe {
            std::env::set_var("SAVVAGENT_AGENT_MAX_DEPTH", v);
        }
    }
}

#[test]
fn depth_limit_env_override_parses() {
    // SAFETY: see `depth_limit_env_default_is_three`.
    let prev = std::env::var("SAVVAGENT_AGENT_MAX_DEPTH").ok();
    unsafe {
        std::env::set_var("SAVVAGENT_AGENT_MAX_DEPTH", "5");
    }
    assert_eq!(max_depth_from_env(), 5);
    unsafe {
        match prev {
            Some(v) => std::env::set_var("SAVVAGENT_AGENT_MAX_DEPTH", v),
            None => std::env::remove_var("SAVVAGENT_AGENT_MAX_DEPTH"),
        }
    }
}

#[test]
fn depth_limit_env_invalid_falls_back() {
    // SAFETY: see `depth_limit_env_default_is_three`.
    let prev = std::env::var("SAVVAGENT_AGENT_MAX_DEPTH").ok();
    unsafe {
        std::env::set_var("SAVVAGENT_AGENT_MAX_DEPTH", "not-a-number");
    }
    assert_eq!(max_depth_from_env(), 3);
    unsafe {
        match prev {
            Some(v) => std::env::set_var("SAVVAGENT_AGENT_MAX_DEPTH", v),
            None => std::env::remove_var("SAVVAGENT_AGENT_MAX_DEPTH"),
        }
    }
}
