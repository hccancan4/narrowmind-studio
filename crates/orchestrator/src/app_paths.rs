//! OS-aware app data paths.
//!
//! All on-disk state the studio owns is rooted under `<data_dir>/narrowmind/`:
//!
//! - `<data_dir>/narrowmind/projects/<name>/`  — project state ([`crate::project::ProjectStore`])
//! - `<data_dir>/narrowmind/hf-cache/`         — `HuggingFace` cache (`HF_HOME` redirect)
//!
//! Concentrating everything under one root means users have a single place to find,
//! back up, or wipe state, and stops `~/.cache/huggingface/` from quietly ballooning.

use std::collections::BTreeMap;
use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppPathError {
    #[error("could not determine OS data dir; pass an explicit path")]
    DataDirUnavailable,
}

/// Root for everything `NarrowMind` keeps on disk.
pub fn app_data_root() -> Result<PathBuf, AppPathError> {
    let base = dirs::data_dir().ok_or(AppPathError::DataDirUnavailable)?;
    Ok(base.join("narrowmind"))
}

/// Directory we point `HF_HOME` at so `HuggingFace` downloads (datasets, models) land in
/// the studio's own tree rather than the user's global cache.
pub fn hf_cache_dir() -> Result<PathBuf, AppPathError> {
    Ok(app_data_root()?.join("hf-cache"))
}

/// Environment variables to apply to every Python sidecar so `HuggingFace`'s three cache
/// locations (`HF_HOME`, `HF_HUB_CACHE`, `TRANSFORMERS_CACHE`) all land under
/// [`hf_cache_dir`]. Pass to [`crate::worker::PythonRunner::with_env`] or merge into
/// `runner.envs`.
///
/// Creates the directory on disk as a side-effect so the child process never has to fight a
/// missing parent dir on first use.
pub fn hf_cache_env() -> Result<BTreeMap<String, String>, AppPathError> {
    let hf = hf_cache_dir()?;
    std::fs::create_dir_all(&hf).ok();
    let mut env = BTreeMap::new();
    env.insert("HF_HOME".into(), hf.display().to_string());
    env.insert("HF_HUB_CACHE".into(), hf.join("hub").display().to_string());
    env.insert(
        "TRANSFORMERS_CACHE".into(),
        hf.join("transformers").display().to_string(),
    );
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_compose_under_one_root() {
        let root = app_data_root().unwrap();
        let hf = hf_cache_dir().unwrap();
        assert!(hf.starts_with(&root));
        assert!(hf.ends_with("hf-cache"));
    }

    #[test]
    fn hf_env_has_three_keys() {
        let env = hf_cache_env().unwrap();
        assert!(env.contains_key("HF_HOME"));
        assert!(env.contains_key("HF_HUB_CACHE"));
        assert!(env.contains_key("TRANSFORMERS_CACHE"));
    }
}
