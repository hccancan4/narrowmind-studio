"""Language identification via `lingua-language-detector`.

The first call builds a detector across all languages — that's a ~1-2 s cost and ~100 MB
of accuracy models. After that, ``detect_language_of`` on a chunk is microseconds. We
memoise the detector with `lru_cache` so the cost is paid exactly once per worker process.
"""

from __future__ import annotations

import logging
from functools import lru_cache

from lingua import LanguageDetectorBuilder

log = logging.getLogger(__name__)

# How many characters of text the detector needs to make a confident call. Below this,
# we return None rather than guessing — short fragments are too noisy.
MIN_TEXT_CHARS = 50


@lru_cache(maxsize=1)
def _detector():
    log.info("building lingua detector across all languages (first-call cost ~1-2s)…")
    return LanguageDetectorBuilder.from_all_languages().build()


def detect_language(text: str) -> str | None:
    """Best-effort ISO 639-1 code (e.g. ``"en"``, ``"tr"``) or `None` if undecidable."""
    s = text.strip()
    if len(s) < MIN_TEXT_CHARS:
        return None
    lang = _detector().detect_language_of(s)
    if lang is None:
        return None
    code = lang.iso_code_639_1
    if code is None:
        return None
    return code.name.lower()


def is_english(text: str) -> bool:
    return detect_language(text) == "en"


def is_language(text: str, expected: str) -> bool:
    expected = expected.strip().lower()
    return detect_language(text) == expected
