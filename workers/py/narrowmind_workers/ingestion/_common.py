"""Helpers shared between the ingestion source handlers.

Pulled out of :mod:`narrowmind_workers.ingestion.local` so the Wikipedia / URL / HF
handlers can reuse the same id-generation + atomic write conventions.
"""

from __future__ import annotations

import json
import os
import uuid
from pathlib import Path

from narrowmind_workers.ingestion.models import SourceManifest


def fresh_source_id(seed: str) -> str:
    return f"{_slugify(seed)[:24]}-{uuid.uuid4().hex[:6]}"


def fresh_doc_id(seed: str) -> str:
    return f"{_slugify(seed)[:24]}-{uuid.uuid4().hex[:6]}"


def _slugify(s: str) -> str:
    out = []
    for c in s.lower():
        if c.isalnum():
            out.append(c)
        elif c in (" ", "-", "_", ".", "/"):
            out.append("-")
    return "".join(out).strip("-") or "x"


def write_manifest_atomic(path: Path, manifest: SourceManifest) -> None:
    tmp = path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(manifest.to_dict(), ensure_ascii=False, indent=2), encoding="utf-8")
    os.replace(tmp, path)
