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
    /// Retrieval-stack tuning (top_k, hybrid weights, RRF fusion constant).
    /// Defaults are filled in by [`RagConfig::default`] when the field is missing
    /// from older project.toml files — this is the back-compat path for projects
    /// created before Phase 3.5 added hybrid retrieval.
    #[serde(default)]
    pub rag: RagConfig,
    /// QLoRA training configuration (Phase 4). Preset id + optional per-knob
    /// overrides; missing section = all defaults (rtx-3070-8gb preset).
    #[serde(default)]
    pub training: TrainingConfig,
}

/// Recorded once per project on the first `generate_sft` run so subsequent runs reproduce
/// the same train/eval split. Per Phase 2 decision F.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SynthConfig {
    /// Seed used to shuffle Q&A pairs before splitting into train + held-out eval.
    pub split_seed: u64,
}

/// Retrieval-stack config introduced in Phase 3.5. Lives in `project.toml` under
/// `[rag]`; older configs that pre-date this section pick up the
/// [`RagConfig::default`] values which match the post-3.5 production defaults.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RagConfig {
    /// Which retriever the agent tools default to. `hybrid` (dense + BM25 + RRF)
    /// is the post-3.5 default because it closes the proper-noun recall gap that
    /// pure dense had. `dense` and `sparse` remain selectable for A/B testing.
    #[serde(default = "RagConfig::default_mode")]
    pub retrieval_mode: RetrievalMode,
    /// Number of chunks returned to the caller.
    #[serde(default = "RagConfig::default_top_k")]
    pub top_k: u32,
    /// Candidate pool size from the dense retriever before RRF fusion.
    #[serde(default = "RagConfig::default_hybrid_k_dense")]
    pub hybrid_k_dense: u32,
    /// Candidate pool size from the sparse (BM25) retriever before RRF fusion.
    #[serde(default = "RagConfig::default_hybrid_k_sparse")]
    pub hybrid_k_sparse: u32,
    /// Reciprocal Rank Fusion smoothing constant. 60 is canonical
    /// (Cormack/Clarke 2009); don't change without re-running the eval set.
    #[serde(default = "RagConfig::default_rrf_k")]
    pub rrf_k: u32,
}

impl RagConfig {
    fn default_mode() -> RetrievalMode {
        RetrievalMode::Hybrid
    }
    fn default_top_k() -> u32 {
        5
    }
    fn default_hybrid_k_dense() -> u32 {
        10
    }
    fn default_hybrid_k_sparse() -> u32 {
        10
    }
    fn default_rrf_k() -> u32 {
        60
    }
}

impl Default for RagConfig {
    fn default() -> Self {
        Self {
            retrieval_mode: Self::default_mode(),
            top_k: Self::default_top_k(),
            hybrid_k_dense: Self::default_hybrid_k_dense(),
            hybrid_k_sparse: Self::default_hybrid_k_sparse(),
            rrf_k: Self::default_rrf_k(),
        }
    }
}

/// Phase 4 `[training]` section: a hardware-preset id plus optional overrides.
/// `None` override = use the preset's value (see `crate::training::preset`).
/// Back-compat by construction: every field is serde-defaulted, so pre-Phase-4
/// project.toml files load with the rtx-3070-8gb defaults.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrainingConfig {
    /// Preset key into `crate::training::preset` (default "rtx-3070-8gb").
    #[serde(default = "TrainingConfig::default_preset_id")]
    pub preset: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lora_r: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lora_alpha: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_seq_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learning_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_train_epochs: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_device_train_batch_size: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gradient_accumulation_steps: Option<u32>,
    /// Explicit training seed. When absent, the tool derives one from
    /// `[synth] split_seed` (same project → reproducible by default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// Activity-based deadline for the streaming worker call: a run is
    /// considered hung after this many seconds of SILENCE (notifications
    /// refresh the timer). 600 s default — model download + first-step
    /// compile both stay comfortably under it on the reference machine.
    #[serde(default = "TrainingConfig::default_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
    /// Most recent run id, written by the manager when a run reaches a
    /// terminal state (KARAR 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_id: Option<String>,
}

