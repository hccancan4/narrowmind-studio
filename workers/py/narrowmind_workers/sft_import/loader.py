"""Import ready-made HuggingFace QA datasets as SFT pairs.

The pure selection logic (:func:`select_pairs`) is deliberately separated from the HF
download (:func:`import_hf_qa`) so it can be unit-tested without network access.

Output rows match the orchestrator's ``QaPair`` shape exactly:
``{"question": str, "answer": str, "source_chunk_id": "hf:<repo_id>#<row_index>"}``.
"""

from __future__ import annotations

import hashlib
import random
from typing import Any, Iterable


def _rng(seed: int, salt: str) -> random.Random:
    """Deterministic RNG keyed on ``seed`` + a per-source ``salt``.

    Uses a SHA-256 digest rather than ``hash()`` so sampling is reproducible across
    processes regardless of ``PYTHONHASHSEED``.
    """

    digest = hashlib.sha256(f"{seed}:{salt}".encode()).digest()
    return random.Random(int.from_bytes(digest[:8], "big"))


def select_pairs(
    rows: Iterable[dict[str, Any]],
    *,
    repo_id: str,
    question_col: str,
    answer_col: str,
    category_col: str | None,
    categories: list[str] | None,
    min_answer_chars: int,
    max_rows: int | None,
    seed: int,
) -> list[dict[str, str]]:
    """Map + filter + (optionally) seed-sample dataset ``rows`` into QA pairs.

    - keeps only rows with non-empty string question + answer,
    - drops answers shorter than ``min_answer_chars``,
    - when ``categories`` is set, keeps only rows whose ``category_col`` value matches
      (case-insensitive),
    - when ``max_rows`` is set and exceeded, takes a deterministic seeded sample.

    ``source_chunk_id`` records the ORIGINAL row index so a pair is always traceable
    back to its dataset row, independent of filtering.
    """

    cat_set = {c.strip().lower() for c in categories} if categories else None
    kept: list[dict[str, str]] = []
    for idx, row in enumerate(rows):
        question = row.get(question_col)
        answer = row.get(answer_col)
        if not isinstance(question, str) or not isinstance(answer, str):
            continue
        question, answer = question.strip(), answer.strip()
        if not question or not answer or len(answer) < min_answer_chars:
            continue
        if cat_set is not None:
            cat = row.get(category_col) if category_col else None
            if not isinstance(cat, str) or cat.strip().lower() not in cat_set:
                continue
        kept.append(
            {
                "question": question,
                "answer": answer,
                "source_chunk_id": f"hf:{repo_id}#{idx}",
            }
        )

    if max_rows is not None and len(kept) > max_rows:
        rng = _rng(seed, repo_id)
        rng.shuffle(kept)
        kept = kept[:max_rows]
    return kept


def import_hf_qa(
    sources: list[dict[str, Any]],
    seed: int,
    max_total: int | None = None,
) -> dict[str, Any]:
    """Download each HF QA dataset, filter/sample it, and concatenate the results.

    Returns ``{"pairs": [...], "meta": {"sources": [...], "total": N}}``. The caller
    (Rust ``import_sft_from_hf``) performs the reproducible train/eval split + writes
    ``sft.jsonl`` / ``eval.jsonl``, so this function only produces the combined pool.
    """

    # Local import: `datasets` is a heavy import; keep it out of module import time so
    # the pure `select_pairs` path (and its tests) never pay for it.
    from datasets import load_dataset  # noqa: PLC0415

    all_pairs: list[dict[str, str]] = []
    meta_sources: list[dict[str, Any]] = []
    for src in sources:
        repo_id = src["repo_id"]
        split = src.get("split") or "train"
        dataset = load_dataset(repo_id, split=split)
        dataset_rows = dataset.to_list()
        kept = select_pairs(
            dataset_rows,
            repo_id=repo_id,
            question_col=src.get("question_col") or "question",
            answer_col=src.get("answer_col") or "answer",
            category_col=src.get("category_col"),
            categories=src.get("categories"),
            min_answer_chars=int(src.get("min_answer_chars") or 0),
            max_rows=src.get("max_rows"),
            seed=seed,
        )
        all_pairs.extend(kept)
        meta_sources.append(
            {"repo_id": repo_id, "loaded": len(dataset_rows), "kept": len(kept)}
        )

    if max_total is not None and len(all_pairs) > max_total:
        rng = _rng(seed, "__total__")
        rng.shuffle(all_pairs)
        all_pairs = all_pairs[:max_total]

    return {
        "pairs": all_pairs,
        "meta": {"sources": meta_sources, "total": len(all_pairs)},
    }
