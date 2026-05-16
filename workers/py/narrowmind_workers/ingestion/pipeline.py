"""End-to-end source processing: read documents.jsonl, filter, chunk, dedup, write chunks.jsonl.

Called from the ingestion RPC methods after they finish writing documents.jsonl. Splitting
this out of the per-source handlers (local / wikipedia / url) keeps the pipeline logic
in one place — handlers only worry about *getting* the text, not what happens to it.
"""

from __future__ import annotations

import json
import logging
import os
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

from narrowmind_workers.chunking import DEFAULT_CONFIG, Chunk, ChunkingConfig, chunk_document
from narrowmind_workers.cleaning import MinHashDedup, is_language, passes_quality

log = logging.getLogger(__name__)

# MinHash threshold defaults to Phase 2's 0.85 (decision in memory).
DEFAULT_DEDUP_THRESHOLD = 0.85
# Default language filter; None disables filtering entirely.
DEFAULT_LANGUAGE = "en"


@dataclass
class ProcessStats:
    docs_processed: int = 0
    docs_filtered_language: int = 0
    docs_filtered_quality: int = 0
    chunks_emitted: int = 0
    chunks_dedup_dropped: int = 0
    chunks_kept: int = 0
    failures: list[dict[str, Any]] = field(default_factory=list)

    def as_dict(self) -> dict[str, Any]:
        return asdict(self)


def process_source(
    project_root: Path,
    source_id: str,
    chunking: ChunkingConfig = DEFAULT_CONFIG,
    dedup_threshold: float = DEFAULT_DEDUP_THRESHOLD,
    language: str | None = DEFAULT_LANGUAGE,
) -> ProcessStats:
    """Clean + chunk + dedup the documents under ``<project>/sources/<source_id>/``.

    Reads ``documents.jsonl``, writes ``chunks.jsonl`` atomically (.tmp swap).
    Returns a stats record the caller can surface to the agent / UI.
    """

    source_dir = project_root / "sources" / source_id
    docs_path = source_dir / "documents.jsonl"
    chunks_path = source_dir / "chunks.jsonl"

    if not docs_path.is_file():
        raise FileNotFoundError(f"documents.jsonl missing for source {source_id}: {docs_path}")

    stats = ProcessStats()
    dedup = MinHashDedup(threshold=dedup_threshold)

    chunks_tmp = chunks_path.with_suffix(".jsonl.tmp")
    with docs_path.open("r", encoding="utf-8") as docs_f, chunks_tmp.open(
        "w", encoding="utf-8"
    ) as out:
        for raw_line in docs_f:
            line = raw_line.strip()
            if not line:
                continue
            stats.docs_processed += 1
            try:
                doc = json.loads(line)
            except json.JSONDecodeError as e:
                stats.failures.append({"doc_id": "?", "error": f"jsonl parse: {e}"})
                continue

            doc_id = doc.get("doc_id", "unknown")
            text = doc.get("text", "")

            # Language filter (skip when configured to None).
            if language is not None and not is_language(text, language):
                stats.docs_filtered_language += 1
                continue

            # Quality filter.
            ok, reason = passes_quality(text)
            if not ok:
                stats.docs_filtered_quality += 1
                continue

            # Chunk.
            doc_meta = {"title": doc.get("title", "")}
            doc_meta.update(doc.get("metadata") or {})
            try:
                chunks = chunk_document(
                    doc_id=doc_id,
                    source_id=source_id,
                    text=text,
                    metadata=doc_meta,
                    config=chunking,
                )
            except Exception as exc:  # noqa: BLE001
                log.warning("chunking failed for %s: %s", doc_id, exc)
                stats.failures.append({"doc_id": doc_id, "error": f"chunk: {type(exc).__name__}: {exc}"})
                continue
            stats.chunks_emitted += len(chunks)

            # MinHash dedup at chunk level.
            for ch in chunks:
                is_dup, _ = dedup.add_unless_duplicate(ch.text, ch.chunk_id)
                if is_dup:
                    stats.chunks_dedup_dropped += 1
                    continue
                out.write(ch.to_json_line())
                out.write("\n")
                stats.chunks_kept += 1

    if stats.chunks_kept == 0:
        chunks_tmp.unlink(missing_ok=True)
    else:
        os.replace(chunks_tmp, chunks_path)

    log.info("processed source %s: %s", source_id, stats.as_dict())
    return stats


def append_chunks_summary_to_manifest(project_root: Path, source_id: str, stats: ProcessStats) -> None:
    """Bolt a `chunks` block onto the existing source.json so the UI / agent can see what
    happened without re-reading chunks.jsonl."""

    manifest_path = project_root / "sources" / source_id / "source.json"
    if not manifest_path.is_file():
        return
    data = json.loads(manifest_path.read_text("utf-8"))
    data["chunks"] = stats.as_dict()
    tmp = manifest_path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8")
    os.replace(tmp, manifest_path)
