//! Base model registry — the single static table of models NarrowMind knows
//! how to serve and (Phase 4+) fine-tune.
//!
//! Until this module existed, model identity lived in one hard-coded
//! constructor (`ModelSpec::default_qwen2_5_7b_q4km`) plus stringly comments
//! scattered through timeouts and tool descriptions. Phase 4 (LoRA) and
//! Phase 4.5 (second-base-model comparison) need per-model metadata the
//! `ModelSpec` serving shape never carried: training repo, tokenizer id,
//! VRAM profile, chat-template family, license, quantization caveats.
//!
//! Design choices:
//! - **Static Rust table, not a TOML manifest.** The filesystem-as-truth
//!   principle governs *project state*; a built-in catalog is a code constant
//!   (same reasoning as `retry::timeouts`). When user-defined models become a
//!   real need (Phase 5+/7), a TOML overlay can extend this table without
//!   changing its shape.
//! - **`ModelSpec` stays the serving contract.** The inference manager and
//!   llama.cpp spawn path are untouched; [`BaseModel::to_model_spec`] is the
//!   bridge. The legacy `ModelSpec::default_qwen2_5_7b_q4km()` constructor now
//!   delegates here, pinned by a field-for-field equality test so the
//!   refactor is provably behavior-neutral.
//! - **Exactly one `default: true` entry.** Enforced by test, relied on by
//!   [`default_model`].

use serde::Serialize;

use crate::inference::{KvCacheType, ModelSpec};

/// Serving context window passed to llama.cpp regardless of what the model
/// could address. 4096 keeps the KV cache inside an 8 GB card next to 7-12B
/// Q4 weights; raising it is a per-call decision (`start_inference_server`'s
/// `n_ctx` arg), not a registry property — [`BaseModel::context_window`]
/// records what the model *supports*, this constant records what we *spawn*.
pub const SERVING_N_CTX: u32 = 4096;

/// KV-cache quantization spawned by default on the 8 GB reference card
/// (Phase 4.6 efficiency Tier 1). `Q8_0` is near-lossless and roughly halves KV
/// memory vs f16, freeing VRAM for longer RAG context next to the Q4 weights.
/// Like [`SERVING_N_CTX`] this is what we *spawn*, overridable per call via
/// `start_inference_server`'s `kv_cache_type` arg; `f16` is the exact-baseline
/// opt-out.
pub const SERVING_KV_CACHE: KvCacheType = KvCacheType::Q8_0;

/// Chat-template family the model's GGUF embeds. Informational for Phase 4
/// dataset formatting (the llama.cpp server applies the GGUF's own embedded
/// template at inference time — nothing in our serving path branches on this
/// yet, but SFT data rendering will).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatTemplateFormat {
    /// `<|im_start|>role ... <|im_end|>` — Qwen family.
    ChatMl,
    /// `<start_of_turn>role ... <end_of_turn>` — Gemma family.
    Gemma,
    /// Llama 3 header-tag format.
    Llama,
}

impl ChatTemplateFormat {
    /// Wire string shared with the Python training worker's `chat_template`
    /// param ("chatml" | "gemma" | "llama"). Renaming breaks the contract.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ChatMl => "chatml",
            Self::Gemma => "gemma",
            Self::Llama => "llama",
        }
    }
}

/// VRAM guidance for the reference-hardware check (RTX 3070, 8 GB).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct VramProfile {
    /// Minimum VRAM (GB) to serve the recommended quant with SERVING_N_CTX.
    pub min_vram_gb: f32,
    /// Quantization we point users at by default.
    pub recommended_quant: &'static str,
}

/// One row of the registry. All fields `'static` — this is a compile-time
/// catalog, not runtime state.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct BaseModel {
    /// Stable lookup key, kebab-case. Wire-stable once shipped: project.toml
    /// `base_model` fields will reference these ids from Phase 4 on.
    pub id: &'static str,
    pub display_name: &'static str,
    /// Training base for the Unsloth path (Phase 4). Usually the 4-bit
    /// pre-quantized variant so QLoRA skips the on-the-fly quantization pass.
    pub hf_repo: &'static str,
    /// GGUF source for the llama.cpp serving path.
    pub gguf_repo: &'static str,
    pub gguf_file: &'static str,
    /// Tokenizer for chat-template rendering + Phase 4 seq-len accounting.
    pub tokenizer_id: &'static str,
    pub vram: VramProfile,
    /// What the model supports — NOT what we spawn (see [`SERVING_N_CTX`]).
    pub context_window: u32,
    pub chat_template: ChatTemplateFormat,
    pub license: &'static str,
    /// Deployment caveats a user must read before swapping quants or shipping
    /// to a new domain. Surfaced verbatim in future UI.
    pub quantization_notes: &'static str,
    /// Whether the Phase 4 training path supports this model today.
    pub training_supported: bool,
    /// Exactly one entry in [`REGISTRY`] may set this.
    pub default: bool,
}

