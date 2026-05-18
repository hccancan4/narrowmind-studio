"""LanceDB-backed vector store for chunks.

One table per project at ``<project>/vector_store/chunks.lance``. The schema mirrors
``chunks.jsonl`` plus an ``embedding`` vector column. Metadata is serialised as a JSON
string so LanceDB's Arrow schema stays flat (nested structs introduce migration pain).

LanceDB is embedded — no server to start, no port to free. The whole index is just files
on disk under ``vector_store/``, which makes back-up + delete operations trivial.
"""

from __future__ import annotations

import json
import logging
from pathlib import Path
from typing import Any

import lancedb
import pyarrow as pa

from narrowmind_workers.rag.embedder import DIM

log = logging.getLogger(__name__)

TABLE_NAME = "chunks"


def store_path(project_root: Path) -> Path:
    return project_root / "vector_store"


def open_db(project_root: Path):
    """Open (or create) the LanceDB at the project's vector store path."""
    store_path(project_root).mkdir(parents=True, exist_ok=True)
    return lancedb.connect(str(store_path(project_root)))


def _table_exists(db, name: str) -> bool:
    """Compatibility shim for lancedb 0.30+ where ``list_tables()`` returns a
    ``ListTablesResponse`` dataclass instead of a bare ``list[str]``. The old
    ``name in db.list_tables()`` idiom silently evaluates to ``False`` against the
    dataclass — which is exactly the bug that made every upsert fall through to
    ``create_table`` and lose its rows. Always go through this helper."""
    result = db.list_tables()
    names = getattr(result, "tables", result)  # list[str] on old lancedb, attr on new
    return name in names


def _schema() -> pa.Schema:
    return pa.schema(
        [
            pa.field("chunk_id", pa.string()),
            pa.field("doc_id", pa.string()),
            pa.field("source_id", pa.string()),
            pa.field("text", pa.string()),
            pa.field("embedding", pa.list_(pa.float32(), DIM)),
            pa.field("token_count", pa.int32()),
            pa.field("metadata", pa.string()),  # JSON blob
        ]
    )


