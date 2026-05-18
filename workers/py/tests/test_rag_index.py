"""LanceDB index round-trip tests. Bypasses the embedder (we use synthetic vectors)
so these tests stay fast and don't trigger a model download on first run."""

from __future__ import annotations

import random
from pathlib import Path

import pytest

from narrowmind_workers.rag.embedder import DIM
from narrowmind_workers.rag.index import (
    TABLE_NAME,
    build_fts_index,
    count_rows,
    fts_query,
    hybrid_query,
    open_db,
    query,
    rrf_fuse,
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


# ---------------------------------------------------------------------------
# Phase 3.5 — FTS + RRF + hybrid retrieval
# ---------------------------------------------------------------------------


def test_fts_index_finds_proper_nouns(tmp_path: Path) -> None:
    """The proper-noun case Phase 3 acceptance bug surfaced: BGE-small ranks
    'Doleantie' too low to make top-5 dense, but BM25 surfaces it instantly."""
    records = [
        _fixture_record(
            "ck_dol",
            "src",
            "The Doleantie was a breakaway movement led by Abraham Kuyper that left the Dutch state Hervormde Kerk.",
            1,
        ),
        _fixture_record("ck_oth1", "src", "qualia are subjective experiences", 2),
        _fixture_record(
            "ck_oth2", "src", "free will and determinism in modern philosophy", 3
        ),
        _fixture_record("ck_oth3", "src", "the hard problem of consciousness", 4),
    ]
    upsert_chunks(tmp_path, records)
    build_fts_index(tmp_path)

    hits = fts_query(tmp_path, "Doleantie movement", top_k=3)
    assert len(hits) >= 1
    # Top hit must be the Doleantie chunk — BM25's whole point.
    assert hits[0]["chunk_id"] == "ck_dol"
    assert "_score" in hits[0]
    assert hits[0]["_score"] > 0


def test_fts_query_without_index_returns_empty(tmp_path: Path) -> None:
    """If FTS index has never been built, fts_query must return [] rather than
    raise. Dense-only fallback is the explicit design choice."""
    records = [_fixture_record("ck1", "src", "no fts index built yet", 1)]
    upsert_chunks(tmp_path, records)
    # No build_fts_index call here.
    assert fts_query(tmp_path, "anything", top_k=3) == []


def test_fts_respects_source_filter(tmp_path: Path) -> None:
    upsert_chunks(
        tmp_path,
        [
            _fixture_record("a1", "srcA", "Doleantie movement detail", 1),
            _fixture_record("b1", "srcB", "Doleantie movement different source", 2),
        ],
    )
    build_fts_index(tmp_path)
    hits = fts_query(tmp_path, "Doleantie", top_k=5, source_filter="srcA")
    assert all(h["source_id"] == "srcA" for h in hits)
    assert len(hits) == 1


# --- RRF unit tests: deterministic inputs to make the fusion math auditable ---


def test_rrf_fuses_two_rankings_with_canonical_score() -> None:
    """Canonical Cormack/Clarke RRF: score(d) = sum_r 1/(k+rank).
    With k=60:
      - 'a' is rank 1 in both lists  → 1/61 + 1/61 = 2/61
      - 'b' is rank 2 in dense only  → 1/62
      - 'c' is rank 2 in sparse only → 1/62
    'a' wins; 'b' and 'c' tie."""
    dense = [
        {"chunk_id": "a", "text": "alpha"},
        {"chunk_id": "b", "text": "beta"},
    ]
    sparse = [
        {"chunk_id": "a", "text": "alpha"},  # rank 1 in both → highest fused
        {"chunk_id": "c", "text": "gamma"},
    ]
    fused = rrf_fuse([dense, sparse], top_k=3, rrf_k=60)
    # 'a' must come first; b/c ordering is arbitrary since they tie at 1/62.
    assert fused[0]["chunk_id"] == "a"
    assert {fused[1]["chunk_id"], fused[2]["chunk_id"]} == {"b", "c"}
    a_score = next(r["_rrf_score"] for r in fused if r["chunk_id"] == "a")
    b_score = next(r["_rrf_score"] for r in fused if r["chunk_id"] == "b")
    c_score = next(r["_rrf_score"] for r in fused if r["chunk_id"] == "c")
    assert a_score == pytest.approx(2.0 / 61, rel=1e-6)
    assert b_score == pytest.approx(1.0 / 62, rel=1e-6)
    assert c_score == pytest.approx(1.0 / 62, rel=1e-6)
    assert a_score > b_score
    assert a_score > c_score


def test_rrf_marks_contributing_sources() -> None:
    dense = [{"chunk_id": "a", "text": "x"}, {"chunk_id": "b", "text": "y"}]
    sparse = [{"chunk_id": "a", "text": "x"}, {"chunk_id": "c", "text": "z"}]
    fused = rrf_fuse([dense, sparse], top_k=3)
    by_id = {r["chunk_id"]: r for r in fused}
    assert set(by_id["a"]["_rrf_sources"]) == {"r0", "r1"}
    assert by_id["b"]["_rrf_sources"] == ["r0"]
    assert by_id["c"]["_rrf_sources"] == ["r1"]


def test_rrf_handles_empty_inputs() -> None:
    assert rrf_fuse([], top_k=5) == []
    assert rrf_fuse([[], []], top_k=5) == []


def test_rrf_handles_top_k_zero() -> None:
    dense = [{"chunk_id": "a"}]
    assert rrf_fuse([dense], top_k=0) == []


def test_hybrid_query_combines_dense_and_sparse(tmp_path: Path) -> None:
    """End-to-end hybrid: 'Doleantie' is a proper noun BGE-small struggles
    with, but BM25 finds it instantly. Hybrid should rank it in top-3."""
    records = [
        _fixture_record(
            "ck_dol", "src", "The Doleantie was led by Abraham Kuyper", 100
        ),
        _fixture_record("ck_qual", "src", "qualia are subjective experiences", 1),
        _fixture_record("ck_det", "src", "free will and determinism", 2),
        _fixture_record("ck_hard", "src", "hard problem of consciousness", 3),
        _fixture_record("ck_mind", "src", "mind body dualism", 4),
    ]
    upsert_chunks(tmp_path, records)
    build_fts_index(tmp_path)

    # Use a vector dissimilar from Doleantie chunk so dense ranks it low.
    qv = _fake_vec(3)
    hybrid = hybrid_query(
        tmp_path,
        "Doleantie movement",
        qv,
        top_k=3,
        k_dense=5,
        k_sparse=5,
    )
    assert len(hybrid) >= 1
    chunk_ids = [r["chunk_id"] for r in hybrid]
    # FTS dominates for this proper-noun query, so ck_dol must be in top results.
    assert "ck_dol" in chunk_ids
    # RRF debug fields present
    assert "_rrf_score" in hybrid[0]
    assert "_rrf_sources" in hybrid[0]
