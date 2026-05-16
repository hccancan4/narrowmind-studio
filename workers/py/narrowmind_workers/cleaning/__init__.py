"""Cleaning pipeline primitives: language filter, quality heuristics, MinHash dedup.

These are reusable building blocks. The chunking pass in M4 wires them together: per-doc
language + quality filter, then chunk, then per-chunk MinHash dedup before writing
``chunks.jsonl``. They're tested in isolation here.
"""

from narrowmind_workers.cleaning.language import detect_language, is_english
from narrowmind_workers.cleaning.minhash import MinHashDedup
from narrowmind_workers.cleaning.quality import passes_quality

__all__ = [
    "MinHashDedup",
    "detect_language",
    "is_english",
    "passes_quality",
]
