"""Behavioral contract probes for third-party ML dependencies.

Born from the lancedb 0.30 incident (docs/testing.md): ``list_tables()``
changed its return shape between minor versions, ``name in db.list_tables()``
silently became always-False, every upsert fell through to ``create_table``
and dropped its rows — and the unit suite stayed green because nothing pinned
the *behavior* we depended on, only the happy-path calls.

Each test here asserts one external contract this codebase relies on. When a
dependency range in ``pyproject.toml`` is widened, this file is the first
thing to run: a failure means the new version drifted on a contract and the
calling code needs auditing BEFORE the bump lands, not after the vector store
corrupts.

Keep these probes cheap (no model downloads beyond what other tests already
require, tmp-dir LanceDB only) and brutally specific.
"""

from __future__ import annotations

import math
from pathlib import Path


# ---------------------------------------------------------------------------
# lancedb — the dependency that already burned us once
# ---------------------------------------------------------------------------


def _connect(tmp_path: Path):
    import lancedb

    return lancedb.connect(str(tmp_path / "probe_store"))


def test_lancedb_list_tables_shape_is_compatible(tmp_path: Path) -> None:
    """Our `_table_exists()` shim (rag/index.py) handles both the pre-0.30
    `list[str]` and the 0.30+ `ListTablesResponse` (`.tables` attr). If a
    future version returns something neither shape covers, this fails before
    the silent-upsert-loss failure mode can re-occur."""
    db = _connect(tmp_path)
    db.create_table("probe", data=[{"id": "a", "text": "x"}], mode="overwrite")

    result = db.list_tables()
    names = getattr(result, "tables", result)
    assert "probe" in list(names), (
        f"list_tables() shape drifted again: {type(result).__name__} -> "
        f"neither a name list nor has .tables"
    )


def test_lancedb_count_rows_after_create_and_add(tmp_path: Path) -> None:
    """The probe that would have caught the 0.30 incident on day one:
    rows written must be rows counted, through the create + add path."""
    db = _connect(tmp_path)
    t = db.create_table("probe", data=[{"id": "a", "text": "x"}], mode="overwrite")
    t.add([{"id": "b", "text": "y"}, {"id": "c", "text": "z"}])
    assert t.count_rows() == 3


def test_lancedb_fts_search_exposes_score(tmp_path: Path) -> None:
    """Hybrid retrieval reads `_score` from FTS hits (rag/index.py::fts_query)
    and rrf_fuse strips underscore-prefixed keys. If the BM25 score column is
    ever renamed, sparse retrieval silently stops contributing to RRF."""
    db = _connect(tmp_path)
    t = db.create_table(
        "probe",
        data=[
            {"id": "a", "text": "the Doleantie movement led by Kuyper"},
            {"id": "b", "text": "qualia and subjective experience"},
        ],
        mode="overwrite",
    )
    t.create_fts_index("text", replace=True)
    hits = t.search("Doleantie", query_type="fts").limit(1).to_list()
    assert hits, "FTS search returned nothing for an exact token"
    assert "_score" in hits[0], f"FTS hit keys drifted: {sorted(hits[0].keys())}"
    assert hits[0]["id"] == "a"


def test_lancedb_vector_search_exposes_distance(tmp_path: Path) -> None:
    """Dense retrieval reads `_distance` (rag/index.py::query); RetrievedChunk
    on the Rust side deserializes it via serde rename. Column rename = silent
    zeros in every citation."""
    db = _connect(tmp_path)
    t = db.create_table(
        "probe",
        data=[
            {"id": "a", "vector": [1.0, 0.0]},
            {"id": "b", "vector": [0.0, 1.0]},
        ],
        mode="overwrite",
    )
    hits = t.search([1.0, 0.1]).limit(1).to_list()
    assert hits and "_distance" in hits[0], (
        f"vector hit keys drifted: {sorted(hits[0].keys()) if hits else 'no hits'}"
    )
    assert hits[0]["id"] == "a"


# ---------------------------------------------------------------------------
# sentence-transformers / BGE-small — embedding contract
# ---------------------------------------------------------------------------


def test_bge_small_dim_and_normalization() -> None:
    """The LanceDB schema hard-codes DIM=384 (rag/index.py::_schema) and the
    cosine metric assumes L2-normalised vectors at write time. A model-loading
    or API change that altered either would corrupt every similarity score
    without raising. Uses the same cached model other tests already pull."""
    from narrowmind_workers.rag.embedder import DIM, embed_one

    vec = embed_one("contract probe sentence")
    assert len(vec) == DIM == 384, f"embedding dim drifted: {len(vec)} vs DIM={DIM}"
    norm = math.sqrt(sum(x * x for x in vec))
    assert abs(norm - 1.0) < 1e-3, f"embeddings no longer L2-normalised (|v|={norm:.4f})"


def test_embed_empty_batch_short_circuits() -> None:
    """embed([]) must return [] without touching the model — callers rely on
    this to skip model load when a source has no included chunks."""
    from narrowmind_workers.rag.embedder import embed

    assert embed([]) == []


# ---------------------------------------------------------------------------
# tiktoken — chunk-size estimation contract
# ---------------------------------------------------------------------------


def test_tiktoken_cl100k_is_offline_and_stable() -> None:
    """Chunking budgets are calibrated against cl100k_base counts. A tokenizer
    revision that changed counts would silently shift every chunk boundary,
    invalidating eval comparability across versions."""
    import tiktoken

    enc = tiktoken.get_encoding("cl100k_base")
    # Frozen reference: this exact string encodes to exactly these 5 token ids
    # under cl100k_base today. Any change means the encoding tables moved.
    tokens = enc.encode("The hard problem of consciousness")
    assert tokens == [791, 2653, 3575, 315, 25917], (
        f"cl100k_base tokenization drifted: {tokens}"
    )
