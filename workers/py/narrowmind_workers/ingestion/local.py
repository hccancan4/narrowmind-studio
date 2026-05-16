"""Local file / directory ingestion.

Given a path to a single file or a directory, run the appropriate handler against every
supported file under it (recursively for directories), and write the results to
``<project>/sources/<source_id>/``:

- ``source.json``     — :class:`SourceManifest`
- ``documents.jsonl`` — one :class:`Document` per line

Both files are written atomically (`.tmp` swap) so a crash mid-write does not leave the
project in a half-ingested state.
"""

from __future__ import annotations

import json
import logging
import os
import uuid
from pathlib import Path

from narrowmind_workers.ingestion.dispatch import handler_for, supported_extensions
from narrowmind_workers.ingestion.models import Document, SourceManifest, SourceType

log = logging.getLogger(__name__)


def ingest_local_path(
    project_root: Path,
    target: Path,
    source_id: str | None = None,
) -> SourceManifest:
    """Ingest ``target`` (file or directory) into the given project.

    Returns the persisted :class:`SourceManifest` so the caller (Rust orchestrator) can
    surface the source id, document count, and any per-file failures to the user.
    """

    if not target.exists():
        raise FileNotFoundError(f"ingest target does not exist: {target}")

    source_id = source_id or _fresh_source_id(target)
    source_type = SourceType.LOCAL_DIR if target.is_dir() else SourceType.LOCAL_FILE

    manifest = SourceManifest.new(
        source_id=source_id,
        source_type=source_type,
        params={
            "path": str(target),
            "supported_extensions": supported_extensions(),
        },
    )

    source_dir = project_root / "sources" / source_id
    source_dir.mkdir(parents=True, exist_ok=True)
    docs_path = source_dir / "documents.jsonl"

    files = _iter_target_files(target)
    log.info("ingesting %d files from %s into source %s", len(files), target, source_id)

    docs_tmp = docs_path.with_suffix(".jsonl.tmp")
    docs_written = 0
    with docs_tmp.open("w", encoding="utf-8") as out:
        for file_path in files:
            try:
                doc = _extract_one(file_path, project_root)
            except Exception as exc:  # noqa: BLE001 — record per-file failures, don't abort
                log.warning("extract failed for %s: %s", file_path, exc)
                manifest.failure_count += 1
                manifest.failures.append(
                    {"path": str(file_path), "error": f"{type(exc).__name__}: {exc}"}
                )
                continue
            out.write(doc.to_json_line())
            out.write("\n")
            docs_written += 1

    if docs_written == 0:
        # Don't leave an empty file lying around — caller can detect by failure_count.
        docs_tmp.unlink(missing_ok=True)
    else:
        os.replace(docs_tmp, docs_path)

    manifest.document_count = docs_written
    _write_manifest_atomic(source_dir / "source.json", manifest)
    return manifest


def _extract_one(file_path: Path, project_root: Path) -> Document:
    handler = handler_for(file_path)
    if handler is None:
        raise ValueError(
            f"no handler for extension `{file_path.suffix}` (supported: {supported_extensions()})"
        )
    title, text, metadata = handler(file_path)
    # Store paths relative to the project root when possible so projects can be moved.
    try:
        rel = file_path.resolve().relative_to(project_root.resolve())
        source_path = str(rel).replace("\\", "/")
    except ValueError:
        source_path = str(file_path)
    return Document(
        doc_id=_fresh_doc_id(file_path),
        title=title,
        text=text,
        source_path=source_path,
        metadata=metadata,
    )


def _iter_target_files(target: Path) -> list[Path]:
    if target.is_file():
        return [target]
    files: list[Path] = []
    for path in sorted(target.rglob("*")):
        if not path.is_file():
            continue
        if handler_for(path) is None:
            continue
        files.append(path)
    return files


def _fresh_source_id(target: Path) -> str:
    return f"{_slugify(target.name)[:24]}-{uuid.uuid4().hex[:6]}"


def _fresh_doc_id(file_path: Path) -> str:
    return f"{_slugify(file_path.stem)[:24]}-{uuid.uuid4().hex[:6]}"


def _slugify(s: str) -> str:
    out = []
    for c in s.lower():
        if c.isalnum():
            out.append(c)
        elif c in (" ", "-", "_", "."):
            out.append("-")
    return "".join(out).strip("-") or "x"


def _write_manifest_atomic(path: Path, manifest: SourceManifest) -> None:
    tmp = path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(manifest.to_dict(), ensure_ascii=False, indent=2), encoding="utf-8")
    os.replace(tmp, path)
