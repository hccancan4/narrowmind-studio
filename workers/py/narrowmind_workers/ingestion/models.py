"""Data models written to disk under ``<project>/sources/<source_id>/``.

The filesystem layout is the contract — the Rust orchestrator reads these JSON files,
the UI renders them, and downstream cleaning / chunking workers append to ``documents.jsonl``
or rewrite it in place. Keep field names stable.
"""

from __future__ import annotations

import json
from dataclasses import asdict, dataclass, field
from datetime import UTC, datetime
from enum import Enum
from typing import Any


class SourceType(str, Enum):
    """How a source was ingested. Used by the UI to show the right icon and re-ingest flow."""

    LOCAL_FILE = "local_file"
    LOCAL_DIR = "local_dir"
    URL = "url"
    WIKIPEDIA = "wikipedia"
    HF_DATASET = "hf_dataset"


@dataclass
class SourceManifest:
    """Written to ``<source_dir>/source.json`` exactly once at ingestion time.

    Subsequent cleaning / chunking passes update ``documents.jsonl`` and ``chunks.jsonl``
    in place; they never rewrite the manifest. The manifest is the audit trail of *how*
    a source got into the project.
    """

    source_id: str
    source_type: SourceType
    created_at: str  # ISO 8601 UTC
    # Free-form record of how this source was constructed; shape depends on source_type.
    params: dict[str, Any]
    # Populated by the ingestion call once it finishes; updated atomically.
    document_count: int = 0
    failure_count: int = 0
    failures: list[dict[str, Any]] = field(default_factory=list)

    @staticmethod
    def new(source_id: str, source_type: SourceType, params: dict[str, Any]) -> SourceManifest:
        return SourceManifest(
            source_id=source_id,
            source_type=source_type,
            created_at=datetime.now(UTC).isoformat(),
            params=params,
        )

    def to_dict(self) -> dict[str, Any]:
        d = asdict(self)
        d["source_type"] = self.source_type.value
        return d


@dataclass
class Document:
    """One extracted document. Many per source.

    ``text`` is plain UTF-8; format-specific markup (PDF layout, Word styles, HTML tags)
    has already been stripped. ``metadata`` carries whatever the handler thought was
    worth keeping — page count for PDFs, author for DOCX, canonical URL for web, etc.
    Downstream chunking + cleaning honour ``metadata`` but never depend on specific keys.
    """

    doc_id: str
    title: str
    text: str
    source_path: str  # relative to the project root if local, URL otherwise
    metadata: dict[str, Any] = field(default_factory=dict)
    # Approximate byte length of `text` — handy for the UI without re-decoding.
    byte_length: int = 0

    def __post_init__(self) -> None:
        if self.byte_length == 0:
            self.byte_length = len(self.text.encode("utf-8"))

    def to_json_line(self) -> str:
        return json.dumps(asdict(self), ensure_ascii=False)
