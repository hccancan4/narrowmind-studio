"""Per-handler smoke tests. Each handler returns (title, text, metadata) — we just
need the basics to come through."""

from __future__ import annotations

from pathlib import Path

from narrowmind_workers.ingestion.handlers import docx as docx_handler
from narrowmind_workers.ingestion.handlers import html as html_handler
from narrowmind_workers.ingestion.handlers import pdf as pdf_handler
from narrowmind_workers.ingestion.handlers import text as text_handler

FIXTURE_DIR = Path(__file__).parent / "fixtures"


def test_text_handler_reads_txt() -> None:
    title, text, meta = text_handler.extract(FIXTURE_DIR / "sample.txt")
    assert "hard problem of consciousness" in title.lower()
    assert "Second paragraph" in text
    assert meta["format"] == "txt"


def test_text_handler_reads_markdown() -> None:
    title, text, meta = text_handler.extract(FIXTURE_DIR / "sample.md")
    # Markdown markup is preserved (we don't strip ** or # at ingestion)
    assert "**markdown**" in text
    assert meta["format"] == "md"
    # Title comes from first non-empty stripped line, after removing `#`
    assert "Phenomenal consciousness" in title


def test_html_handler_strips_boilerplate() -> None:
    title, text, meta = html_handler.extract(FIXTURE_DIR / "sample.html")
    assert title.startswith("Qualia")
    # Article body present, nav/aside/footer skipped by trafilatura. The article body itself
    # mentions "sidebar" so we can't just grep for that — assert on the actual chrome strings.
    assert "article body" in text.lower()
    assert "site navigation that trafilatura should strip" not in text.lower()
    assert "a sidebar we don't care about" not in text.lower()
    assert "footer content, also boilerplate" not in text.lower()
    assert meta["format"] == "html"


def test_pdf_handler_reads_generated_pdf(pdf_fixture: Path) -> None:
    title, text, meta = pdf_handler.extract(pdf_fixture)
    assert "hard problem" in text.lower()
    assert meta["format"] == "pdf"
    assert meta["page_count"] == 1


def test_docx_handler_reads_generated_docx(docx_fixture: Path) -> None:
    title, text, meta = docx_handler.extract(docx_fixture)
    assert "Functionalism" in title
    assert "multiple realizability" in text
    assert meta["format"] == "docx"
    assert meta["paragraph_count"] >= 2
