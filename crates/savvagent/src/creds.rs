//! Thin wrapper over [`keyring`] for stashing per-provider API keys.
//!
//! Storage backend is whatever `keyring` picks for the platform — macOS
//! Keychain, Windows Credential Manager, or the freedesktop Secret Service on
//! Linux. The TUI never touches the raw bytes outside this module.

use keyring::Entry;

/// Service name we register entries under.
const SERVICE: &str = "savvagent";

/// Persist `api_key` under `provider_id`, overwriting any previous value.
pub fn save(provider_id: &str, api_key: &str) -> Result<(), keyring::Error> {
    Entry::new(SERVICE, provider_id)?.set_password(api_key)
}

/// Look up the key for `provider_id`. Returns `Ok(None)` if no entry exists or
/// the platform backend is unavailable; `Err` only on real backend faults.
pub fn load(provider_id: &str) -> Result<Option<String>, keyring::Error> {
    let entry = match Entry::new(SERVICE, provider_id) {
        Ok(e) => e,
        Err(keyring::Error::NoStorageAccess(_)) => return Ok(None),
        Err(e) => return Err(e),
    };
    match entry.get_password() {
        Ok(s) => Ok(Some(s)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(keyring::Error::NoStorageAccess(_)) => Ok(None),
        Err(e) => Err(e),
    }
}
