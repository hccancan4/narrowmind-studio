"""HuggingFace text-dataset ingestion (Phase 4.7).

Pull a column of text out of a HF dataset into the project as documents → chunks, so a
ready-made corpus (e.g. the Stanford Encyclopedia of Philosophy) can back RAG retrieval.
Mirrors :mod:`narrowmind_workers.ingestion.local`: writes
``sources/<id>/documents.jsonl`` + ``source.json`` atomically, then runs the shared
cleaning/chunking pipeline so the call ends with ``chunks.jsonl`` on disk.

Only the document-fetch front-end is new — everything downstream (chunking, language +
quality filtering, MinHash dedup) is the existing pipeline.
"""

from __future__ import annotations

import logging
import os
from pathlib import Path
from typing import Any

from narrowmind_workers.ingestion._common import (
    fresh_doc_id,
    fresh_source_id,
    write_manifest_atomic,
)
from narrowmind_workers.ingestion.models import Document, SourceManifest, SourceType
from narrowmind_workers.ingestion.pipeline import (
    append_chunks_summary_to_manifest,
    process_source,
)

log = logging.getLogger(__name__)


def ingest_hf_dataset(
    project_root: Path,
    repo_id: str,
    text_column: str,
    *,
    split: str = "train",
    category_column: str | None = None,
    categories: list[str] | None = None,
    max_rows: int | None = None,
    source_id: str | None = None,
) -> SourceManifest:
    """Ingest text rows from a HF dataset into the given project.

    Each kept row becomes one :class:`Document` (``text`` = the ``text_column`` value).
    When ``categories`` is set, keeps only rows whose ``category_column`` matches
    (case-insensitive). When ``max_rows`` caps a larger filtered set, takes an evenly
    spaced (order-spanning) subsample so every category survives the cap rather than
    truncating to whatever sorts first — deterministic, no seed needed.
    """

    # Local import: `datasets` is a heavy import; keep it out of module import time.
    from datasets import load_dataset  # noqa: PLC0415

    source_id = source_id or fresh_source_id(repo_id.replace("/", "-"))
    manifest = SourceManifest.new(
        source_id=source_id,
        source_type=SourceType.HF_DATASET,
        params={
            "repo_id": repo_id,
            "split": split,
            "text_column": text_column,
            "category_column": category_column,
            "categories": categories,
            "max_rows": max_rows,
        },
    )

    source_dir = project_root / "sources" / source_id
    source_dir.mkdir(parents=True, exist_ok=True)
    docs_path = source_dir / "documents.jsonl"

    dataset = load_dataset(repo_id, split=split)
    cat_set = {c.strip().lower() for c in categories} if categories else None

    # First pass: filter + map to Documents, preserving dataset order.
    candidates: list[Document] = []
    for idx, row in enumerate(dataset):
        doc = _row_to_document(
            row,
            idx,
            repo_id=repo_id,
            text_column=text_column,
            category_column=category_column,
            cat_set=cat_set,
        )
        if doc is not None:
            candidates.append(doc)

    if max_rows is not None and len(candidates) > max_rows:
        candidates = _even_subsample(candidates, max_rows)

    docs_tmp = docs_path.with_suffix(".jsonl.tmp")
    with docs_tmp.open("w", encoding="utf-8") as out:
        for doc in candidates:
            out.write(doc.to_json_line())
            out.write("\n")

    docs_written = len(candidates)
    if docs_written == 0:
        docs_tmp.unlink(missing_ok=True)
    else:
        os.replace(docs_tmp, docs_path)

    manifest.document_count = docs_written
    write_manifest_atomic(source_dir / "source.json", manifest)

    # Drive cleaning + chunking so the call ends with chunks.jsonl on disk — what every
    # downstream tool (build_dataset, rag.embed_chunks, generate_sft) reads from.
    if docs_written > 0:
        stats = process_source(project_root, manifest.source_id)
        append_chunks_summary_to_manifest(project_root, manifest.source_id, stats)
    log.info(
        "hf_dataset source %s: %d docs from %s[%s]",
        source_id,
        docs_written,
        repo_id,
        split,
    )
    return manifest


def _row_to_document(
    row: dict[str, Any],
    idx: int,
    *,
    repo_id: str,
    text_column: str,
    category_column: str | None,
    cat_set: set[str] | None,
) -> Document | None:
    """Map one dataset row to a Document, or ``None`` if it's empty or filtered out."""

    text = row.get(text_column)
    if not isinstance(text, str) or not text.strip():
        return None
    category = row.get(category_column) if category_column else None
    if cat_set is not None and (
        not isinstance(category, str) or category.strip().lower() not in cat_set
    ):
        return None

    metadata: dict[str, Any] = {"hf_repo_id": repo_id, "hf_row": idx}
    if isinstance(category, str) and category.strip():
        metadata["category"] = category
    title = (
        category.strip()
        if isinstance(category, str) and category.strip()
        else f"{repo_id}#{idx}"
    )
    short_name = repo_id.split("/")[-1]
    return Document(
        doc_id=fresh_doc_id(f"{short_name}-{idx}"),
        title=title,
        text=text,
        source_path=f"hf:{repo_id}#{idx}",
        metadata=metadata,
    )


def _even_subsample(items: list[Document], k: int) -> list[Document]:
    """Take ``k`` evenly spaced items spanning the full list (order-preserving)."""

    stride = len(items) / k
    return [items[int(i * stride)] for i in range(k)]
