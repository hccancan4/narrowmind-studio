"""Unit tests for URL ingestion's link-extraction helper.

Live HTTP is intentionally NOT exercised here — the wikipedia + url RPC paths are
covered end-to-end by the Phase 2 acceptance prompt, not unit tests. We do cover the
pure logic (link discovery, domain filtering, fragment stripping)."""

from __future__ import annotations

from narrowmind_workers.ingestion.url import extract_links


def test_extract_links_finds_anchors_and_normalises_relative_urls() -> None:
    html = """<html><body>
        <a href="/a">Page A</a>
        <a href="https://example.com/b">Page B</a>
        <a href="b/c#section">Page B/C</a>
        <a href="javascript:void(0)">js</a>
        <a href="mailto:x@y.com">mail</a>
    </body></html>"""
    links = extract_links(html, base_url="https://example.com/", same_host=None)
    assert "https://example.com/a" in links
    assert "https://example.com/b" in links
    # Fragments stripped
    assert any(l.startswith("https://example.com/b/c") and "#" not in l for l in links)
    # JS / mailto schemes skipped
    assert not any("javascript" in l or "mailto" in l for l in links)


def test_extract_links_filters_by_same_host() -> None:
    html = """<html><body>
        <a href="https://example.com/a">on-domain</a>
        <a href="https://other.com/b">off-domain</a>
    </body></html>"""
    links = extract_links(html, base_url="https://example.com/", same_host="example.com")
    assert links == ["https://example.com/a"]


def test_extract_links_deduplicates() -> None:
    html = """<html><body>
        <a href="/a">first</a>
        <a href="/a#frag">same after defrag</a>
        <a href="/a">repeat</a>
    </body></html>"""
    links = extract_links(html, base_url="https://example.com/", same_host=None)
    assert links == ["https://example.com/a"]


def test_extract_links_skips_non_http_schemes() -> None:
    html = """<html><body>
        <a href="ftp://example.com/foo">ftp</a>
        <a href="data:text/plain,hi">data</a>
        <a href="https://example.com/ok">ok</a>
    </body></html>"""
    links = extract_links(html, base_url="https://example.com/", same_host=None)
    assert links == ["https://example.com/ok"]
