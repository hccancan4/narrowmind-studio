"""PDF extraction via PyMuPDF (fitz).

We pull plain text per page and join with two newlines so the chunker sees page breaks
as natural sentence boundaries. PDF layouts vary wildly — this is a starting point that
will get refined when we hit real-world failure cases.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import fitz  # pymupdf


def extract(path: Path) -> tuple[str, str, dict[str, Any]]:
    pages: list[str] = []
    metadata: dict[str, Any] = {"format": "pdf"}
    with fitz.open(path) as doc:
        metadata["page_count"] = doc.page_count
        info = doc.metadata or {}
        if info.get("title"):
            metadata["pdf_title"] = info["title"]
        if info.get("author"):
            metadata["pdf_author"] = info["author"]
        for page in doc:
            text = page.get_text("text") or ""
            pages.append(text.strip())

    body = "\n\n".join(p for p in pages if p)
    title = (
        metadata.get("pdf_title")
        or _first_meaningful_line(body)
        or path.stem
    )
    return title[:120], body, metadata


def _first_meaningful_line(text: str) -> str | None:
    for line in text.splitlines():
        s = line.strip()
        if len(s) >= 4:
            return s
    return None
