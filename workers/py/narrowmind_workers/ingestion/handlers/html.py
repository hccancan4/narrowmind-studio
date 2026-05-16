"""HTML extraction via trafilatura.

Trafilatura strips boilerplate (nav, footer, sidebars, ads) and returns just the article
body — what we'd want for ingestion. Falls back to BeautifulSoup's all-text path when
trafilatura returns nothing (e.g. very minimal markup, fragment files).
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import trafilatura
from bs4 import BeautifulSoup


def extract(path: Path) -> tuple[str, str, dict[str, Any]]:
    raw = path.read_text(encoding="utf-8", errors="replace")
    return extract_html(raw, fallback_title=path.stem)


def extract_html(html: str, fallback_title: str = "untitled") -> tuple[str, str, dict[str, Any]]:
    """Pure-string variant — used by the URL crawler in M2 to avoid a tempfile round-trip."""
    metadata_obj = trafilatura.extract_metadata(html)
    title = fallback_title
    metadata: dict[str, Any] = {"format": "html"}
    if metadata_obj is not None:
        if metadata_obj.title:
            title = metadata_obj.title[:120]
        if metadata_obj.author:
            metadata["html_author"] = metadata_obj.author
        if metadata_obj.url:
            metadata["html_url"] = metadata_obj.url
        if metadata_obj.date:
            metadata["html_date"] = metadata_obj.date

    text = trafilatura.extract(
        html,
        include_comments=False,
        include_tables=True,
        no_fallback=False,
        favor_recall=True,
    )
    if not text:
        soup = BeautifulSoup(html, "html.parser")
        text = soup.get_text(separator="\n").strip()

    return title, text or "", metadata
