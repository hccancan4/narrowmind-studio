"""LanceDB index round-trip tests. Bypasses the embedder (we use synthetic vectors)
so these tests stay fast and don't trigger a model download on first run."""

from __future__ import annotations

import random
from pathlib import Path

import pytest

from narrowmind_workers.rag.embedder import DIM
from narrowmind_workers.rag.index import (
    TABLE_NAME,
    count_rows,
    open_db,
    query,
    store_path,
    upsert_chunks,
)


def _fake_vec(seed: int) -> list[float]:
    rng = random.Random(seed)
    raw = [rng.uniform(-1.0, 1.0) for _ in range(DIM)]
    # L2-normalise so cosine distance is well-behaved
    norm = sum(x * x for x in raw) ** 0.5 or 1.0
    return [x / norm for x in raw]


def _fixture_record(chunk_id: str, source_id: str, text: str, seed: int) -> dict:
    return {
        "chunk_id": chunk_id,
        "doc_id": f"doc-{source_id}",
        "source_id": source_id,
        "text": text,
        "embedding": _fake_vec(seed),
        "token_count": len(text.split()),
        "metadata": {"title": f"title-{chunk_id}"},
    }


def test_upsert_creates_table_first_time(tmp_path: Path) -> None:
    records = [
        _fixture_record("ck1", "src1", "first chunk", 1),
        _fixture_record("ck2", "src1", "second chunk", 2),
    ]
    written = upsert_chunks(tmp_path, records)
    assert written == 2
    assert count_rows(tmp_path) == 2
    assert store_path(tmp_path).is_dir()


def test_upsert_replaces_existing_chunks(tmp_path: Path) -> None:
    initial = [_fixture_record("ck1", "src1", "version 1", 1)]
    upsert_chunks(tmp_path, initial)
    assert count_rows(tmp_path) == 1

    # Update ck1 + add ck2
    updated = [
        _fixture_record("ck1", "src1", "version 2", 1),  # same id, new text
        _fixture_record("ck2", "src1", "new chunk", 2),
    ]
    upsert_chunks(tmp_path, updated)
    assert count_rows(tmp_path) == 2


def test_query_returns_top_k(tmp_path: Path) -> None:
    records = [_fixture_record(f"ck{i}", "src1", f"chunk {i}", i) for i in range(10)]
    upsert_chunks(tmp_path, records)

    qv = _fake_vec(3)  # close to records with seed near 3
    hits = query(tmp_path, qv, top_k=3)
    assert len(hits) == 3
    # Each hit has the expected fields
    for h in hits:
        assert "chunk_id" in h
        assert "text" in h
        assert "_distance" in h
        # Embedding column is stripped from query results to keep payloads small.
        assert "embedding" not in h
        # Metadata is parsed back into a dict
        assert isinstance(h["metadata"], dict)


def test_query_respects_source_filter(tmp_path: Path) -> None:
    a = [_fixture_record(f"a{i}", "srcA", f"a {i}", i) for i in range(5)]
    b = [_fixture_record(f"b{i}", "srcB", f"b {i}", 100 + i) for i in range(5)]
    upsert_chunks(tmp_path, a + b)

    qv = _fake_vec(0)
    hits_all = query(tmp_path, qv, top_k=10)
    assert len(hits_all) == 10
    hits_a = query(tmp_path, qv, top_k=10, source_filter="srcA")
    assert all(h["source_id"] == "srcA" for h in hits_a)
    assert len(hits_a) == 5


def test_empty_records_noop(tmp_path: Path) -> None:
    assert upsert_chunks(tmp_path, []) == 0
    assert count_rows(tmp_path) == 0
    # Note: count_rows opens the DB which creates the dir. That's acceptable —
    # a missing table just means 0 rows, no error.


def test_query_on_empty_db_returns_empty(tmp_path: Path) -> None:
    # No upsert; an empty store should return no hits, not error.
    _ = open_db(tmp_path)  # create the empty dir
    qv = _fake_vec(0)
    assert query(tmp_path, qv, top_k=5) == []


@pytest.mark.parametrize("malformed_metadata", [None, "", "not json", "{broken"])
def test_query_tolerates_bad_metadata(tmp_path: Path, malformed_metadata) -> None:
    record = _fixture_record("ck1", "src1", "x", 1)
    record["metadata"] = malformed_metadata
    upsert_chunks(tmp_path, [record])
    hits = query(tmp_path, _fake_vec(1), top_k=1)
    # Bad JSON → empty dict, not exception
    assert hits[0]["metadata"] == {}
