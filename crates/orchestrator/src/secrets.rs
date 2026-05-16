//! OS-keychain wrapper for provider API keys and other secrets.
//!
//! Per AGENTS.md "Secrets via keychain only" — API keys never touch project files, env
//! files committed to the repo, or logs. Every secret is stored against a stable
//! `service` + `account` pair so users (and us, in tests) can find and delete them via
//! the OS UI: `Keychain Access.app` on macOS, `Credential Manager` on Windows,
//! `libsecret`-aware tools on Linux.

use keyring::Entry;
use thiserror::Error;
use tracing::{debug, warn};

/// `service` string passed to the OS keychain. All entries the studio creates share this so
/// they're easy to find and uninstall together.
pub const SERVICE: &str = "narrowmind-studio";

#[derive(Debug, Error)]
pub enum SecretError {
    #[error("keychain backend error: {0}")]
    Backend(#[from] keyring::Error),

    #[error("rejected: secret value is empty")]
    Empty,
}

/// Thin facade over [`keyring::Entry`]. Static methods only — there is nothing to keep
/// in memory for keychain access, the cost is in the syscall, not the handle.
pub struct SecretStore;

impl SecretStore {
    /// Build the `account` string used for a provider's API key.
    /// Stable so deleting a key out-of-band stays addressable.
    #[must_use]
    pub fn provider_account(provider: &str) -> String {
        format!("provider.{provider}.api_key")
    }

    /// Store (or replace) a provider API key. Empty / whitespace-only values are refused
    /// rather than silently storing nothing — the caller almost always meant to delete.
    pub fn set_provider_key(provider: &str, key: &str) -> Result<(), SecretError> {
        if key.trim().is_empty() {
            return Err(SecretError::Empty);
        }
        let account = Self::provider_account(provider);
        Entry::new(SERVICE, &account)?.set_password(key)?;
        debug!(provider, "stored provider key in OS keychain");
        Ok(())
    }

    /// Fetch a provider API key. `Ok(None)` means there is no entry — the caller's job
    /// is to surface a "set your API key in Settings" message, not to crash.
    pub fn get_provider_key(provider: &str) -> Result<Option<String>, SecretError> {
        let account = Self::provider_account(provider);
        match Entry::new(SERVICE, &account)?.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete a provider API key. Idempotent: removing a non-existent entry is `Ok(())`.
    pub fn delete_provider_key(provider: &str) -> Result<(), SecretError> {
        let account = Self::provider_account(provider);
        match Entry::new(SERVICE, &account)?.delete_credential() {
            Ok(()) => {
                debug!(provider, "removed provider key from OS keychain");
                Ok(())
            }
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => {
                warn!(provider, error = %e, "delete_provider_key failed");
                Err(e.into())
            }
        }
    }

    /// Convenience for the UI: returns `true` if a provider key is set, without exposing
    /// the value itself.
    pub fn has_provider_key(provider: &str) -> Result<bool, SecretError> {
        Self::get_provider_key(provider).map(|o| o.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_rejects_empty_key() {
        let err = SecretStore::set_provider_key("test-empty", "   ").unwrap_err();
        assert!(matches!(err, SecretError::Empty));
    }

    #[test]
    fn provider_account_is_stable() {
        assert_eq!(SecretStore::provider_account("anthropic"), "provider.anthropic.api_key");
        assert_eq!(SecretStore::provider_account("openai"), "provider.openai.api_key");
    }

    // Tests that actually touch the OS keychain are intentionally not included here:
    // they would mutate the user's real credential store in CI / on dev machines,
    // require platform-specific setup (libsecret on Linux), and have no value beyond
    // exercising the `keyring` crate's own test suite. The set/get/delete code path
    // is covered end-to-end via the Settings dialog in the desktop shell.
}