def _records_to_rows(records: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Normalise records before write: stringify metadata, drop unknown keys."""
    out: list[dict[str, Any]] = []
    allowed = {"chunk_id", "doc_id", "source_id", "text", "embedding", "token_count", "metadata"}
    for r in records:
        row = {k: v for k, v in r.items() if k in allowed}
        meta = row.get("metadata")
        if meta is None:
            row["metadata"] = "{}"
        elif not isinstance(meta, str):
            row["metadata"] = json.dumps(meta, ensure_ascii=False)
        if "token_count" not in row:
            row["token_count"] = 0
        out.append(row)
    return out


def upsert_chunks(project_root: Path, records: list[dict[str, Any]]) -> int:
    """Insert or replace chunk rows by ``chunk_id``. Returns the count written."""
    rows = _records_to_rows(records)
    if not rows:
        return 0
    db = open_db(project_root)

    if _table_exists(db, TABLE_NAME):
        table = db.open_table(TABLE_NAME)
        # Delete existing rows we're about to re-insert. LanceDB has no native upsert
        # so this two-step is the canonical pattern.
        ids = [r["chunk_id"] for r in rows]
        if ids:
            # SQL injection isn't a real concern here — chunk_ids are validator-shaped
            # (`ck_<hex>`) from Phase 2's chunker — but escape anyway for safety.
            quoted = ", ".join(f"'{cid.replace(chr(39), chr(39) + chr(39))}'" for cid in ids)
            table.delete(f"chunk_id IN ({quoted})")
        table.add(rows)
    else:
        table = db.create_table(TABLE_NAME, data=rows, schema=_schema())

    log.info("upserted %d rows into %s", len(rows), TABLE_NAME)
    return len(rows)


def count_rows(project_root: Path) -> int:
    """Total chunks in the index. 0 if the table doesn't exist yet."""
    db = open_db(project_root)
    if not _table_exists(db, TABLE_NAME):
        return 0
    return db.open_table(TABLE_NAME).count_rows()


def query(
    project_root: Path,
    query_vector: list[float],
    top_k: int = 5,
    source_filter: str | None = None,
) -> list[dict[str, Any]]:
    """Cosine-via-dot-product search. Returns ``top_k`` rows with ``_distance`` populated.

    Embeddings are L2-normalised at write time so the default ``cosine`` metric is correct
    out of the box.
    """
    db = open_db(project_root)
    if not _table_exists(db, TABLE_NAME):
        return []
    table = db.open_table(TABLE_NAME)
    q = table.search(query_vector).limit(top_k)
    if source_filter:
        # Escape single quotes (paranoid; source_ids come from our own slugifier).
        safe = source_filter.replace("'", "''")
        q = q.where(f"source_id = '{safe}'")
    rows = q.to_list()
    for r in rows:
        try:
            r["metadata"] = json.loads(r.get("metadata") or "{}")
        except json.JSONDecodeError:
            r["metadata"] = {}
        # LanceDB returns the embedding column too; drop it from results to keep payloads small.
        r.pop("embedding", None)
    return rows


# ---------------------------------------------------------------------------
# Phase 3.5 — BM25 (full-text search) sidecar
# ---------------------------------------------------------------------------
# Phase 3 eval surfaced a recurring miss class: proper-noun-heavy queries
# (Doleantie, Iamblichus, Eureqa, "Guerizoli 2006 Brill") that the BGE-small
# dense embedder couldn't rank into top-5 even when the gold chunk was
# clearly in the corpus. A BM25 sparse index complements dense retrieval:
# exact term hits dominate sparse, semantic neighbours dominate dense, and
# Reciprocal Rank Fusion merges both rankings without needing tuned weights.


def build_fts_index(project_root: Path) -> None:
    """(Re)build the BM25 full-text index on the ``text`` column.

    Idempotent: ``replace=True`` rebuilds in place if the index already
    exists. Cheap for our corpus sizes (<10k chunks). Callers run this
    after a full ``upsert_chunks`` batch, not after every single row —
    the cost is dominated by tantivy/lance-index file I/O.

    Default tokenizer: ``simple`` base + English stemming + stop-word
    removal + lower-casing. Verified that "Doleantie" and other rare
    proper nouns survive the pipeline because they don't appear in
    the English stop-word list and the stemmer leaves them alone.
    """
    db = open_db(project_root)
    if not _table_exists(db, TABLE_NAME):
        log.warning("FTS index build skipped: table `%s` doesn't exist", TABLE_NAME)
        return
    table = db.open_table(TABLE_NAME)
    table.create_fts_index("text", replace=True)
    log.info("built FTS index on `%s.text`", TABLE_NAME)


def fts_query(
    project_root: Path,
    query_string: str,
    top_k: int = 5,
    source_filter: str | None = None,
) -> list[dict[str, Any]]:
    """BM25 query over the ``text`` column. Returns hits with ``_score`` populated
    (higher = better, in contrast to dense ``_distance`` where lower = better).

    Falls back to an empty list if the FTS index hasn't been built yet — caller
    decides whether to surface that as an error or silently degrade to dense-only.
    The exception is raised at query *execution* time (``to_list``), not at
    builder construction time, so the try/except must wrap the full pipeline.
    """
    db = open_db(project_root)
    if not _table_exists(db, TABLE_NAME):
        return []
    table = db.open_table(TABLE_NAME)
    try:
        q = table.search(query_string, query_type="fts").limit(top_k)
        if source_filter:
            safe = source_filter.replace("'", "''")
            q = q.where(f"source_id = '{safe}'")
        rows = q.to_list()
    except (RuntimeError, ValueError) as e:
        # Lance raises RuntimeError("Cannot perform full text search unless an
        # INVERTED index has been created...") when the FTS sidecar is missing,
        # and ValueError for other malformed-query cases. Either way the right
        # behaviour for retrieval is "degrade gracefully" — log and return [].
        log.warning("FTS query failed (no index?): %s", e)
        return []
    for r in rows:
        try:
            r["metadata"] = json.loads(r.get("metadata") or "{}")
        except json.JSONDecodeError:
            r["metadata"] = {}
        r.pop("embedding", None)
    return rows


def rrf_fuse(
    rankings: list[list[dict[str, Any]]],
    top_k: int,
    rrf_k: int = 60,
) -> list[dict[str, Any]]:
    """Reciprocal Rank Fusion across multiple ranked lists.

    Standard formula:    score(d) = Σ_r  1 / (rrf_k + rank_r(d))
    rrf_k = 60 is the canonical default (Cormack/Clarke 2009); the result
    is dominated by documents that appear in the top of multiple rankings
    while still rewarding strong-single-list hits.

    Each input list is a list of row dicts already shaped by
    :func:`query` / :func:`fts_query`. The output preserves the row dict
    of the first ranking that contributed (so chunk_id / text / metadata
    pass through), plus a ``_rrf_score`` and ``_rrf_sources`` debug field
    that names which underlying ranker contributed.
    """
    if top_k <= 0:
        return []
    fused: dict[str, dict[str, Any]] = {}
    scores: dict[str, float] = {}
    sources: dict[str, list[str]] = {}
    for source_idx, ranking in enumerate(rankings):
        source_name = f"r{source_idx}"
        for rank, row in enumerate(ranking, start=1):
            cid = row.get("chunk_id")
            if not cid:
                continue
            contribution = 1.0 / (rrf_k + rank)
            scores[cid] = scores.get(cid, 0.0) + contribution
            sources.setdefault(cid, []).append(source_name)
            # First-wins for the canonical row payload — every ranker
            # returns the same logical chunk, so dropping later copies
            # is safe.
            if cid not in fused:
                fused[cid] = {k: v for k, v in row.items() if not k.startswith("_")}
    # Sort by fused score descending, take top_k.
    ordered_ids = sorted(scores.keys(), key=lambda c: scores[c], reverse=True)[:top_k]
    out: list[dict[str, Any]] = []
    for cid in ordered_ids:
        row = fused[cid]
        row["_rrf_score"] = scores[cid]
        row["_rrf_sources"] = sources[cid]
        out.append(row)
    return out


def hybrid_query(
    project_root: Path,
    query_string: str,
    query_vector: list[float],
    top_k: int = 5,
    k_dense: int = 10,
    k_sparse: int = 10,
    rrf_k: int = 60,
    source_filter: str | None = None,
) -> list[dict[str, Any]]:
    """Hybrid retrieval: pull ``k_dense`` from BGE-small + ``k_sparse`` from BM25,
    fuse via Reciprocal Rank Fusion, return top ``top_k``.

    Each underlying retriever runs independently — failure of one (e.g. missing
    FTS index) degrades gracefully to the other rather than failing the request.
    """
    dense_hits = query(project_root, query_vector, top_k=k_dense, source_filter=source_filter)
    sparse_hits = fts_query(project_root, query_string, top_k=k_sparse, source_filter=source_filter)
    return rrf_fuse([dense_hits, sparse_hits], top_k=top_k, rrf_k=rrf_k)