impl BaseModel {
    /// Bridge into the serving contract. Field values for the default model
    /// are pinned byte-for-byte to the pre-registry hard-codes by test —
    /// this function is what makes the registry refactor behavior-neutral.
    #[must_use]
    pub fn to_model_spec(&self) -> ModelSpec {
        ModelSpec {
            repo_id: self.gguf_repo.to_string(),
            filename: self.gguf_file.to_string(),
            n_ctx: SERVING_N_CTX,
            n_gpu_layers: -1,
            // Phase 4.6: quantize the KV cache by default for the 8 GB band.
            kv_cache_type: SERVING_KV_CACHE,
            // Forced on by the quantized cache; not an independent default.
            flash_attn: false,
            // Phase 4.6: reuse the prompt cache across requests (host RAM).
            prompt_cache: true,
        }
    }
}

/// The catalog. Ordering is presentation order (default first).
static REGISTRY: &[BaseModel] = &[
    BaseModel {
        id: "qwen2.5-7b-instruct",
        display_name: "Qwen2.5 7B Instruct",
        hf_repo: "unsloth/Qwen2.5-7B-Instruct-bnb-4bit",
        gguf_repo: "bartowski/Qwen2.5-7B-Instruct-GGUF",
        gguf_file: "Qwen2.5-7B-Instruct-Q4_K_M.gguf",
        tokenizer_id: "Qwen/Qwen2.5-7B-Instruct",
        vram: VramProfile {
            min_vram_gb: 5.0,
            recommended_quant: "Q4_K_M",
        },
        context_window: 32_768,
        chat_template: ChatTemplateFormat::ChatMl,
        license: "Apache-2.0",
        quantization_notes: "Standard PTQ (post-training quantization) via \
            llama.cpp imatrix. Q4_K_M is the proven Phase 3 baseline on the \
            reference RTX 3070: full GPU offload, ~71 tok/s generation.",
        training_supported: true,
        default: true,
    },
    BaseModel {
        id: "gemma-4-12b-it",
        display_name: "Gemma 4 12B Instruct",
        // Exact bnb-4bit variant name re-verified at Phase 4.5 entry; the
        // instruct base is the anchor either way.
        hf_repo: "unsloth/gemma-4-12b-it",
        // QAT-aware GGUFs ONLY — see quantization_notes and
        // docs/quantization.md. Generic conversions of the QAT checkpoints
        // measurably destroy accuracy.
        gguf_repo: "unsloth/gemma-4-12B-it-qat-GGUF",
        gguf_file: "gemma-4-12b-it-qat-Q4_K_M.gguf",
        tokenizer_id: "google/gemma-4-12B-it",
        vram: VramProfile {
            min_vram_gb: 6.6,
            recommended_quant: "Q4_K_M (QAT)",
        },
        context_window: 262_144,
        chat_template: ChatTemplateFormat::Gemma,
        license: "Apache-2.0",
        quantization_notes: "Official QAT (quantization-aware training) \
            checkpoints, released 2026-06-05. NEVER convert the QAT weights \
            naively to Q4_0: llama.cpp's F16-scale conversion against \
            BF16-trained scales measurably loses 8.8-15.4 accuracy points \
            across the Gemma 4 family; use Unsloth's QAT-aware dynamic GGUFs \
            only. QLoRA fine-tune needs 8-10 GB VRAM (at the RTX 3070 8 GB \
            floor — monitor for OOM). Turkish quality unverified — eval \
            before production use for Turkish domains.",
        // Unsloth Gemma 4 support confirmed 2026-06 (text/vision/audio/RL,
        // 12B QLoRA on consumer GPUs) — re-verify at Phase 4.5 entry.
        training_supported: true,
        default: false,
    },
];

/// Every registered model, presentation order.
#[must_use]
pub fn all() -> &'static [BaseModel] {
    REGISTRY
}

/// Lookup by stable id. `None` for unknown ids — callers surface the
/// available ids rather than panicking.
#[must_use]
pub fn get(id: &str) -> Option<&'static BaseModel> {
    REGISTRY.iter().find(|m| m.id == id)
}

