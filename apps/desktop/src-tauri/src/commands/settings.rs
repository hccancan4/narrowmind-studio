//! Settings dialog backing: API key storage and presence checks.
//!
//! The actual key never leaves the OS keychain — the front-end only ever asks
//! `has_provider_key` (boolean) or pushes a new value via `set_provider_key`.

// Tauri commands receive deserialised owned types (String, etc.) from the JS bridge;
// clippy's needless_pass_by_value lint can't see the bridge contract.
#![allow(clippy::needless_pass_by_value)]

use narrowmind_orchestrator::SecretStore;

#[tauri::command]
pub fn set_provider_key(provider: String, api_key: String) -> Result<(), String> {
    SecretStore::set_provider_key(&provider, &api_key).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn has_provider_key(provider: String) -> Result<bool, String> {
    SecretStore::has_provider_key(&provider).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_provider_key(provider: String) -> Result<(), String> {
    SecretStore::delete_provider_key(&provider).map_err(|e| e.to_string())
}
