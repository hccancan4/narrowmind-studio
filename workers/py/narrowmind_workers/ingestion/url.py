"""URL ingestion + bounded crawl.

Single URL: fetch + extract with trafilatura. Bounded crawl: BFS from the seed URL,
optionally restricted to the same domain, capped at ``max_pages`` and ``max_depth``.

We don't aggressively respect robots.txt here yet — that's a Phase 7 polish item.
Default behaviour is one URL one fetch, which is hard to abuse.
"""

from __future__ import annotations

import logging
import os
import time
from pathlib import Path
from urllib.parse import urldefrag, urljoin, urlparse

import trafilatura
from bs4 import BeautifulSoup

from narrowmind_workers.ingestion._common import (
    fresh_doc_id,
    fresh_source_id,
    write_manifest_atomic,
)
from narrowmind_workers.ingestion.handlers.html import extract_html
from narrowmind_workers.ingestion.models import Document, SourceManifest, SourceType

log = logging.getLogger(__name__)

# Half-second pause between fetches inside a crawl; single-URL ingest takes no pause.
CRAWL_PAUSE_SECONDS = 0.5


def ingest_url(
    project_root: Path,
    url: str,
    max_depth: int = 0,
    max_pages: int = 1,
    same_domain_only: bool = True,
    source_id: str | None = None,
) -> SourceManifest:
    if max_pages < 1:
        raise ValueError("max_pages must be >= 1")
    if max_depth < 0:
        raise ValueError("max_depth must be >= 0")

    source_id = source_id or fresh_source_id(_host(url) or "url")
    manifest = SourceManifest.new(
        source_id=source_id,
        source_type=SourceType.URL,
        params={
            "seed_url": url,
            "max_depth": max_depth,
            "max_pages": max_pages,
            "same_domain_only": same_domain_only,
        },
    )

    source_dir = project_root / "sources" / source_id
    source_dir.mkdir(parents=True, exist_ok=True)
    docs_path = source_dir / "documents.jsonl"
    docs_tmp = docs_path.with_suffix(".jsonl.tmp")

    seed_host = _host(url)
    visited: set[str] = set()
    queue: list[tuple[str, int]] = [(url, 0)]
    docs_written = 0

    with docs_tmp.open("w", encoding="utf-8") as out:
        while queue and docs_written < max_pages:
            cur, depth = queue.pop(0)
            cur, _frag = urldefrag(cur)
            if cur in visited:
                continue
            visited.add(cur)
            try:
                doc, html_blob = _fetch_one(cur)
                out.write(doc.to_json_line())
                out.write("\n")
                docs_written += 1
            except Exception as exc:  # noqa: BLE001
                log.warning("url fetch/extract failed for %s: %s", cur, exc)
                manifest.failure_count += 1
                manifest.failures.append(
                    {"path": cur, "error": f"{type(exc).__name__}: {exc}"}
                )
                if max_depth > 0:
                    time.sleep(CRAWL_PAUSE_SECONDS)
                continue

            if depth < max_depth and docs_written < max_pages:
                for link in extract_links(html_blob, base_url=cur, same_host=seed_host if same_domain_only else None):
                    if link not in visited:
                        queue.append((link, depth + 1))
                time.sleep(CRAWL_PAUSE_SECONDS)

    if docs_written == 0:
        docs_tmp.unlink(missing_ok=True)
    else:
        os.replace(docs_tmp, docs_path)

    manifest.document_count = docs_written
    write_manifest_atomic(source_dir / "source.json", manifest)
    return manifest


def _fetch_one(url: str) -> tuple[Document, str]:
    blob = trafilatura.fetch_url(url)
    if not blob:
        raise RuntimeError(f"fetch returned no body for {url}")
    title, text, metadata = extract_html(blob, fallback_title=url)
    if not text.strip():
        raise RuntimeError("extracted body was empty")
    metadata.setdefault("html_url", url)
    doc = Document(
        doc_id=fresh_doc_id(_host(url) or "url"),
        title=title,
        text=text,
        source_path=url,
        metadata=metadata,
    )
    return doc, blob


def extract_links(html: str, base_url: str, same_host: str | None) -> list[str]:
    """Find ``<a href>`` targets, normalise relative to ``base_url``, optionally restrict
    to a single host. Strips fragments. Returns unique URLs preserving discovery order."""

    soup = BeautifulSoup(html, "html.parser")
    seen: set[str] = set()
    out: list[str] = []
    for a in soup.find_all("a", href=True):
        raw = a["href"]
        if raw.startswith(("javascript:", "mailto:", "tel:")):
            continue
        absolute = urljoin(base_url, raw)
        absolute, _frag = urldefrag(absolute)
        parsed = urlparse(absolute)
        if parsed.scheme not in ("http", "https"):
            continue
        if same_host is not None and parsed.netloc != same_host:
            continue
        if absolute in seen:
            continue
        seen.add(absolute)
        out.append(absolute)
    return out


def _host(url: str) -> str | None:
    try:
        return urlparse(url).netloc or None
    except ValueError:
        return None
