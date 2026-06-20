"""Tests for HuggingFace text-dataset ingestion.

`_row_to_document` is tested purely (no network). The end-to-end ingest path mocks
`datasets.load_dataset` so it exercises the real chunking pipeline without a download.
"""

from __future__ import annotations

from narrowmind_workers.ingestion.hf_dataset import (
    _even_subsample,
    _row_to_document,
    ingest_hf_dataset,
)
from narrowmind_workers.ingestion.models import Document, SourceType


def test_row_to_document_maps_and_filters() -> None:
    doc = _row_to_document(
        {"text": "hello world", "category": "Mind"},
        3,
        repo_id="a/b",
        text_column="text",
        category_column="category",
        cat_set={"mind"},
    )
    assert doc is not None
    assert doc.source_path == "hf:a/b#3"
    assert doc.metadata["category"] == "Mind"
    assert doc.metadata["hf_row"] == 3
    assert doc.title == "Mind"

    # Filtered out: category not in the allow-set (case-insensitive miss).
    assert (
        _row_to_document(
            {"text": "x", "category": "Ethics"},
            0,
            repo_id="a/b",
            text_column="text",
            category_column="category",
            cat_set={"mind"},
        )
        is None
    )
    # Skipped: empty / whitespace text.
    assert (
        _row_to_document(
            {"text": "   "},
            0,
            repo_id="a/b",
            text_column="text",
            category_column=None,
            cat_set=None,
        )
        is None
    )
    # Skipped: missing / non-string text.
    assert (
        _row_to_document(
            {"text": 123},
            0,
            repo_id="a/b",
            text_column="text",
            category_column=None,
            cat_set=None,
        )
        is None
    )


def test_even_subsample_spans_and_caps() -> None:
    items = [Document(doc_id=str(i), title="t", text="x", source_path="p") for i in range(100)]
    out = _even_subsample(items, 10)
    assert len(out) == 10
    # First item kept, evenly spaced, last index strictly inside range.
    assert out[0].doc_id == "0"
    assert out[1].doc_id == "10"
    assert int(out[-1].doc_id) < 100


_PASSAGE = (
    "Philosophy of mind studies the nature of consciousness, mental states, and their "
    "relation to the physical brain. Dualists hold that mind and matter are distinct, "
    "while physicalists argue that mental phenomena are ultimately physical. The hard "
    "problem of consciousness asks why subjective experience arises at all. "
)


def _rows() -> list[dict]:
    return [
        {"text": _PASSAGE + "Qualia are the felt qualities of experience.", "category": "mind"},
        {"text": _PASSAGE + "Virtue ethics centres moral character over rules.", "category": "ethics"},
        {"text": "", "category": "mind"},  # empty → skipped
        {"text": _PASSAGE + "Valid inference preserves truth from premises.", "category": "logic"},
    ]


def test_ingest_hf_dataset_end_to_end(tmp_path, monkeypatch) -> None:
    monkeypatch.setattr("datasets.load_dataset", lambda repo_id, split="train": _rows())

    (tmp_path / "sources").mkdir()
    manifest = ingest_hf_dataset(
        project_root=tmp_path,
        repo_id="acme/sep",
        text_column="text",
        split="train",
        category_column="category",
        categories=["Mind", "logic"],  # case-insensitive; excludes ethics
        source_id="sep-test",
    )

    assert manifest.source_type == SourceType.HF_DATASET
    assert manifest.params["repo_id"] == "acme/sep"
    # mind + logic kept; ethics filtered; empty skipped.
    assert manifest.document_count == 2

    source_dir = tmp_path / "sources" / "sep-test"
    doc_lines = (source_dir / "documents.jsonl").read_text(encoding="utf-8").splitlines()
    assert len(doc_lines) == 2
    # The shared pipeline ran → chunks.jsonl exists with at least one chunk.
    chunk_lines = (source_dir / "chunks.jsonl").read_text(encoding="utf-8").splitlines()
    assert len(chunk_lines) >= 1


def test_ingest_hf_dataset_no_matches_writes_empty(tmp_path, monkeypatch) -> None:
    monkeypatch.setattr("datasets.load_dataset", lambda repo_id, split="train": _rows())
    (tmp_path / "sources").mkdir()
    manifest = ingest_hf_dataset(
        project_root=tmp_path,
        repo_id="acme/sep",
        text_column="text",
        category_column="category",
        categories=["nonexistent"],
        source_id="empty-test",
    )
    assert manifest.document_count == 0
    # No documents.jsonl left behind when nothing matched.
    assert not (tmp_path / "sources" / "empty-test" / "documents.jsonl").exists()
