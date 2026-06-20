"""Unit tests for the pure SFT-import selection logic (no network)."""

from __future__ import annotations

from narrowmind_workers.sft_import.loader import select_pairs


def _rows() -> list[dict]:
    return [
        {"question": "q0", "answer": "a" * 300, "category": "Ethics"},
        {"question": "q1", "answer": "short", "category": "logic"},
        {"question": "q2", "answer": "b" * 300, "category": "logic"},
        {"question": "  ", "answer": "c" * 300, "category": "ethics"},  # blank question
        {"question": "q4", "answer": "d" * 300},  # no category field
        {"question": "q5", "answer": 123, "category": "ethics"},  # non-string answer
    ]


def _ids(pairs: list[dict]) -> list[str]:
    return [p["source_chunk_id"] for p in pairs]


def test_maps_filters_and_assigns_source_id() -> None:
    pairs = select_pairs(
        _rows(),
        repo_id="r",
        question_col="question",
        answer_col="answer",
        category_col=None,
        categories=None,
        min_answer_chars=100,
        max_rows=None,
        seed=1,
    )
    # Dropped: q1 (short answer), blank question, q5 (non-str answer). Kept: q0, q2, q4.
    assert {p["question"] for p in pairs} == {"q0", "q2", "q4"}
    assert all(len(p["answer"]) >= 100 for p in pairs)
    # source_chunk_id encodes the ORIGINAL row index, not the kept index.
    by_q = {p["question"]: p["source_chunk_id"] for p in pairs}
    assert by_q["q0"] == "hf:r#0"
    assert by_q["q2"] == "hf:r#2"
    assert by_q["q4"] == "hf:r#4"


def test_category_filter_is_case_insensitive() -> None:
    pairs = select_pairs(
        _rows(),
        repo_id="r",
        question_col="question",
        answer_col="answer",
        category_col="category",
        categories=["LOGIC"],
        min_answer_chars=0,
        max_rows=None,
        seed=1,
    )
    # category == logic (case-insensitive): q1 ("short" ok at min 0) + q2.
    assert {p["question"] for p in pairs} == {"q1", "q2"}


def test_max_rows_is_seeded_and_reproducible() -> None:
    rows = [{"question": f"q{i}", "answer": "a" * 200} for i in range(100)]
    kwargs = {
        "repo_id": "r",
        "question_col": "question",
        "answer_col": "answer",
        "category_col": None,
        "categories": None,
        "min_answer_chars": 0,
        "max_rows": 10,
    }
    a = select_pairs(rows, seed=42, **kwargs)
    b = select_pairs(rows, seed=42, **kwargs)
    c = select_pairs(rows, seed=99, **kwargs)
    assert len(a) == 10
    assert _ids(a) == _ids(b)  # same seed → identical sample
    assert _ids(a) != _ids(c)  # different seed → different sample
