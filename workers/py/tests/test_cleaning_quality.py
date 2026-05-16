from narrowmind_workers.cleaning.quality import QualityLimits, passes_quality


GOOD_PROSE = (
    "This is a reasonably long paragraph of prose that should pass every quality check. "
    "It has plenty of letters, the right kind of punctuation, and only a few newlines. "
    "The cleaner should accept it without complaining."
) * 3


def test_passes_good_prose() -> None:
    ok, reason = passes_quality(GOOD_PROSE)
    assert ok, reason


def test_rejects_too_short() -> None:
    ok, reason = passes_quality("hi")
    assert not ok
    assert "too short" in reason


def test_rejects_low_letter_ratio() -> None:
    # Mostly digits and punctuation
    junk = "1234567890 !@#$%^&*() " * 50
    ok, reason = passes_quality(junk)
    assert not ok
    assert "letter ratio" in reason


def test_rejects_too_many_newlines() -> None:
    nl = "\n".join(["a" for _ in range(500)])
    ok, reason = passes_quality(nl)
    assert not ok
    assert "newlines" in reason


def test_custom_limits() -> None:
    short = "a" * 50
    # Default rejects (too short) — custom limit accepts
    assert not passes_quality(short)[0]
    assert passes_quality(short, QualityLimits(min_chars=10, min_letter_ratio=0.9))[0]