/// The catalog's default model. The exactly-one-default invariant is enforced
/// by unit test, so the `expect` here is unreachable in a shipped build.
#[must_use]
pub fn default_model() -> &'static BaseModel {
    REGISTRY
        .iter()
        .find(|m| m.default)
        .expect("registry invariant: exactly one default model")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_one_default() {
        let defaults: Vec<_> = all().iter().filter(|m| m.default).collect();
        assert_eq!(defaults.len(), 1, "registry must have exactly one default");
        assert_eq!(defaults[0].id, "qwen2.5-7b-instruct");
    }

    #[test]
    fn lookup_hit_and_miss() {
        assert_eq!(get("qwen2.5-7b-instruct").unwrap().display_name, "Qwen2.5 7B Instruct");
        assert_eq!(get("gemma-4-12b-it").unwrap().display_name, "Gemma 4 12B Instruct");
        assert!(get("nonexistent-model").is_none());
        assert!(get("").is_none());
    }

    /// THE pinning test: the registry-backed default must be byte-identical
    /// to the literals `ModelSpec::default_qwen2_5_7b_q4km()` carried before
    /// the registry refactor. If the first four fail, the refactor changed
    /// serving behavior. The KV fields are a deliberate Phase 4.6 addition
    /// (Q8_0 by default) — pinned here so a future edit can't silently drift them.
    #[test]
    fn default_model_spec_matches_pre_registry_literals() {
        let spec = default_model().to_model_spec();
        assert_eq!(spec.repo_id, "bartowski/Qwen2.5-7B-Instruct-GGUF");
        assert_eq!(spec.filename, "Qwen2.5-7B-Instruct-Q4_K_M.gguf");
        assert_eq!(spec.n_ctx, 4096);
        assert_eq!(spec.n_gpu_layers, -1);
        assert_eq!(spec.kv_cache_type, KvCacheType::Q8_0);
        assert!(!spec.flash_attn);
        assert!(spec.prompt_cache);
    }

    /// And the legacy constructor itself must produce the same thing — it now
    /// delegates here, but callers depending on its exact output must never
    /// notice the indirection.
    #[test]
    fn legacy_constructor_delegates_unchanged() {
        let legacy = ModelSpec::default_qwen2_5_7b_q4km();
        let registry = default_model().to_model_spec();
        assert_eq!(legacy, registry);
    }

    #[test]
    fn every_entry_has_required_metadata() {
        for m in all() {
            assert!(!m.id.is_empty(), "id empty");
            assert!(!m.display_name.is_empty(), "{}: display_name empty", m.id);
            assert!(!m.hf_repo.is_empty(), "{}: hf_repo empty", m.id);
            assert!(!m.gguf_repo.is_empty(), "{}: gguf_repo empty", m.id);
            assert!(!m.gguf_file.is_empty(), "{}: gguf_file empty", m.id);
            assert!(!m.tokenizer_id.is_empty(), "{}: tokenizer_id empty", m.id);
            assert!(!m.license.is_empty(), "{}: license empty", m.id);
            assert!(!m.quantization_notes.is_empty(), "{}: quantization_notes empty", m.id);
            assert!(m.context_window > 0, "{}: context_window zero", m.id);
            assert!(m.vram.min_vram_gb > 0.0, "{}: vram zero", m.id);
            assert!(
                m.gguf_file.ends_with(".gguf"),
                "{}: gguf_file should be a .gguf filename",
                m.id
            );
        }
    }

    #[test]
    fn ids_are_unique_and_kebab_case() {
        let mut seen = std::collections::HashSet::new();
        for m in all() {
            assert!(seen.insert(m.id), "duplicate id: {}", m.id);
            assert!(
                m.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.'),
                "{}: id not kebab-case",
                m.id
            );
        }
    }

    #[test]
    fn default_model_is_trainable() {
        // Phase 4 trains the default; a non-trainable default would dead-end
        // the primary flow.
        assert!(default_model().training_supported);
    }

    #[test]
    fn gemma_entry_is_qat_aware_and_non_default() {
        let g = get("gemma-4-12b-it").unwrap();
        assert!(!g.default, "Phase 4 ships on Qwen; Gemma is the 4.5 comparison");
        assert!(g.training_supported, "Unsloth support confirmed 2026-06");
        assert!(
            g.gguf_repo.to_lowercase().contains("qat"),
            "Gemma 4 GGUF source must be a QAT-aware repo, got `{}`",
            g.gguf_repo
        );
        assert!(g.quantization_notes.contains("NEVER convert the QAT weights"));
        assert!(g.quantization_notes.contains("Turkish quality unverified"));
        assert_eq!(g.chat_template, ChatTemplateFormat::Gemma);
        assert_eq!(g.context_window, 262_144);
        // Fits the reference card for INFERENCE; training is at the floor.
        assert!(g.vram.min_vram_gb <= 8.0);
    }

    #[test]
    fn serving_n_ctx_is_vram_conservative() {
        // SERVING_N_CTX intentionally stays far below what models support —
        // it's a KV-cache budget for the 8 GB reference card, not a model cap.
        for m in all() {
            assert!(m.context_window >= SERVING_N_CTX, "{}: serving ctx exceeds model ctx", m.id);
        }
    }
}
