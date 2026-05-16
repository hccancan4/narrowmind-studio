"""MinHash near-duplicate detection.

Backed by `datasketch.MinHashLSH` — buckets candidates whose Jaccard similarity is
likely ≥ ``threshold``. Used at the chunk level (after chunking, before writing
``chunks.jsonl``) so the model never sees the same paragraph twice when chunks of two
related Wikipedia articles overlap.

Default threshold is **0.85** per Phase 2 decision: protective, only obvious duplicates.

Shingling: 3-grams of normalised whitespace-split tokens. Good enough for prose;
character-shingles would be more robust to typos but cost more.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

from datasketch import MinHash, MinHashLSH

_WHITESPACE = re.compile(r"\s+")


@dataclass
class MinHashDedup:
    """Add items by id, ask whether each new item is a near-duplicate of an existing one.

    Stateful — keep a single instance per dedup pass. Construction is cheap; the cost is
    per-item insertion + lookup, both O(num_perm).
    """

    threshold: float = 0.85
    num_perm: int = 128
    shingle_size: int = 3

    def __post_init__(self) -> None:
        if not 0.0 < self.threshold <= 1.0:
            raise ValueError(f"threshold must be in (0, 1], got {self.threshold}")
        if self.shingle_size < 1:
            raise ValueError(f"shingle_size must be >= 1, got {self.shingle_size}")
        self._lsh = MinHashLSH(threshold=self.threshold, num_perm=self.num_perm)

    def is_duplicate(self, text: str) -> tuple[bool, list[str]]:
        """Returns ``(duplicate, existing_ids)``. ``existing_ids`` is the LSH neighbour set
        — callers can use it for audit trails (which earlier chunk did this one duplicate)."""

        mh = self._fingerprint(text)
        if mh is None:
            return False, []
        neighbours = list(self._lsh.query(mh))
        return bool(neighbours), neighbours

    def add(self, text: str, item_id: str) -> bool:
        """Insert ``item_id`` for future dedup queries. Returns False when the text is
        too short to fingerprint (and so nothing was added)."""

        mh = self._fingerprint(text)
        if mh is None:
            return False
        # MinHashLSH refuses duplicate keys; skip silently in case the caller re-adds.
        if item_id in self._lsh:
            return False
        self._lsh.insert(item_id, mh)
        return True

    def add_unless_duplicate(self, text: str, item_id: str) -> tuple[bool, list[str]]:
        """One-shot helper: check + insert in a single call. Returns the same shape as
        ``is_duplicate``; when not a dup, ``item_id`` has been added to the index."""

        is_dup, neighbours = self.is_duplicate(text)
        if not is_dup:
            self.add(text, item_id)
        return is_dup, neighbours

    def _fingerprint(self, text: str) -> MinHash | None:
        tokens = _WHITESPACE.split(text.strip().lower())
        if len(tokens) < self.shingle_size:
            return None
        mh = MinHash(num_perm=self.num_perm)
        for i in range(len(tokens) - self.shingle_size + 1):
            shingle = " ".join(tokens[i : i + self.shingle_size])
            mh.update(shingle.encode("utf-8"))
        return mh
