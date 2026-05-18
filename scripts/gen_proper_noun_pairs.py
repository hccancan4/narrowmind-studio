"""Phase 3.5 — generate proper-noun-heavy eval pairs from the project's
chunks.jsonl files using a cheap LLM.

Rationale: the Phase 3 baseline-19pair eval had four recall failures
(Doleantie / Iamblichus / Eureqa / Guerizoli 2006) where the gold chunk
existed in the corpus but BGE-small couldn't surface it. We need MORE of
those — specifically proper-noun-anchored questions that hybrid retrieval
should ace — to validate that the BM25 sidecar actually closes the gap
on the eval set as a whole.

Pipeline:
  1. Walk every sources/<id>/chunks.jsonl, score each chunk by
     proper-noun density (uppercase tokens that aren't sentence-initial,
     names with apostrophes / "von" / "de la" markers, years).
  2. Keep the top-N densest chunks (N = 3x the requested pair count so
     the LLM has variety to pick the most interesting ones).
  3. For each surviving chunk, ask the LLM to produce ONE question +
     ground-truth answer + source_chunk_id, where the question is
     answerable ONLY from that chunk (no general knowledge), and the
     answer cites the proper noun specifically.
  4. Append the resulting pairs to datasets/eval.jsonl (append, not
     overwrite — caller decides the merge order).

Usage:
  export ANTHROPIC_API_KEY=sk-ant-...
  uv run python scripts/gen_proper_noun_pairs.py \
      --project-dir "$APPDATA/narrowmind/projects/deneme1-faz2" \
      --n 10 \
      --model claude-haiku-4-5-20251001
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sys
from pathlib import Path

import anthropic

# Regexes that act as proxies for "this chunk is proper-noun heavy".
# We don't need precision here — false positives just give the LLM more
# candidate material than it needs.
PROPER_NOUN_PATTERNS = [
    re.compile(r"\b[A-Z][a-z]+(?:\s+[A-Z][a-z]+){1,3}\b"),  # "Abraham Kuyper", "Vernor Vinge"
    re.compile(r"\b[A-Z][a-z]+(?:'s|'s)?\b"),  # any capitalised word
    re.compile(r"\b\d{4}\b"),                   # years (1883, 2009, 2006)
    re.compile(r"\b(?:von|de|de la|du|della|van)\s+[A-Z][a-z]+\b"),  # european name particles
]


def score_proper_noun_density(text: str) -> float:
    """Higher = denser proper-noun content. Sum of pattern hits normalised
    by word count, with a small bonus for chunks that pack multiple distinct
    proper-noun-like tokens."""
    words = text.split()
    if len(words) < 30:
        return 0.0
    hits = 0
    distinct: set[str] = set()
    for pat in PROPER_NOUN_PATTERNS:
        for m in pat.findall(text):
            hits += 1
            distinct.add(m if isinstance(m, str) else m[0])
    density = hits / max(len(words), 1)
    # Reward variety — a chunk with one name repeated 10 times shouldn't
    # outrank a chunk with 5 distinct names.
    variety_bonus = min(len(distinct), 10) / 10.0
    return density * (1.0 + variety_bonus)


def gather_chunks(project_dir: Path) -> list[dict]:
    """Walk every sources/<id>/chunks.jsonl, return all included chunks."""
    out: list[dict] = []
    sources_dir = project_dir / "sources"
    if not sources_dir.is_dir():
        return out
    for entry in sorted(sources_dir.iterdir()):
        if not entry.is_dir():
            continue
        chunks_path = entry / "chunks.jsonl"
        if not chunks_path.is_file():
            continue
        with chunks_path.open("r", encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    rec = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if rec.get("include", True):
                    out.append(rec)
    return out


PROMPT_TEMPLATE = """You are helping build an EVAL set for a retrieval-augmented
philosophy DSLM. We have a chunk of source text below. Your job:

Produce ONE question + ground-truth answer pair where:
1. The question is anchored on a SPECIFIC PROPER NOUN from the chunk (a
   philosopher's name, a movement, a book title, a year + event, a place,
   a school of thought). Not "what does the text say about consciousness"
   — that's too generic. Ask "Who coined X?" or "What was the Doleantie?"
   or "When did Vinge predict the Singularity?".
