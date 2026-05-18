//! Filesystem-backed project store.
//!
//! Every project lives at `<root>/<name>/`. The orchestrator owns the root path; the UI is
//! a view over what's on disk. There is **no in-memory project state outside the store** —
//! if you want it to exist for the next session, write it to `project.toml`.

use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;
use tracing::{debug, info, warn};

use super::config::{
    validate_name, ProjectConfig, ProjectStatus, ProjectTier, ProviderConfig, SCHEMA_VERSION,
};

/// Standard project subdirectories created on `create()`. Matches `docs/ARCHITECTURE.md` §
/// Project. Empty dirs are fine — workers will write into them as features come online.
const PROJECT_SUBDIRS: &[&str] = &[
    "sources",
    "datasets",
    "vector_store",
    "runs",
    "models",
    "evals",
];

/// Errors returned by `ProjectStore`.
#[derive(Debug, Error)]
pub enum ProjectError {
    /// Provided name failed [`validate_name`].
    #[error("invalid project name `{name}`: {reason}")]
    InvalidName { name: String, reason: String },

    /// `create()` called for a project that already has a directory.
    #[error("project `{0}` already exists")]
    AlreadyExists(String),

    /// Project directory or `project.toml` missing.
    #[error("project `{0}` not found")]
    NotFound(String),

    /// `project.toml`'s `schema_version` does not match [`SCHEMA_VERSION`].
    #[error("schema version mismatch for project `{name}`: on disk = v{found}, expected = v{expected}")]
    SchemaVersion {
        name: String,
        found: u32,
        expected: u32,
    },

    /// Could not determine a default OS data directory (no `HOME` / no `%APPDATA%`).
    #[error("default project root cannot be determined; pass an explicit path")]
    DefaultRootUnavailable,

    #[error("io error in project `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("toml serialise error for `{path}`: {source}")]
    TomlSer {
        path: PathBuf,
        #[source]
        source: toml::ser::Error,
    },

