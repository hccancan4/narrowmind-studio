"""Document-level quality heuristics.

Cheap reasons to drop a document before paying for chunking + embedding cost:

- way too short (likely scraping failure)
- way too long (likely PDF dump of a textbook — process separately)
- low letter ratio (likely table dump, code listing, ASCII art, or garbled encoding)
- runaway newlines (broken layout, OCR error)

Returns ``(ok: bool, reason: str)`` so the caller can record *why* a doc was dropped —
silent filters are debugging hell.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class QualityLimits:
    min_chars: int = 200
    max_chars: int = 2_000_000  # ~500k tokens — books fit, but datasets don't
    min_letter_ratio: float = 0.5
    max_newline_ratio: float = 0.15


DEFAULT_LIMITS = QualityLimits()


def passes_quality(text: str, limits: QualityLimits = DEFAULT_LIMITS) -> tuple[bool, str]:
    n = len(text)
    if n < limits.min_chars:
        return False, f"too short ({n} chars < {limits.min_chars})"
    if n > limits.max_chars:
        return False, f"too long ({n} chars > {limits.max_chars})"

    letters = sum(1 for c in text if c.isalpha())
    letter_ratio = letters / n
    if letter_ratio < limits.min_letter_ratio:
        return False, f"low letter ratio ({letter_ratio:.2f} < {limits.min_letter_ratio})"

    newlines = text.count("\n")
    newline_ratio = newlines / n
    if newline_ratio > limits.max_newline_ratio:
        return False, f"too many newlines ({newline_ratio:.2f} > {limits.max_newline_ratio})"

    return True, "ok"
