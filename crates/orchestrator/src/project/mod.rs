//! Project state: `project.toml` schema, on-disk store, name validation.
//!
//! The filesystem is the source of truth. No in-memory project state outside `ProjectStore`.

pub mod config;
pub mod store;

pub use config::{
    validate_name, ProjectConfig, ProjectStatus, ProjectTier, ProviderConfig, RagConfig,
    RetrievalMode, SynthConfig, TrainingConfig, SCHEMA_VERSION,
};
pub use store::{ProjectError, ProjectStore};
