"""Training hyperparameter schema + validation. Pure module — no torch/unsloth
imports, so it unit-tests in the CPU environment.

The Rust orchestrator owns config assembly (preset + project.toml overrides,
see crates/orchestrator/src/training/preset.rs) and ships the FINAL values
here via RPC params. This module's job is defense: reject obviously broken
configs before a 6 GB model load, with errors a user can act on.
"""

from __future__ import annotations

from dataclasses import dataclass, fields
from typing import Any


@dataclass
class TrainingParams:
    """Mirror of the Rust-side preset shape (KARAR 3, rtx-3070-8gb defaults)."""

    base_model_hf_repo: str = ""
    tokenizer_id: str = ""
    chat_template: str = "chatml"  # chatml | gemma | llama
    load_in_4bit: bool = True
    lora_r: int = 16
    lora_alpha: int = 32
    target_modules: str = "all-linear"
    per_device_train_batch_size: int = 1
    gradient_accumulation_steps: int = 8
    max_seq_length: int = 2048
    learning_rate: float = 2e-4
    warmup_ratio: float = 0.03
    num_train_epochs: int = 3
    lr_scheduler_type: str = "cosine"
    bf16: bool = True
    optim: str = "adamw_8bit"
    gradient_checkpointing: str = "unsloth"
    seed: int = 42
    validation_split: float = 0.10
    save_total_limit: int = 3

    @classmethod
    def from_params(cls, raw: dict[str, Any]) -> "TrainingParams":
        """Build from an RPC params dict, ignoring unknown keys (forward compat)
        and type-checking the knowns. Raises ValueError with a user-actionable
        message on anything that would waste a model load."""
        known = {f.name: f for f in fields(cls)}
        kwargs: dict[str, Any] = {}
        for key, value in raw.items():
            if key not in known:
                continue  # forward compat: newer Rust may send fields we don't know
            kwargs[key] = value
        params = cls(**kwargs)
        params.validate()
        return params

    def validate(self) -> None:
        if not self.base_model_hf_repo:
            raise ValueError("base_model_hf_repo is required (registry hf_repo)")
        if not self.tokenizer_id:
            raise ValueError("tokenizer_id is required (registry tokenizer_id)")
        if self.chat_template not in ("chatml", "gemma", "llama"):
            raise ValueError(f"unknown chat_template `{self.chat_template}`")
        if not (1 <= self.lora_r <= 256):
            raise ValueError(f"lora_r out of range [1, 256]: {self.lora_r}")
        if self.lora_alpha <= 0:
            raise ValueError(f"lora_alpha must be positive: {self.lora_alpha}")
        if not (64 <= self.max_seq_length <= 32768):
            raise ValueError(f"max_seq_length out of range [64, 32768]: {self.max_seq_length}")
        if not (0.0 < self.learning_rate < 1.0):
            raise ValueError(f"learning_rate out of sane range (0, 1): {self.learning_rate}")
        if not (0.0 <= self.warmup_ratio <= 0.5):
            raise ValueError(f"warmup_ratio out of range [0, 0.5]: {self.warmup_ratio}")
        if not (1 <= self.num_train_epochs <= 100):
            raise ValueError(f"num_train_epochs out of range [1, 100]: {self.num_train_epochs}")
        if self.per_device_train_batch_size < 1 or self.gradient_accumulation_steps < 1:
            raise ValueError("batch size and grad accumulation must be >= 1")
        if not (0.0 < self.validation_split <= 0.5):
            raise ValueError(f"validation_split out of range (0, 0.5]: {self.validation_split}")
        # numpy / transformers set_seed reject anything outside u32.
        if not (0 <= self.seed <= 2**32 - 1):
            raise ValueError(f"seed out of range [0, 2**32-1]: {self.seed}")

    def effective_batch_size(self) -> int:
        return self.per_device_train_batch_size * self.gradient_accumulation_steps


# Default export shape for tests / documentation.
DEFAULTS = TrainingParams(
    base_model_hf_repo="unsloth/Qwen2.5-7B-Instruct-bnb-4bit",
    tokenizer_id="Qwen/Qwen2.5-7B-Instruct",
)
