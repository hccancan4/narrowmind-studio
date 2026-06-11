//! Hardware-profile-keyed training presets (KARAR 3).
//!
//! Same house pattern as `models::REGISTRY` and `retry::timeouts`: a static
//! table of known-good configurations, each value carrying its rationale.
//! Users override individual knobs via `project.toml [training]`; the preset
//! supplies everything they don't.

use serde::Serialize;

/// One complete training configuration for a hardware profile.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct TrainingPreset {
    /// Stable lookup key (project.toml `[training] preset = "..."`).
    pub id: &'static str,
    pub backend: &'static str,
    pub load_in_4bit: bool,
    pub lora_r: u32,
    pub lora_alpha: u32,
    pub target_modules: &'static str,
    pub per_device_train_batch_size: u32,
    pub gradient_accumulation_steps: u32,
    pub max_seq_length: u32,
    pub learning_rate: f64,
    pub warmup_ratio: f64,
    pub num_train_epochs: u32,
    pub lr_scheduler_type: &'static str,
    pub bf16: bool,
    pub optim: &'static str,
    pub gradient_checkpointing: &'static str,
    pub validation_split: f64,
    pub save_total_limit: u32,
}

/// The reference preset: QLoRA on a 7B-class base inside 8 GB of VRAM.
///
/// OOM degrade path (documented per KARAR 3): if training OOMs, the FIRST
/// knob to lower is `max_seq_length`, stepping 2048 → 1536 → 1024. Sequence
/// length dominates activation memory; rank/batch stay put until seq-len
/// relief is exhausted. Below 1024, stop and reassess (surface rule: ask).
pub static RTX_3070_8GB: TrainingPreset = TrainingPreset {
    id: "rtx-3070-8gb",
    backend: "unsloth",
    load_in_4bit: true,
    lora_r: 16,
    lora_alpha: 32,
    target_modules: "all-linear",
    per_device_train_batch_size: 1,
    gradient_accumulation_steps: 8, // effective batch 8
    max_seq_length: 2048,
    learning_rate: 2e-4,
    warmup_ratio: 0.03,
    num_train_epochs: 3,
    lr_scheduler_type: "cosine",
    bf16: true, // Ampere supports bf16 natively
    optim: "adamw_8bit",
    gradient_checkpointing: "unsloth",
    validation_split: 0.10,
    save_total_limit: 3,
};

static PRESETS: &[&TrainingPreset] = &[&RTX_3070_8GB];

#[must_use]
pub fn get(id: &str) -> Option<&'static TrainingPreset> {
    PRESETS.iter().copied().find(|p| p.id == id)
}

#[must_use]
pub fn default_preset() -> &'static TrainingPreset {
    &RTX_3070_8GB
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtx_3070_values_match_karar_3() {
        let p = default_preset();
        assert_eq!(p.id, "rtx-3070-8gb");
        assert_eq!(p.backend, "unsloth");
        assert!(p.load_in_4bit);
        assert_eq!(p.lora_r, 16);
        assert_eq!(p.lora_alpha, 32);
        assert_eq!(p.target_modules, "all-linear");
        assert_eq!(p.per_device_train_batch_size, 1);
        assert_eq!(p.gradient_accumulation_steps, 8);
        assert_eq!(p.max_seq_length, 2048);
        assert!((p.learning_rate - 2e-4).abs() < f64::EPSILON);
        assert!((p.warmup_ratio - 0.03).abs() < f64::EPSILON);
        assert_eq!(p.num_train_epochs, 3);
        assert_eq!(p.lr_scheduler_type, "cosine");
        assert!(p.bf16);
        assert_eq!(p.optim, "adamw_8bit");
        assert_eq!(p.gradient_checkpointing, "unsloth");
    }

    #[test]
    fn lookup_hit_and_miss() {
        assert!(get("rtx-3070-8gb").is_some());
        assert!(get("h100-80gb").is_none());
    }
}
