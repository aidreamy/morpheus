//! Provider credential keys shared across user interfaces.

use aiface::connector::get_provider_env_var;

/// Resolve the API-key env var name for a (cloud) provider: built-ins
/// carry a documented one; user-registered custom providers derive
/// `<NAME>_API_KEY`, matching `aiface`'s own derivation so a key saved
/// under that name is actually found later.
#[must_use]
pub fn provider_env_var_name(name: &str) -> String {
    get_provider_env_var(name)
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}_API_KEY", name.to_ascii_uppercase().replace('-', "_")))
}

/// Persist a provider API key in the OS credential store (keyring) under
/// [`KEYRING_SERVICE`](crate::KEYRING_SERVICE), keyed by the provider's API
/// key environment variable name.
///
/// The key is stored via the native store (Secret Service on Linux, Keychain
/// on macOS, Credential Manager on Windows). Errors are returned to the caller
/// so it can surface them instead of failing silently.
pub fn save_provider_api_key(env_var: &str, api_key: &str) -> Result<(), keyring::Error> {
    let entry = keyring::Entry::new(crate::KEYRING_SERVICE, env_var)?;
    entry.set_password(api_key)?;
    Ok(())
}
