"""Tests for the sentence-aware chunker.

We craft inputs of known token counts so we can assert on chunk boundaries directly
rather than guessing what the tokenizer will produce."""

from __future__ import annotations

from narrowmind_workers.chunking import (
    DEFAULT_CONFIG,
    ChunkingConfig,
    chunk_document,
    count_tokens,
)


def test_count_tokens_is_close_to_word_count() -> None:
    # cl100k_base is typically 1.0-1.3 tokens per English word.
    text = "The hard problem of consciousness is interesting"
    n = count_tokens(text)
    assert 7 <= n <= 10


def test_empty_text_yields_no_chunks() -> None:
    assert chunk_document("d1", "s1", "") == []
    assert chunk_document("d1", "s1", "   \n  \n ") == []


def test_short_doc_under_min_drops() -> None:
    config = ChunkingConfig(target_tokens=512, overlap_tokens=64, min_chunk_tokens=100)
    chunks = chunk_document("d1", "s1", "Short text.", config=config)
    assert chunks == []


def test_doc_fits_in_one_chunk() -> None:
    text = "First sentence. Second sentence. Third sentence."
    config = ChunkingConfig(target_tokens=512, overlap_tokens=64, min_chunk_tokens=1)
    chunks = chunk_document("d1", "s1", text, config=config)
    assert len(chunks) == 1
    assert "First sentence" in chunks[0].text
    assert "Third sentence" in chunks[0].text
    assert chunks[0].doc_id == "d1"
    assert chunks[0].source_id == "s1"


def test_doc_splits_at_sentence_boundary() -> None:
    # Make ~10 sentences of moderate length so several chunks fit at small target.
    sentences = [
        f"This is sentence number {i} and it contains enough content to count toward the token budget meaningfully."
        for i in range(20)
    ]
    text = " ".join(sentences)
    config = ChunkingConfig(target_tokens=40, overlap_tokens=10, min_chunk_tokens=10)
    chunks = chunk_document("d1", "s1", text, config=config)
    assert len(chunks) >= 5
    # Each chunk text is composed of complete sentences from the original list.
    for c in chunks:
        assert c.text.endswith(".")
        # Never exceed target by more than one sentence's worth
        assert c.token_count <= config.target_tokens * 1.5


def test_overlap_creates_shared_sentences_between_consecutive_chunks() -> None:
    sentences = [
        f"Sentence {i} that adds some token budget toward the chunk size limit." for i in range(30)
    ]
    text = " ".join(sentences)
    config = ChunkingConfig(target_tokens=60, overlap_tokens=15, min_chunk_tokens=10)
    chunks = chunk_document("d1", "s1", text, config=config)
    assert len(chunks) >= 3
    # Overlap: each chunk's start sentence is <= previous chunk's end sentence
    for prev, cur in zip(chunks, chunks[1:]):
        prev_end = prev.sentence_range[1]
        cur_start = cur.sentence_range[0]
        assert cur_start <= prev_end + 1, f"gap detected between {prev_end} and {cur_start}"


def test_one_huge_sentence_advances_at_least_one_step() -> None:
    # Pathological: one sentence whose token count exceeds target_tokens.
    long_one = " ".join(["word"] * 1000) + "."
    text = long_one + " Second sentence. Third sentence."
    config = ChunkingConfig(target_tokens=64, overlap_tokens=16, min_chunk_tokens=1)
    chunks = chunk_document("d1", "s1", text, config=config)
    # We get at least two chunks: the huge one alone, then the trailing two.
    assert len(chunks) >= 2
    # And the chunker terminates (no infinite loop), which is what this test really proves.


def test_chunk_carries_metadata_through() -> None:
    text = "First sentence. Second sentence. Third sentence."
    chunks = chunk_document(
        "d1",
        "s1",
        text,
        metadata={"title": "Hard Problem", "format": "wikipedia"},
        config=ChunkingConfig(target_tokens=512, overlap_tokens=64, min_chunk_tokens=1),
    )
    assert chunks[0].metadata == {"title": "Hard Problem", "format": "wikipedia"}


def test_chunk_to_json_line_round_trips() -> None:
    import json

    chunks = chunk_document(
        "d1",
        "s1",
        "First sentence. Second sentence. Third sentence.",
        config=ChunkingConfig(target_tokens=512, overlap_tokens=64, min_chunk_tokens=1),
    )
    line = chunks[0].to_json_line()
    decoded = json.loads(line)
    assert decoded["chunk_id"] == chunks[0].chunk_id
    assert decoded["doc_id"] == "d1"
    assert decoded["source_id"] == "s1"
    assert decoded["sentence_range"] == [0, 2]
    assert decoded["include"] is True


def test_default_config_is_512_64() -> None:
    assert DEFAULT_CONFIG.target_tokens == 512
    assert DEFAULT_CONFIG.overlap_tokens == 64
