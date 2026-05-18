//! `project.toml` v1 schema. The file is the source of truth for a single DSLM build.
//!
//! ```toml
//! schema_version = 1
//! name = "test-philosophy"
//! created_at = "2026-05-16T14:32:00Z"
//! status = "draft"
//! tier = "rag"
//! base_model = ""
//!
//! [provider]
//! name = "anthropic"
//! model = "claude-sonnet-4-6"
//! ```
//!
//! `schema_version` is mandatory and validated on read; bumping it requires a migration step
//! in the orchestrator (none exist yet — v1 only).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Current `project.toml` schema version. Bump this and add a migration in
/// `ProjectStore::load_config` when a breaking schema change ships.
pub const SCHEMA_VERSION: u32 = 1;

/// Top-level project metadata persisted to `project.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectConfig {
    /// Migration anchor. Must be [`SCHEMA_VERSION`] on read; refuse otherwise.
    pub schema_version: u32,
    /// Project name (also the directory name on disk).
    pub name: String,
    /// When the project directory was created. UTC, RFC 3339 in TOML.
    pub created_at: DateTime<Utc>,
    /// Lifecycle state — drives sidebar grouping and which actions the UI offers.
    pub status: ProjectStatus,
    /// Which DSLM technique this project is targeting.
    pub tier: ProjectTier,
    /// Base model id, e.g. `Qwen/Qwen2.5-7B-Instruct`. Empty until Phase 3+ wires model picking.
    #[serde(default)]
    pub base_model: String,
    /// Which LLM provider drives the *agent loop* (not the trained model).
    pub provider: ProviderConfig,
    /// Synthetic Q&A generation config (set by `generate_sft` on first run).
    /// Optional + back-compat: old project.toml files without this section still parse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synth: Option<SynthConfig>,
}

/// Recorded once per project on the first `generate_sft` run so subsequent runs reproduce
/// the same train/eval split. Per Phase 2 decision F.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SynthConfig {
    /// Seed used to shuffle Q&A pairs before splitting into train + held-out eval.
    pub split_seed: u64,
}

/// Lifecycle state of a project.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProjectStatus {
    #[default]
    Draft,
    Active,
    Archived,
}

/// DSLM tier targeted by the project (see `docs/ARCHITECTURE.md` § DSLM Tiers).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProjectTier {
    /// Tier 0 — RAG only. No fine-tuning.
    #[default]
    Rag,
    /// Tier 1 — `LoRA` / `QLoRA` fine-tune.
    Lora,
    /// Tier 2 — Hybrid (`LoRA` + RAG).
    Hybrid,
}

/// LLM provider configuration for the agent loop.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderConfig {
    /// Provider id (e.g. `"anthropic"`).
    pub name: String,
    /// Default model id for the agent loop. Pulled from app Settings on project create.
    pub model: String,
    /// Optional cheaper model dedicated to bulk synthetic-data generation
    /// (`generate_sft` and similar). Empty string means "fall back to `model`".
    /// Treat as an override for cost-sensitive batch work — the agent loop itself
    /// keeps using `model` for reasoning and tool calls. Loaded as empty string
    /// when missing from older project.toml files so the field is back-compatible.
    #[serde(default)]
    pub synth_model: String,
}

impl ProjectConfig {
    /// Create a fresh config with `now_utc()` as the timestamp and v1 schema.
    #[must_use]
    pub fn new(name: impl Into<String>, tier: ProjectTier, provider: ProviderConfig) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            name: name.into(),
            created_at: Utc::now(),
            status: ProjectStatus::Draft,
            tier,
            base_model: String::new(),
            provider,
            synth: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Name validation
// ---------------------------------------------------------------------------

/// Project names map to filesystem directory names, are passed to git commands, and show up
/// in URLs — so we restrict them to a safe, predictable charset.
///
/// Rule: must start with a lowercase letter or digit, followed by 1..=63 of
/// `[a-z0-9-]`, total length 2..=64.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.len() < 2 || name.len() > 64 {
        return Err(format!("must be 2-64 characters (got {})", name.len()));
    }
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err("must be 2-64 characters (got 0)".into());
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err("must start with a lowercase letter or digit".into());
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return Err(format!("invalid character `{c}`; only [a-z0-9-] allowed"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_validation_accepts_typical_names() {
        for ok in ["a1", "test-philosophy", "nous-philosophy", "x9", "abc-123-def"] {
            assert!(validate_name(ok).is_ok(), "expected ok: {ok}");
        }
    }

    #[test]
    fn name_validation_rejects_bad_names() {
        for bad in ["", "a", "-leading", "UPPER", "with_underscore", "with space", "a$b"] {
            assert!(validate_name(bad).is_err(), "expected err: {bad}");
        }
    }

    #[test]
    fn name_validation_enforces_length_ceiling() {
        let too_long = "a".repeat(65);
        assert!(validate_name(&too_long).is_err());
    }

    #[test]
    fn defaults_use_v1_and_draft_rag() {
        let cfg = ProjectConfig::new(
            "demo",
            ProjectTier::Rag,
            ProviderConfig {
                name: "anthropic".into(),
                model: "claude-opus-4-7".into(),
                synth_model: String::new(),
            },
        );
        assert_eq!(cfg.schema_version, SCHEMA_VERSION);
        assert_eq!(cfg.status, ProjectStatus::Draft);
        assert_eq!(cfg.tier, ProjectTier::Rag);
        assert!(cfg.base_model.is_empty());
    }

    #[test]
    fn toml_round_trip() {
        let cfg = ProjectConfig::new(
            "demo",
            ProjectTier::Hybrid,
            ProviderConfig {
                name: "anthropic".into(),
                model: "claude-opus-4-7".into(),
                synth_model: String::new(),
            },
        );
        let s = toml::to_string(&cfg).unwrap();
        let back: ProjectConfig = toml::from_str(&s).unwrap();
        assert_eq!(cfg, back);
    }
}
