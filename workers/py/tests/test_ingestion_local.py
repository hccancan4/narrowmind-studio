"""End-to-end tests for the local-path ingestion entry point.

We ingest a small mixed-format directory into a temporary project and assert on the
written ``source.json`` + ``documents.jsonl``.
"""

from __future__ import annotations

import json
import shutil
from pathlib import Path

import pytest

from narrowmind_workers.ingestion.local import ingest_local_path
from narrowmind_workers.ingestion.models import SourceType

FIXTURE_DIR = Path(__file__).parent / "fixtures"


@pytest.fixture()
def project_dir(tmp_path: Path) -> Path:
    (tmp_path / "sources").mkdir()
    return tmp_path


def test_ingest_single_text_file_writes_one_document(project_dir: Path) -> None:
    manifest = ingest_local_path(project_dir, FIXTURE_DIR / "sample.txt")
    assert manifest.document_count == 1
    assert manifest.failure_count == 0
    assert manifest.source_type == SourceType.LOCAL_FILE
    source_dir = project_dir / "sources" / manifest.source_id

    docs = [json.loads(line) for line in (source_dir / "documents.jsonl").read_text("utf-8").splitlines()]
    assert len(docs) == 1
    assert docs[0]["title"].lower().startswith("the hard problem")
    assert "Second paragraph" in docs[0]["text"]
    assert docs[0]["metadata"]["format"] == "txt"

    persisted = json.loads((source_dir / "source.json").read_text("utf-8"))
    assert persisted["document_count"] == 1
    assert persisted["source_type"] == "local_file"


def test_ingest_directory_recurses_and_writes_one_jsonl(
    project_dir: Path, tmp_path: Path, pdf_fixture: Path, docx_fixture: Path
) -> None:
    # Copy a heterogenous set of fixtures into a single dir.
    src_dir = tmp_path / "incoming"
    src_dir.mkdir()
    shutil.copy(FIXTURE_DIR / "sample.txt", src_dir / "a.txt")
    shutil.copy(FIXTURE_DIR / "sample.md", src_dir / "b.md")
    shutil.copy(FIXTURE_DIR / "sample.html", src_dir / "c.html")
    shutil.copy(pdf_fixture, src_dir / "d.pdf")
    shutil.copy(docx_fixture, src_dir / "e.docx")
    # And one file with no handler that should be skipped silently (no failure recorded).
    (src_dir / "skip.bin").write_bytes(b"\x00\x01")

    manifest = ingest_local_path(project_dir, src_dir)
    assert manifest.document_count == 5
    assert manifest.failure_count == 0
    assert manifest.source_type == SourceType.LOCAL_DIR


def test_ingest_records_per_file_failures_without_aborting(
    project_dir: Path, tmp_path: Path
) -> None:
    src_dir = tmp_path / "incoming"
    src_dir.mkdir()
    shutil.copy(FIXTURE_DIR / "sample.txt", src_dir / "good.txt")
    # Pretend-PDF that PyMuPDF will reject when it tries to open it.
    (src_dir / "bad.pdf").write_bytes(b"not actually a pdf")

    manifest = ingest_local_path(project_dir, src_dir)
    assert manifest.document_count == 1
    assert manifest.failure_count == 1
    assert manifest.failures[0]["path"].endswith("bad.pdf")


def test_ingest_with_custom_source_id_uses_it(project_dir: Path) -> None:
    manifest = ingest_local_path(
        project_dir, FIXTURE_DIR / "sample.txt", source_id="my-fixed-id"
    )
    assert manifest.source_id == "my-fixed-id"
    assert (project_dir / "sources" / "my-fixed-id" / "source.json").is_file()


def test_ingest_missing_target_raises(project_dir: Path) -> None:
    with pytest.raises(FileNotFoundError):
        ingest_local_path(project_dir, project_dir / "does-not-exist")
