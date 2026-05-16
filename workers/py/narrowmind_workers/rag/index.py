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

    if TABLE_NAME in db.list_tables():
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
    if TABLE_NAME not in db.list_tables():
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
    if TABLE_NAME not in db.list_tables():
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
