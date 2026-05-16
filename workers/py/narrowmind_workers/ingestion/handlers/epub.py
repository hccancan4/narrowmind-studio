"""EPUB extraction via EbookLib.

EPUB is XHTML packaged in a zip — we pull text out of each ITEM_DOCUMENT chapter and
concatenate. Front-matter (nav, copyright pages) is left in because the chunker can
re-decide downstream.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from bs4 import BeautifulSoup
from ebooklib import ITEM_DOCUMENT, epub


def extract(path: Path) -> tuple[str, str, dict[str, Any]]:
    book = epub.read_epub(str(path))

    parts: list[str] = []
    for item in book.get_items_of_type(ITEM_DOCUMENT):
        soup = BeautifulSoup(item.get_body_content(), "html.parser")
        text = soup.get_text(separator="\n").strip()
        if text:
            parts.append(text)

    body = "\n\n".join(parts)

    title = path.stem
    title_meta = book.get_metadata("DC", "title")
    if title_meta:
        title = title_meta[0][0]

    metadata: dict[str, Any] = {"format": "epub", "chapter_count": len(parts)}
    author_meta = book.get_metadata("DC", "creator")
    if author_meta:
        metadata["epub_author"] = author_meta[0][0]

    return title[:120], body, metadata
