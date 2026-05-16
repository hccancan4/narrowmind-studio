"""JSON-RPC methods exposed by the rag worker.

- ``rag.embed_chunks``: read ``chunks.jsonl`` (filterable by source_id, include flag),
  embed via BGE-small, upsert into LanceDB.
- ``rag.query``: embed a query string, search the index, return top-k.
- ``rag.count``: how many rows are in the index.

The Rust orchestrator's ``build_dataset`` tool drives ``embed_chunks``; the ``query_index``
tool drives ``rag.query``. Both shapes are stable from this milestone onward.
"""

from __future__ import annotations

import json
import logging
import time
from pathlib import Path
from typing import Any

from narrowmind_workers.rag.embedder import embed, embed_one
from narrowmind_workers.rag.index import count_rows, query as index_query, upsert_chunks
from narrowmind_workers.rpc import JsonRpcError, MethodRegistry, error_codes

log = logging.getLogger(__name__)


def register_methods(registry: MethodRegistry) -> None:
    registry.register("rag.embed_chunks", _embed_chunks)
    registry.register("rag.query", _query)
    registry.register("rag.count", _count)


def _embed_chunks(params: dict[str, Any]) -> dict[str, Any]:
    """Args: ``{"project_root", "source_id"?, "include_only"?: bool=true, "batch_size"?: int=64}``.

    Returns: ``{"embedded": N, "skipped_excluded": M, "elapsed_seconds": T}``.
    """

    project_root = _required_dir(params, "project_root")
    source_filter = params.get("source_id")
    if source_filter is not None and not isinstance(source_filter, str):
        raise JsonRpcError(error_codes.INVALID_PARAMS, "`source_id` must be a string if provided")
    include_only = bool(params.get("include_only", True))
    batch_size = _bounded_int(params, "batch_size", default=64, minimum=1, maximum=512)

    chunk_records = _collect_chunks(project_root, source_filter, include_only)
    if not chunk_records:
        return {"embedded": 0, "skipped_excluded": 0, "elapsed_seconds": 0.0, "note": "no chunks matched"}

    started = time.perf_counter()
    texts = [r["text"] for r in chunk_records]
    log.info("embedding %d chunks via BGE-small (batch_size=%d) ...", len(texts), batch_size)
    vectors = embed(texts, batch_size=batch_size)
    for r, v in zip(chunk_records, vectors, strict=True):
        r["embedding"] = v
    written = upsert_chunks(project_root, chunk_records)
    elapsed = time.perf_counter() - started

    log.info("embedded + upserted %d chunks in %.1fs", written, elapsed)
    return {
        "embedded": written,
        "skipped_excluded": 0,  # populated by callers that pre-filter; keep field stable
        "elapsed_seconds": round(elapsed, 2),
    }


def _query(params: dict[str, Any]) -> dict[str, Any]:
    """Args: ``{"project_root", "query", "top_k"?: 5, "source_id"?: str}``.

    Returns: ``{"hits": [{chunk_id, doc_id, source_id, text, token_count, metadata, _distance}]}``.
    """

    project_root = _required_dir(params, "project_root")
    q_text = _required_str(params, "query")
    top_k = _bounded_int(params, "top_k", default=5, minimum=1, maximum=50)
    source_filter = params.get("source_id")
    if source_filter is not None and not isinstance(source_filter, str):
        raise JsonRpcError(error_codes.INVALID_PARAMS, "`source_id` must be a string if provided")

    qv = embed_one(q_text)
    hits = index_query(project_root, qv, top_k=top_k, source_filter=source_filter)
    return {"hits": hits}


def _count(params: dict[str, Any]) -> dict[str, Any]:
    """Args: ``{"project_root"}``. Returns ``{"count": N}``."""

    project_root = _required_dir(params, "project_root")
    return {"count": count_rows(project_root)}


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _collect_chunks(
    project_root: Path,
    source_filter: str | None,
    include_only: bool,
) -> list[dict[str, Any]]:
    sources_dir = project_root / "sources"
    if not sources_dir.is_dir():
        return []
    records: list[dict[str, Any]] = []
    for entry in sorted(sources_dir.iterdir()):
        if not entry.is_dir():
            continue
        if source_filter and entry.name != source_filter:
            continue
        chunks_path = entry / "chunks.jsonl"
        if not chunks_path.is_file():
            continue
        with chunks_path.open("r", encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    rec = json.loads(line)
                except json.JSONDecodeError as e:
                    log.warning("skipping malformed chunk line in %s: %s", chunks_path, e)
                    continue
                if include_only and not rec.get("include", True):
                    continue
                records.append(rec)
    return records


def _required_str(params: dict[str, Any], key: str) -> str:
    v = params.get(key)
    if not isinstance(v, str) or not v:
        raise JsonRpcError(error_codes.INVALID_PARAMS, f"missing or empty `{key}`")
    return v


def _required_dir(params: dict[str, Any], key: str) -> Path:
    p = Path(_required_str(params, key))
    if not p.is_dir():
        raise JsonRpcError(error_codes.INVALID_PARAMS, f"{key} is not a directory: {p}")
    return p


def _bounded_int(
    params: dict[str, Any], key: str, default: int, minimum: int, maximum: int
) -> int:
    v = params.get(key, default)
    if not isinstance(v, int) or isinstance(v, bool):
        raise JsonRpcError(error_codes.INVALID_PARAMS, f"`{key}` must be an integer")
    if v < minimum or v > maximum:
        raise JsonRpcError(
            error_codes.INVALID_PARAMS, f"`{key}` must be in [{minimum}, {maximum}], got {v}"
        )
    return v
