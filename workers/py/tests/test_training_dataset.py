"""Pure-python tests for the training worker's CPU-safe pieces: config
validation, dataset loading/format/split, truncation pre-screen, and the
run-directory bookkeeping. No torch/unsloth imports — these run in the
normal workers/py environment."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from narrowmind_workers.training.config import DEFAULTS, TrainingParams
from narrowmind_workers.training.dataset import (
    load_sft_pairs,
    split_train_validation,
    to_messages,
    truncation_prescreen,
)
from narrowmind_workers.training.runner import (
    ALL_LINEAR_MODULES,
    _latest_checkpoint,
    append_metric,
    resolve_target_modules,
    run_dir,
    write_status,
)

# ---------------------------------------------------------------------------
# config
# ---------------------------------------------------------------------------


def test_defaults_match_rtx_3070_preset() -> None:
    """KARAR 3 values pinned — drift between Rust preset and Python defaults
    would silently change training behavior."""
    d = DEFAULTS
    assert d.load_in_4bit is True
    assert d.lora_r == 16
    assert d.lora_alpha == 32
    assert d.target_modules == "all-linear"
    assert d.per_device_train_batch_size == 1
    assert d.gradient_accumulation_steps == 8
    assert d.effective_batch_size() == 8
    assert d.max_seq_length == 2048
    assert d.learning_rate == pytest.approx(2e-4)
    assert d.warmup_ratio == pytest.approx(0.03)
    assert d.num_train_epochs == 3
    assert d.lr_scheduler_type == "cosine"
    assert d.bf16 is True
    assert d.optim == "adamw_8bit"
    assert d.gradient_checkpointing == "unsloth"


def test_from_params_ignores_unknown_keys() -> None:
    tp = TrainingParams.from_params(
        {
            "base_model_hf_repo": "unsloth/x",
            "tokenizer_id": "x/t",
            "lora_r": 8,
            "some_future_field": 123,
        }
    )
    assert tp.lora_r == 8
    assert not hasattr(tp, "some_future_field")


@pytest.mark.parametrize(
    "bad",
    [
        {"tokenizer_id": "t"},  # missing repo
        {"base_model_hf_repo": "r"},  # missing tokenizer
        {"base_model_hf_repo": "r", "tokenizer_id": "t", "chat_template": "mistral"},
        {"base_model_hf_repo": "r", "tokenizer_id": "t", "lora_r": 0},
        {"base_model_hf_repo": "r", "tokenizer_id": "t", "max_seq_length": 16},
        {"base_model_hf_repo": "r", "tokenizer_id": "t", "learning_rate": 2.0},
        {"base_model_hf_repo": "r", "tokenizer_id": "t", "validation_split": 0.0},
        # u64 seed from a buggy caller: numpy/set_seed only accept u32.
        {"base_model_hf_repo": "r", "tokenizer_id": "t", "seed": 2**32},
        {"base_model_hf_repo": "r", "tokenizer_id": "t", "seed": -1},
    ],
)
def test_validation_rejects_broken_configs(bad: dict) -> None:
    with pytest.raises(ValueError):
        TrainingParams.from_params(bad)


def test_resolve_target_modules_expands_all_linear() -> None:
    """Regression: 'all-linear' must become an explicit list, never be passed as
    a string. The installed Unsloth/PEFT iterates a bare string into a set of
    characters ({'a','l','-','i','n','e','r'}) and fails the run with
    'Target modules ... not found in the base model'."""
    assert resolve_target_modules("all-linear") == ALL_LINEAR_MODULES
    assert "q_proj" in resolve_target_modules("all-linear")
    assert "down_proj" in resolve_target_modules("all-linear")
    # a comma-separated custom spec -> list of names
    assert resolve_target_modules("q_proj, v_proj") == ["q_proj", "v_proj"]
    # a bare module name must NOT become a set of characters
    assert resolve_target_modules("q_proj") == ["q_proj"]
    # the result is always a list (the type PEFT needs), never a str
    assert isinstance(resolve_target_modules("all-linear"), list)


# ---------------------------------------------------------------------------
# dataset
# ---------------------------------------------------------------------------


def _write_sft(path: Path, n: int) -> None:
    with path.open("w", encoding="utf-8") as f:
        for i in range(n):
            f.write(
                json.dumps(
                    {"question": f"q{i}", "answer": f"a{i}", "source_chunk_id": f"ck{i}"}
                )
                + "\n"
            )


def test_load_sft_pairs_skips_malformed_lines(tmp_path: Path) -> None:
    p = tmp_path / "sft.jsonl"
    with p.open("w", encoding="utf-8") as f:
        f.write('{"question": "q", "answer": "a", "source_chunk_id": "c"}\n')
        f.write("not json\n")
        f.write('{"question": "", "answer": "a"}\n')  # empty q -> skip
        f.write('{"question": "q2", "answer": "a2"}\n')
    pairs = load_sft_pairs(p)
    assert [p["question"] for p in pairs] == ["q", "q2"]


def test_to_messages_shape() -> None:
    msgs = to_messages({"question": "Why?", "answer": "Because."})
    assert msgs == [
        {"role": "user", "content": "Why?"},
        {"role": "assistant", "content": "Because."},
    ]


def test_split_is_seeded_and_deterministic(tmp_path: Path) -> None:
    p = tmp_path / "sft.jsonl"
    _write_sft(p, 100)
    pairs = load_sft_pairs(p)
    t1, v1 = split_train_validation(pairs, 0.10, seed=1234)
    t2, v2 = split_train_validation(pairs, 0.10, seed=1234)
    assert t1 == t2 and v1 == v2, "same seed -> same split"
    assert len(v1) == 10 and len(t1) == 90
    t3, v3 = split_train_validation(pairs, 0.10, seed=9999)
    assert v3 != v1, "different seed -> different split (overwhelmingly likely)"


def test_split_validation_minimum_one() -> None:
    pairs = [{"question": f"q{i}", "answer": "a"} for i in range(5)]
    train, val = split_train_validation(pairs, 0.10, seed=1)
    assert len(val) == 1, "round(5*0.1)=0 would break eval_loss; floor is 1"
    assert len(train) == 4


def test_truncation_prescreen_counts_oversized() -> None:
    pairs = [
        {"question": "short", "answer": "short"},
        {"question": "x" * 9000, "answer": "y" * 9000},  # 18k chars >> 2048*4
    ]
    stats = truncation_prescreen(pairs, max_seq_length=2048)
    assert stats["pairs"] == 2
    assert stats["estimated_over_budget"] == 1
    assert stats["char_budget"] == 8192


# ---------------------------------------------------------------------------
# runner bookkeeping (pure parts)
# ---------------------------------------------------------------------------


def test_write_status_merges_and_is_atomic(tmp_path: Path) -> None:
    rd = run_dir(tmp_path, "run1")
    rd.mkdir(parents=True)
    write_status(rd, run_id="run1", status="running", step=0)
    write_status(rd, step=42, epoch=1.5)
    data = json.loads((rd / "status.json").read_text(encoding="utf-8"))
    assert data["run_id"] == "run1"
    assert data["status"] == "running"  # preserved by merge
    assert data["step"] == 42
    assert data["epoch"] == 1.5
    assert not (rd / "status.json.tmp").exists(), "tmp swapped away"


def test_append_metric_is_jsonl(tmp_path: Path) -> None:
    rd = run_dir(tmp_path, "run1")
    rd.mkdir(parents=True)
    append_metric(rd, {"step": 1, "loss": 2.5})
    append_metric(rd, {"step": 2, "loss": 2.1})
    lines = (rd / "metrics.jsonl").read_text(encoding="utf-8").splitlines()
    assert len(lines) == 2
    assert json.loads(lines[1])["loss"] == 2.1


def test_latest_checkpoint_picks_highest_step(tmp_path: Path) -> None:
    cps = tmp_path / "checkpoints"
    for name in ["checkpoint-10", "checkpoint-200", "checkpoint-35", "not-a-checkpoint"]:
        (cps / name).mkdir(parents=True)
    found = _latest_checkpoint(cps)
    assert found is not None and found.name == "checkpoint-200"
    assert _latest_checkpoint(tmp_path / "missing") is None