2. The answer is grounded ONLY in this chunk. Do not invent details from
   general knowledge. If the chunk doesn't contain enough for a sharp
   factual answer, return an empty answer string and we'll skip this pair.
3. The answer should be 1-3 sentences. Concrete and citation-shaped.

Return STRICT JSON, single object, no code fences, no commentary:
{{"question": "...", "answer": "...", "source_chunk_id": "{chunk_id}"}}

If the chunk has nothing proper-noun-worthy, return:
{{"question": "", "answer": "", "source_chunk_id": "{chunk_id}"}}

CHUNK:
{chunk_text}
"""


def llm_generate_pair(client: anthropic.Anthropic, model: str, chunk: dict) -> dict | None:
    prompt = PROMPT_TEMPLATE.format(chunk_id=chunk["chunk_id"], chunk_text=chunk["text"])
    try:
        resp = client.messages.create(
            model=model,
            max_tokens=512,
            messages=[{"role": "user", "content": prompt}],
        )
    except Exception as e:
        print(f"  LLM error on {chunk['chunk_id']}: {e}", file=sys.stderr)
        return None
    text = "".join(b.text for b in resp.content if getattr(b, "type", None) == "text").strip()
    # Strip ``` fences if the model added them despite instructions.
    if text.startswith("```"):
        text = re.sub(r"^```[a-z]*\n?|\n?```$", "", text, flags=re.MULTILINE).strip()
    try:
        obj = json.loads(text)
    except json.JSONDecodeError as e:
        print(f"  parse error on {chunk['chunk_id']}: {e}\n  raw: {text[:200]}", file=sys.stderr)
        return None
    if not obj.get("question") or not obj.get("answer"):
        return None
    obj["source_chunk_id"] = chunk["chunk_id"]
    return obj


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--project-dir", required=True, type=Path)
    p.add_argument("--n", type=int, default=10, help="number of pairs to produce")
    p.add_argument("--model", default="claude-haiku-4-5-20251001")
    p.add_argument("--candidate-multiplier", type=int, default=4,
                   help="how many candidate chunks per requested pair (LLM may reject some)")
    p.add_argument("--show-first", type=int, default=3,
                   help="print the first N produced pairs to stdout so the caller can sanity-check")
    p.add_argument("--out", type=Path, default=None,
                   help="output file; default is datasets/eval-proper-noun.jsonl in the project dir")
    args = p.parse_args()

    if not os.environ.get("ANTHROPIC_API_KEY"):
        print("set ANTHROPIC_API_KEY", file=sys.stderr)
        return 1

    chunks = gather_chunks(args.project_dir)
    if not chunks:
        print(f"no chunks under {args.project_dir}/sources", file=sys.stderr)
        return 1
    print(f"scored {len(chunks)} chunks for proper-noun density")

    scored = sorted(chunks, key=lambda c: score_proper_noun_density(c["text"]), reverse=True)
    candidate_count = args.n * args.candidate_multiplier
    candidates = scored[:candidate_count]
    print(f"top {candidate_count} candidates by density; producing {args.n} pairs via {args.model}")

    client = anthropic.Anthropic()
    pairs: list[dict] = []
    for c in candidates:
        if len(pairs) >= args.n:
            break
        pair = llm_generate_pair(client, args.model, c)
        if pair is None:
            continue
        pairs.append(pair)
        print(f"  +{len(pairs)}/{args.n}  {pair['source_chunk_id']}  {pair['question'][:70]}")

    if not pairs:
        print("no pairs produced", file=sys.stderr)
        return 1

    out_path = args.out or (args.project_dir / "datasets" / "eval-proper-noun.jsonl")
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with out_path.open("w", encoding="utf-8") as f:
        for r in pairs:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    print(f"wrote {len(pairs)} pairs -> {out_path}")

    print()
    print("=== first sanity-check sample ===")
    for r in pairs[: args.show_first]:
        print(json.dumps(r, ensure_ascii=False, indent=2))
        print()

    return 0


if __name__ == "__main__":
    sys.exit(main())
