"""Tests for the end-to-end clean+chunk pipeline applied to documents.jsonl."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from narrowmind_workers.chunking import ChunkingConfig
from narrowmind_workers.ingestion.pipeline import process_source


@pytest.fixture()
def project_with_docs(tmp_path: Path) -> tuple[Path, str]:
    source_id = "test-source"
    source_dir = tmp_path / "sources" / source_id
    source_dir.mkdir(parents=True)
    docs = [
        {
            "doc_id": "d1",
            "title": "Hard problem",
            "text": (
                "The hard problem of consciousness asks why physical processes give rise to "
                "subjective experience. Coined by David Chalmers in 1995, the term distinguishes "
                "the explanatory gap from easier problems. Easier problems include attention, "
                "memory, and reportability. Functionalism holds that mental states are defined "
                "by their causal role rather than their material substrate."
            ),
            "source_path": "wiki/hard-problem",
            "metadata": {"format": "wikipedia", "language": "en"},
            "byte_length": 0,
        },
        {
            "doc_id": "d2",
            "title": "Phenomenal consciousness",
            "text": (
                "Phenomenal consciousness refers to the subjective qualitative aspects of "
                "experience. It is sometimes contrasted with access consciousness, which "
                "concerns the availability of mental content for reasoning and report. "
                "Philosophers disagree about whether the two come apart."
            ),
            "source_path": "wiki/phenomenal",
            "metadata": {"format": "wikipedia", "language": "en"},
            "byte_length": 0,
        },
    ]
    docs_path = source_dir / "documents.jsonl"
    with docs_path.open("w", encoding="utf-8") as f:
        for d in docs:
            f.write(json.dumps(d, ensure_ascii=False) + "\n")
    return tmp_path, source_id


def test_process_source_writes_chunks_jsonl(project_with_docs: tuple[Path, str]) -> None:
    project_root, source_id = project_with_docs
    stats = process_source(project_root, source_id)
    chunks_path = project_root / "sources" / source_id / "chunks.jsonl"
    assert chunks_path.is_file()
    assert stats.docs_processed == 2
    assert stats.chunks_kept > 0
    lines = chunks_path.read_text("utf-8").splitlines()
    assert len(lines) == stats.chunks_kept
    first = json.loads(lines[0])
    assert "chunk_id" in first
    assert "doc_id" in first
    assert first["source_id"] == source_id
    assert first["include"] is True


def test_process_source_filters_non_english(tmp_path: Path) -> None:
    source_id = "tr-source"
    source_dir = tmp_path / "sources" / source_id
    source_dir.mkdir(parents=True)
    doc = {
        "doc_id": "d1",
        "title": "Turkish",
        "text": (
            "Bilinç'in zor problemi, fiziksel süreçlerin neden öznel deneyime yol açtığını "
            "sorar. David Chalmers tarafından 1995'te ortaya atılan bu terim, açıklayıcı "
            "boşluğu daha kolay problemlerden ayırır. Daha kolay problemler dikkat, hafıza "
            "ve bildirme yeteneği gibi konuları içerir."
        ),
        "source_path": "wiki/zor-problem",
        "metadata": {},
        "byte_length": 0,
    }
    (source_dir / "documents.jsonl").write_text(json.dumps(doc) + "\n", encoding="utf-8")

    stats = process_source(tmp_path, source_id, language="en")
    assert stats.docs_filtered_language == 1
    assert stats.chunks_kept == 0
    # No chunks.jsonl written when nothing survived.
    assert not (source_dir / "chunks.jsonl").exists()


def test_process_source_dedups_near_duplicate_chunks(tmp_path: Path) -> None:
    source_id = "dup-source"
    source_dir = tmp_path / "sources" / source_id
    source_dir.mkdir(parents=True)
    long_text = (
        "The hard problem of consciousness asks why physical processes give rise to subjective experience. "
        "Coined by David Chalmers in 1995, the term distinguishes the explanatory gap from easier problems. "
        "Easier problems include attention, memory, and reportability. The literature on this topic has grown "
        "extensively over the past three decades, with functionalists, dualists, and panpsychists each "
        "proposing distinct frameworks for understanding the phenomenal character of mind."
    )
    docs = [
        {"doc_id": "d1", "title": "A", "text": long_text, "source_path": "a", "metadata": {}, "byte_length": 0},
        {
            "doc_id": "d2",
            "title": "B",
            "text": long_text.replace("physical processes", "neural processes"),
            "source_path": "b",
            "metadata": {},
            "byte_length": 0,
        },
    ]
    with (source_dir / "documents.jsonl").open("w", encoding="utf-8") as f:
        for d in docs:
            f.write(json.dumps(d) + "\n")

    # Use a moderately small chunk so each doc produces 1-2 chunks; the near-dup should land
    # in the second doc and get deduped out.
    stats = process_source(
        tmp_path,
        source_id,
        chunking=ChunkingConfig(target_tokens=120, overlap_tokens=20, min_chunk_tokens=20),
    )
    assert stats.chunks_dedup_dropped >= 1


def test_process_source_missing_documents_raises(tmp_path: Path) -> None:
    with pytest.raises(FileNotFoundError):
        process_source(tmp_path, "missing-source")
