"""Wikipedia category scraper.

Walks a category tree breadth-first up to ``max_depth`` levels, dedups page titles,
caps at ``max_pages`` articles, and pulls plain text via the ``wikipedia-api`` package.

Per Phase 2 decision E:
- ``user_agent`` is `"NarrowMindStudio/{version} (https://...)"` so Wikipedia can identify
  us. Version comes from :mod:`narrowmind_workers` at runtime — never hard-coded.
- Conservative 1 req/sec rate limit applied around each ``page.text`` access.
"""

from __future__ import annotations

import logging
import os
import time
from pathlib import Path

import wikipediaapi

from narrowmind_workers import __version__
from narrowmind_workers.ingestion.models import Document, SourceManifest, SourceType
from narrowmind_workers.ingestion._common import (
    fresh_doc_id,
    fresh_source_id,
    write_manifest_atomic,
)

log = logging.getLogger(__name__)

WIKIPEDIA_NAMESPACE_ARTICLE = 0
WIKIPEDIA_NAMESPACE_CATEGORY = 14

# Phase 2 decision E.
RATE_LIMIT_SECONDS = 1.0


def user_agent() -> str:
    return (
        f"NarrowMindStudio/{__version__} (https://github.com/hccancan4/narrowmind-studio)"
    )


def ingest_wikipedia_category(
    project_root: Path,
    category: str,
    language: str = "en",
    max_depth: int = 1,
    max_pages: int = 50,
    source_id: str | None = None,
) -> SourceManifest:
    wiki = wikipediaapi.Wikipedia(user_agent=user_agent(), language=language)
    root_cat = wiki.page(f"Category:{category}")
    if not root_cat.exists():
        raise ValueError(f"Wikipedia category does not exist: Category:{category}")

    pages = collect_category_pages(root_cat, max_depth=max_depth, max_pages=max_pages)
    log.info("wikipedia category `%s` resolved to %d pages", category, len(pages))

    source_id = source_id or fresh_source_id(f"wiki-{category}")
    manifest = SourceManifest.new(
        source_id=source_id,
        source_type=SourceType.WIKIPEDIA,
        params={
            "category": category,
            "language": language,
            "max_depth": max_depth,
            "max_pages": max_pages,
        },
    )

    source_dir = project_root / "sources" / source_id
    source_dir.mkdir(parents=True, exist_ok=True)
    docs_path = source_dir / "documents.jsonl"
    docs_tmp = docs_path.with_suffix(".jsonl.tmp")

    docs_written = 0
    with docs_tmp.open("w", encoding="utf-8") as out:
        for page in pages:
            try:
                # `page.text` triggers an API fetch; rate-limit *around* it.
                text = page.text
                if not text.strip():
                    raise ValueError("empty page text from Wikipedia")
                doc = Document(
                    doc_id=fresh_doc_id(page.title),
                    title=page.title,
                    text=text,
                    source_path=page.fullurl,
                    metadata={
                        "format": "wikipedia",
                        "language": language,
                        "summary": (page.summary or "")[:500],
                    },
                )
                out.write(doc.to_json_line())
                out.write("\n")
                docs_written += 1
            except Exception as exc:  # noqa: BLE001
                log.warning("wikipedia extract failed for %s: %s", page.title, exc)
                manifest.failure_count += 1
                manifest.failures.append(
                    {"path": page.fullurl, "error": f"{type(exc).__name__}: {exc}"}
                )
            time.sleep(RATE_LIMIT_SECONDS)

    if docs_written == 0:
        docs_tmp.unlink(missing_ok=True)
    else:
        os.replace(docs_tmp, docs_path)

    manifest.document_count = docs_written
    write_manifest_atomic(source_dir / "source.json", manifest)
    return manifest


def collect_category_pages(
    cat_page: "wikipediaapi.WikipediaPage",
    max_depth: int,
    max_pages: int,
) -> list["wikipediaapi.WikipediaPage"]:
    """Breadth-first walk of a category; returns up to `max_pages` article pages.

    Sub-categories at depth > `max_depth` are not descended into. Visited titles are
    tracked so the same article appearing in multiple sub-categories is counted once.
    """

    pages: list[wikipediaapi.WikipediaPage] = []
    visited: set[str] = set()
    # (category page, depth)
    queue: list[tuple[wikipediaapi.WikipediaPage, int]] = [(cat_page, 0)]

    while queue and len(pages) < max_pages:
        cat, depth = queue.pop(0)
        for title, member in cat.categorymembers.items():
            if title in visited:
                continue
            visited.add(title)
            if member.ns == WIKIPEDIA_NAMESPACE_ARTICLE:
                pages.append(member)
                if len(pages) >= max_pages:
                    break
            elif member.ns == WIKIPEDIA_NAMESPACE_CATEGORY and depth < max_depth:
                queue.append((member, depth + 1))
    return pages[:max_pages]
