"""Language detection tests. The first call builds the detector across all languages
(~1-2s, ~100MB models loaded once via lru_cache). pytest runs them serially so the
total per-suite overhead is amortised."""

from narrowmind_workers.cleaning.language import detect_language, is_english, is_language


ENGLISH = (
    "The hard problem of consciousness asks why physical processes give rise to "
    "subjective experience. Coined by David Chalmers in 1995, the term distinguishes "
    "the explanatory gap from easier problems."
)

TURKISH = (
    "Bilinç'in zor problemi, fiziksel süreçlerin neden öznel deneyime yol açtığını "
    "sorar. David Chalmers tarafından 1995'te ortaya atılan bu terim, açıklayıcı "
    "boşluğu daha kolay problemlerden ayırır."
)


def test_english_detected() -> None:
    assert detect_language(ENGLISH) == "en"
    assert is_english(ENGLISH)


def test_turkish_detected() -> None:
    assert detect_language(TURKISH) == "tr"
    assert not is_english(TURKISH)
    assert is_language(TURKISH, "tr")


def test_very_short_text_returns_none() -> None:
    assert detect_language("hi") is None
    assert not is_english("hi")


def test_whitespace_returns_none() -> None:
    assert detect_language("   \n\n  ") is None
