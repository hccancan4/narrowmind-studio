from narrowmind_workers.cleaning.minhash import MinHashDedup


def test_exact_duplicate_detected() -> None:
    dedup = MinHashDedup(threshold=0.85)
    text = "The hard problem of consciousness asks why physical processes give rise to subjective experience."
    assert dedup.add(text, "a")
    is_dup, neighbours = dedup.is_duplicate(text)
    assert is_dup
    assert neighbours == ["a"]


def test_distinct_texts_not_flagged() -> None:
    dedup = MinHashDedup(threshold=0.85)
    a = "The hard problem of consciousness asks why physical processes give rise to subjective experience."
    b = "Functionalism holds that mental states are defined by their causal role rather than their material substrate."
    assert dedup.add(a, "a")
    is_dup, _ = dedup.is_duplicate(b)
    assert not is_dup


def test_near_duplicate_detected_above_threshold() -> None:
    # Two long passages differing by a single inline edit. Their 3-gram word shingles
    # overlap almost perfectly so the Jaccard similarity sits well above 0.85.
    dedup = MinHashDedup(threshold=0.85)
    original = (
        "The hard problem of consciousness asks why physical processes give rise to subjective experience. "
        "Coined by David Chalmers in 1995, the term distinguishes the explanatory gap from easier problems. "
        "Easier problems include attention, memory, and reportability. The literature on this topic has grown "
        "extensively over the past three decades, with functionalists, dualists, and panpsychists each "
        "proposing distinct frameworks for understanding the phenomenal character of mind."
    )
    near_dup = original.replace("physical processes", "neural processes")
    assert dedup.add(original, "orig")
    is_dup, _ = dedup.is_duplicate(near_dup)
    assert is_dup


def test_low_jaccard_text_passes_strict_threshold() -> None:
    dedup = MinHashDedup(threshold=0.95)
    a = "The hard problem of consciousness asks why physical processes give rise to subjective experience."
    b = "Easier problems of consciousness include attention, memory, and reportability — but they're tractable."
    assert dedup.add(a, "a")
    is_dup, _ = dedup.is_duplicate(b)
    assert not is_dup


def test_too_short_text_skipped() -> None:
    dedup = MinHashDedup()
    # Below shingle window — nothing to fingerprint
    added = dedup.add("hi", "x")
    assert not added


def test_add_unless_duplicate_inserts_on_miss() -> None:
    dedup = MinHashDedup(threshold=0.85)
    a = "The hard problem of consciousness asks why physical processes give rise to subjective experience."
    b = "Functionalism holds that mental states are defined by causal role rather than substrate."
    is_dup_a, _ = dedup.add_unless_duplicate(a, "a")
    is_dup_b, _ = dedup.add_unless_duplicate(b, "b")
    assert not is_dup_a
    assert not is_dup_b
    # Adding `a` again should now flag it as a duplicate
    is_dup_again, neighbours = dedup.add_unless_duplicate(a, "a-again")
    assert is_dup_again
    assert "a" in neighbours
