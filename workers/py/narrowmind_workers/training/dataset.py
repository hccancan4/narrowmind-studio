"""SFT dataset loading + chat-template rendering + train/validation split.

Pure module (no torch/unsloth) so it unit-tests in the CPU environment.
The actual tokenization happens in runner.py with the real AutoTokenizer
(KARAR 7 — Phase 2's tiktoken approximation stays in chunking only);
this module produces the message-structured records the tokenizer's own
``apply_chat_template`` consumes, plus a cheap char-length pre-screen used
for the truncation-stats report.

Input contract: ``datasets/sft.jsonl`` lines of
``{"question": str, "answer": str, "source_chunk_id": str}`` (synth_gen's
QaPair shape). ``datasets/eval.jsonl`` is NEVER touched here — that file is
the RAG/judge eval set; training validation is split from sft.jsonl itself
(KARAR 5).
"""

from __future__ import annotations

import json
import random
from pathlib import Path
from typing import Any


def load_sft_pairs(sft_path: Path) -> list[dict[str, str]]:
    """Read sft.jsonl, skipping malformed lines (count surfaced to caller via
    length diff if they care). Raises FileNotFoundError naturally if absent."""
    pairs: list[dict[str, str]] = []
    with sft_path.open("r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                continue
            q = rec.get("question")
            a = rec.get("answer")
            if isinstance(q, str) and isinstance(a, str) and q and a:
                pairs.append({"question": q, "answer": a})
    return pairs


def to_messages(pair: dict[str, str]) -> list[dict[str, str]]:
    """One QA pair → chat-message structure. The tokenizer's own
    ``apply_chat_template`` renders this into the model's native format
    (ChatML for Qwen, Gemma turns for Gemma) — we never hand-write template
    strings, per KARAR 7."""
    return [
        {"role": "user", "content": pair["question"]},
        {"role": "assistant", "content": pair["answer"]},
    ]


def split_train_validation(
    pairs: list[dict[str, str]],
    validation_split: float,
    seed: int,
) -> tuple[list[dict[str, str]], list[dict[str, str]]]:
    """Deterministic seeded split. Validation gets ``round(n * split)`` items,
    minimum 1 (a zero-item eval set makes eval_loss meaningless and
    load_best_model_at_end crash)."""
    if not pairs:
        return [], []
    shuffled = list(pairs)
    rng = random.Random(seed)
    rng.shuffle(shuffled)
    n_val = max(1, round(len(shuffled) * validation_split))
    return shuffled[n_val:], shuffled[:n_val]


def truncation_prescreen(
    pairs: list[dict[str, str]],
    max_seq_length: int,
    chars_per_token: float = 4.0,
) -> dict[str, Any]:
    """Cheap char-based estimate of how many pairs will exceed max_seq_length.

    This is a REPORTING aid only (logged at training start, KARAR 7) — the
    authoritative count comes from the real tokenizer in runner.py. The
    pre-screen exists so the user sees a red flag before the model loads.
    """
    over = 0
    longest_chars = 0
    budget_chars = int(max_seq_length * chars_per_token)
    for p in pairs:
        total = len(p["question"]) + len(p["answer"])
        longest_chars = max(longest_chars, total)
        if total > budget_chars:
            over += 1
    return {
        "pairs": len(pairs),
        "estimated_over_budget": over,
        "longest_chars": longest_chars,
        "char_budget": budget_chars,
    }
