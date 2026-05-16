"""Sentence-aware token-window chunker.

Per Phase 2 design decision:
- Default chunk size: **512 tokens**, overlap **64 tokens**.
- Sentence-aware: a chunk never cuts mid-sentence; it greedily packs sentences until
  adding the next one would exceed ``target_tokens``.
- Token counts use ``tiktoken``'s `cl100k_base` — close enough to BGE-small's actual
  tokenizer (~10-20 % off in either direction) and importantly **offline**, so Phase 2
  ingest never triggers a HuggingFace download (decision B).
- Overlap is implemented by walking backwards from each emitted chunk's last sentence,
  accumulating tokens until reaching ``overlap_tokens``, and starting the next chunk
  from there. Forward progress is always at least one sentence to avoid infinite loops
  on pathological inputs (one sentence longer than ``target_tokens``).

The chunker is a pure function over text + config; pipeline orchestration (read
documents.jsonl, filter, dedup, write chunks.jsonl) lives in
:mod:`narrowmind_workers.ingestion.pipeline`.
"""

from __future__ import annotations

import logging
import uuid
from dataclasses import asdict, dataclass, field
from typing import Any

import pysbd
import tiktoken

log = logging.getLogger(__name__)

# Module-scoped encoder. cl100k_base is bundled in the tiktoken wheel — no network access.
_ENC = tiktoken.get_encoding("cl100k_base")


@dataclass(frozen=True)
class ChunkingConfig:
    target_tokens: int = 512
    overlap_tokens: int = 64
    min_chunk_tokens: int = 50  # drop fragments smaller than this

    def __post_init__(self) -> None:
        if self.target_tokens <= 0:
            raise ValueError(f"target_tokens must be > 0, got {self.target_tokens}")
        if not 0 <= self.overlap_tokens < self.target_tokens:
            raise ValueError(
                f"overlap_tokens must be in [0, target_tokens), got {self.overlap_tokens} / {self.target_tokens}"
            )


DEFAULT_CONFIG = ChunkingConfig()


@dataclass
class Chunk:
    chunk_id: str
    doc_id: str
    source_id: str
    text: str
    token_count: int
    # Inclusive sentence indices [start, end] within the source document.
    sentence_range: tuple[int, int]
    # Whether the user / agent has explicitly excluded this chunk from build_dataset.
    # Mutated by `filter_chunks`. True (kept) by default.
    include: bool = True
    metadata: dict[str, Any] = field(default_factory=dict)

    def to_json_line(self) -> str:
        import json

        d = asdict(self)
        d["sentence_range"] = list(self.sentence_range)
        return json.dumps(d, ensure_ascii=False)


def count_tokens(text: str) -> int:
    return len(_ENC.encode(text))


def chunk_document(
    doc_id: str,
    source_id: str,
    text: str,
    metadata: dict[str, Any] | None = None,
    config: ChunkingConfig = DEFAULT_CONFIG,
) -> list[Chunk]:
    """Split a document into sentence-aware chunks. Returns an empty list when the
    document has no sentences (whitespace only)."""

    text = text.strip()
    if not text:
        return []

    seg = pysbd.Segmenter(language="en", clean=False)
    sentences = seg.segment(text)
    if not sentences:
        return []

    sentence_tokens = [count_tokens(s) for s in sentences]

    chunks: list[Chunk] = []
    base_metadata = dict(metadata or {})
    i = 0
    n = len(sentences)
    while i < n:
        # Greedy forward pack: include sentences until adding one would exceed target.
        cur_tokens = sentence_tokens[i]
        j = i + 1
        while j < n and cur_tokens + sentence_tokens[j] <= config.target_tokens:
            cur_tokens += sentence_tokens[j]
            j += 1
        end_inclusive = j - 1
        chunk_text = " ".join(sentences[i : j]).strip()

        if cur_tokens >= config.min_chunk_tokens and chunk_text:
            chunks.append(
                Chunk(
                    chunk_id=_fresh_chunk_id(),
                    doc_id=doc_id,
                    source_id=source_id,
                    text=chunk_text,
                    token_count=cur_tokens,
                    sentence_range=(i, end_inclusive),
                    metadata=base_metadata.copy(),
                )
            )

        if j >= n:
            break

        # Compute next chunk start using overlap: walk backwards from end_inclusive,
        # accumulating tokens until we reach overlap_tokens.
        overlap_acc = 0
        k = end_inclusive
        while k > i and overlap_acc + sentence_tokens[k] <= config.overlap_tokens:
            overlap_acc += sentence_tokens[k]
            k -= 1
        # Always advance by at least one sentence; protects against the degenerate case
        # where a single sentence is itself longer than target_tokens.
        next_i = max(i + 1, k + 1)
        i = next_i

    return chunks


def _fresh_chunk_id() -> str:
    return f"ck_{uuid.uuid4().hex[:10]}"