impl TrainingConfig {
    fn default_preset_id() -> String {
        "rtx-3070-8gb".into()
    }
    fn default_idle_timeout_secs() -> u64 {
        600
    }
}

impl Default for TrainingConfig {
    fn default() -> Self {
        Self {
            preset: Self::default_preset_id(),
            lora_r: None,
            lora_alpha: None,
            max_seq_length: None,
            learning_rate: None,
            num_train_epochs: None,
            per_device_train_batch_size: None,
            gradient_accumulation_steps: None,
            seed: None,
            idle_timeout_secs: Self::default_idle_timeout_secs(),
            last_run_id: None,
        }
    }
}

/// Retriever variant selected by `[rag] retrieval_mode` and by the
/// `query_index` / `rag_chat` agent tool `mode` arg.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RetrievalMode {
    /// BGE-small cosine similarity only. Phase 3 baseline.
    Dense,
    /// BM25 over the `text` column only. Strong for exact-term / proper-noun queries.
    Sparse,
    /// Reciprocal Rank Fusion across both retrievers. The post-3.5 default.
    #[default]
    Hybrid,
}

impl RetrievalMode {
    /// Stable wire string used by JSON-RPC params + tool args.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Dense => "dense",
            Self::Sparse => "sparse",
            Self::Hybrid => "hybrid",
        }
    }
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
            rag: RagConfig::default(),
            training: TrainingConfig::default(),
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

    #[test]
    fn rag_defaults_match_phase_35_decision() {
        let r = RagConfig::default();
        assert!(matches!(r.retrieval_mode, RetrievalMode::Hybrid));
        assert_eq!(r.top_k, 5);
        assert_eq!(r.hybrid_k_dense, 10);
        assert_eq!(r.hybrid_k_sparse, 10);
        assert_eq!(r.rrf_k, 60);
    }

    #[test]
    fn rag_section_missing_from_old_project_toml_uses_defaults() {
        // Realistic Phase 3 project.toml — no [rag] section. Must still parse and
        // fall back to hybrid defaults so existing projects don't break on upgrade.
        let old_toml = r#"
schema_version = 1
name = "deneme1-faz2"
created_at = "2026-05-16T22:46:56.266694300Z"
status = "draft"
tier = "rag"
base_model = ""

[provider]
name = "anthropic"
model = "claude-sonnet-4-6"

[synth]
split_seed = 5405776840459666338
"#;
        let cfg: ProjectConfig = toml::from_str(old_toml).expect("must parse pre-3.5 toml");
        assert_eq!(cfg.name, "deneme1-faz2");
        // Default-filled rag section
        assert!(matches!(cfg.rag.retrieval_mode, RetrievalMode::Hybrid));
        assert_eq!(cfg.rag.top_k, 5);
        // Also: synth_model on provider defaults to empty (from earlier fix, regression check).
        assert!(cfg.provider.synth_model.is_empty());
    }

    #[test]
    fn rag_section_partial_override_keeps_other_defaults() {
        // User edits one knob (e.g. swap to dense for A/B) — others stay default.
        let partial = r#"
schema_version = 1
name = "x"
created_at = "2026-05-16T22:46:56Z"
status = "draft"
tier = "rag"

[provider]
name = "anthropic"
model = "m"

[rag]
retrieval_mode = "dense"
"#;
        let cfg: ProjectConfig = toml::from_str(partial).expect("partial rag section must parse");
        assert!(matches!(cfg.rag.retrieval_mode, RetrievalMode::Dense));
        assert_eq!(cfg.rag.top_k, 5);
        assert_eq!(cfg.rag.hybrid_k_dense, 10);
        assert_eq!(cfg.rag.rrf_k, 60);
    }

    #[test]
    fn retrieval_mode_wire_strings_are_stable() {
        // The JSON-RPC contract with workers/py depends on these strings.
        // Renaming any of them is a breaking change.
        assert_eq!(RetrievalMode::Dense.as_str(), "dense");
        assert_eq!(RetrievalMode::Sparse.as_str(), "sparse");
        assert_eq!(RetrievalMode::Hybrid.as_str(), "hybrid");
    }
}