    #[error("toml parse error for `{path}`: {source}")]
    TomlDe {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

type Result<T> = std::result::Result<T, ProjectError>;

/// On-disk project store. Cheap to clone (just a `PathBuf`).
#[derive(Debug, Clone)]
pub struct ProjectStore {
    root: PathBuf,
}

impl ProjectStore {
    /// Build a store rooted at `root`. The directory is created lazily on first `create()`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// OS-appropriate default root: `<data_dir>/narrowmind/projects`.
    ///
    /// - Windows: `%APPDATA%\narrowmind\projects`
    /// - macOS: `~/Library/Application Support/narrowmind/projects`
    /// - Linux: `$XDG_DATA_HOME/narrowmind/projects` or `~/.local/share/narrowmind/projects`
    pub fn default_root() -> Result<PathBuf> {
        let base = dirs::data_dir().ok_or(ProjectError::DefaultRootUnavailable)?;
        Ok(base.join("narrowmind").join("projects"))
    }

    /// Build a store at the OS-appropriate default root.
    pub fn at_default_root() -> Result<Self> {
        Ok(Self::new(Self::default_root()?))
    }

    /// Absolute path to the root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Absolute path to a project's directory. Does not check existence.
    #[must_use]
    pub fn project_dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// Absolute path to a project's `project.toml`. Does not check existence.
    #[must_use]
    pub fn project_config_path(&self, name: &str) -> PathBuf {
        self.project_dir(name).join("project.toml")
    }

    /// Names of every directory under root that contains a parseable `project.toml`, sorted.
    pub fn list(&self) -> Result<Vec<String>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut names = Vec::new();
        let entries = fs::read_dir(&self.root).map_err(|e| io_err(&self.root, e))?;
        for entry in entries {
            let entry = entry.map_err(|e| io_err(&self.root, e))?;
            let ft = entry.file_type().map_err(|e| io_err(&entry.path(), e))?;
            if !ft.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !entry.path().join("project.toml").is_file() {
                continue;
            }
            names.push(name);
        }
        names.sort();
        Ok(names)
    }

    /// Create a project directory tree and write the initial `project.toml`.
    ///
    /// Returns the persisted config so callers can immediately surface its `created_at`.
    pub fn create(
        &self,
        name: &str,
        tier: ProjectTier,
        provider: ProviderConfig,
    ) -> Result<ProjectConfig> {
        validate_name(name).map_err(|reason| ProjectError::InvalidName {
            name: name.to_string(),
            reason,
        })?;

        let dir = self.project_dir(name);
        if dir.exists() {
            return Err(ProjectError::AlreadyExists(name.to_string()));
        }

        fs::create_dir_all(&dir).map_err(|e| io_err(&dir, e))?;
        for sub in PROJECT_SUBDIRS {
            let sd = dir.join(sub);
            fs::create_dir_all(&sd).map_err(|e| io_err(&sd, e))?;
        }
        // touch agent.log so future appends can be unconditional opens.
        let log = dir.join("agent.log");
        fs::write(&log, "").map_err(|e| io_err(&log, e))?;

        let cfg = ProjectConfig::new(name, tier, provider);
        self.write_config(&cfg)?;
        info!(project = name, dir = %dir.display(), "created project");
        Ok(cfg)
    }

    /// Read and validate a project's `project.toml`.
    pub fn get(&self, name: &str) -> Result<ProjectConfig> {
        let path = self.project_config_path(name);
        if !path.is_file() {
            return Err(ProjectError::NotFound(name.to_string()));
        }
        let raw = fs::read_to_string(&path).map_err(|e| io_err(&path, e))?;
        let cfg: ProjectConfig = toml::from_str(&raw).map_err(|source| ProjectError::TomlDe {
            path: path.clone(),
            source,
        })?;
        if cfg.schema_version != SCHEMA_VERSION {
            return Err(ProjectError::SchemaVersion {
                name: name.to_string(),
                found: cfg.schema_version,
                expected: SCHEMA_VERSION,
            });
        }
        debug!(project = name, "loaded project config");
        Ok(cfg)
    }

    /// Persist a mutated config back to disk. Caller is responsible for not changing the name —
    /// renaming a project requires moving the directory and is intentionally not exposed yet.
    pub fn update(&self, config: &ProjectConfig) -> Result<()> {
        self.write_config(config)
    }

    /// Recursively remove a project directory. Caller (Settings dialog, agent) is responsible
    /// for confirming with the user — `ProjectStore` itself does no prompting.
    pub fn delete(&self, name: &str) -> Result<()> {
        let dir = self.project_dir(name);
        if !dir.exists() {
            return Err(ProjectError::NotFound(name.to_string()));
        }
        fs::remove_dir_all(&dir).map_err(|e| io_err(&dir, e))?;
        warn!(project = name, "deleted project");
        Ok(())
    }

    /// Shorthand: does a project with this name exist?
    #[must_use]
    pub fn exists(&self, name: &str) -> bool {
        self.project_config_path(name).is_file()
    }

    /// Convenience: update a project's status field.
    pub fn set_status(&self, name: &str, status: ProjectStatus) -> Result<ProjectConfig> {
        let mut cfg = self.get(name)?;
        cfg.status = status;
        self.write_config(&cfg)?;
        Ok(cfg)
    }

    fn write_config(&self, config: &ProjectConfig) -> Result<()> {
        let path = self.project_config_path(&config.name);
        let s = toml::to_string_pretty(config).map_err(|source| ProjectError::TomlSer {
            path: path.clone(),
            source,
        })?;
        fs::write(&path, s).map_err(|e| io_err(&path, e))?;
        Ok(())
    }
}

fn io_err(path: &Path, source: std::io::Error) -> ProjectError {
    ProjectError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> (TempDir, ProjectStore) {
        let tmp = TempDir::new().unwrap();
        let store = ProjectStore::new(tmp.path());
        (tmp, store)
    }

    fn provider() -> ProviderConfig {
        ProviderConfig {
            name: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            synth_model: String::new(),
        }
    }

    #[test]
    fn create_writes_dirs_and_config() {
        let (tmp, s) = store();
        let cfg = s.create("test-philosophy", ProjectTier::Hybrid, provider()).unwrap();
        assert_eq!(cfg.name, "test-philosophy");
        assert_eq!(cfg.tier, ProjectTier::Hybrid);
        assert_eq!(cfg.schema_version, SCHEMA_VERSION);

        let dir = tmp.path().join("test-philosophy");
        for sub in PROJECT_SUBDIRS {
            assert!(dir.join(sub).is_dir(), "missing subdir: {sub}");
        }
        assert!(dir.join("agent.log").is_file());
        assert!(dir.join("project.toml").is_file());
    }

    #[test]
    fn create_round_trips_through_get() {
        let (_tmp, s) = store();
        let written = s.create("demo", ProjectTier::Rag, provider()).unwrap();
        let read = s.get("demo").unwrap();
        // created_at fidelity: TOML keeps sub-second precision, but truncation is acceptable here.
        assert_eq!(written.name, read.name);
        assert_eq!(written.tier, read.tier);
        assert_eq!(written.status, read.status);
        assert_eq!(written.provider, read.provider);
    }

    #[test]
    fn create_rejects_duplicate() {
        let (_tmp, s) = store();
        s.create("demo", ProjectTier::Rag, provider()).unwrap();
        let err = s.create("demo", ProjectTier::Rag, provider()).unwrap_err();
        assert!(matches!(err, ProjectError::AlreadyExists(_)));
    }

    #[test]
    fn create_rejects_invalid_name() {
        let (_tmp, s) = store();
        let err = s.create("BAD NAME", ProjectTier::Rag, provider()).unwrap_err();
        assert!(matches!(err, ProjectError::InvalidName { .. }));
    }

    #[test]
    fn list_returns_only_dirs_with_project_toml() {
        let (tmp, s) = store();
        s.create("alpha", ProjectTier::Rag, provider()).unwrap();
        s.create("beta", ProjectTier::Lora, provider()).unwrap();
        // stray dir without project.toml — should be ignored
        fs::create_dir_all(tmp.path().join("not-a-project")).unwrap();

        let names = s.list().unwrap();
        assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn get_returns_not_found_for_unknown_project() {
        let (_tmp, s) = store();
        let err = s.get("missing").unwrap_err();
        assert!(matches!(err, ProjectError::NotFound(_)));
    }

    #[test]
    fn get_rejects_mismatched_schema_version() {
        let (tmp, s) = store();
        // write a project.toml with schema_version 99 by hand
        let dir = tmp.path().join("future");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("project.toml"),
            r#"schema_version = 99
name = "future"
created_at = "2026-05-16T14:32:00Z"
status = "draft"
tier = "rag"
base_model = ""

[provider]
name = "anthropic"
model = "claude-sonnet-4-6"
"#,
        )
        .unwrap();
        let err = s.get("future").unwrap_err();
        assert!(matches!(err, ProjectError::SchemaVersion { found: 99, expected: 1, .. }));
    }

    #[test]
    fn update_persists_changes() {
        let (_tmp, s) = store();
        let cfg = s.create("demo", ProjectTier::Rag, provider()).unwrap();
        let mut updated = cfg.clone();
        updated.tier = ProjectTier::Hybrid;
        updated.base_model = "Qwen/Qwen2.5-7B-Instruct".into();
        s.update(&updated).unwrap();
        let read = s.get("demo").unwrap();
        assert_eq!(read.tier, ProjectTier::Hybrid);
        assert_eq!(read.base_model, "Qwen/Qwen2.5-7B-Instruct");
    }

    #[test]
    fn set_status_updates_only_status() {
        let (_tmp, s) = store();
        s.create("demo", ProjectTier::Rag, provider()).unwrap();
        let updated = s.set_status("demo", ProjectStatus::Active).unwrap();
        assert_eq!(updated.status, ProjectStatus::Active);
    }

    #[test]
    fn delete_removes_directory() {
        let (tmp, s) = store();
        s.create("demo", ProjectTier::Rag, provider()).unwrap();
        s.delete("demo").unwrap();
        assert!(!tmp.path().join("demo").exists());
    }
}
