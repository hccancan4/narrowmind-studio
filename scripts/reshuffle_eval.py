"""Phase 3.5 — reshuffle sft.jsonl + eval.jsonl with a fresh seed and a 25 %
eval split.

Why: the Phase 2 split used a 90/10 ratio (167 sft + 19 eval). 19 eval pairs
is too small to draw conclusions about hybrid retrieval improvements with any
confidence — single-pair flips swing recall by 5 percentage points. Bumping
to 25 % eval gives ~46 pairs, which puts confidence intervals on numbers we
actually care about (recall ± ~7 %, judge mean ± ~0.3 at 95 % CI).

This script is a one-shot Phase 3.5 migration. Run it once on the target
project; afterwards the new sft.jsonl + eval.jsonl + project.toml [synth]
split_seed are the persistent baseline. Re-running with the same seed
reproduces the same split, so this is idempotent.

Usage:
  uv run python scripts/reshuffle_eval.py \
      --project-dir "$APPDATA/narrowmind/projects/deneme1-faz2" \
      --eval-frac 0.25 \
      --seed 7340118592873561  # any u64
"""
from __future__ import annotations

import argparse
import json
import random
import sys
from pathlib import Path


def read_jsonl(path: Path) -> list[dict]:
    out: list[dict] = []
    if not path.is_file():
        return out
    with path.open("r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            out.append(json.loads(line))
    return out


def write_jsonl(path: Path, rows: list[dict]) -> None:
    # Atomic rewrite: write to .tmp first, then os.replace into place so a
    # crash mid-write never leaves a half-merged jsonl.
    tmp = path.with_suffix(path.suffix + ".tmp")
    with tmp.open("w", encoding="utf-8") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False))
            f.write("\n")
    tmp.replace(path)


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--project-dir", required=True, type=Path)
    p.add_argument("--eval-frac", type=float, default=0.25)
    p.add_argument("--seed", type=int, required=True, help="u64 seed for shuffling")
    args = p.parse_args()

    datasets = args.project_dir / "datasets"
    sft_path = datasets / "sft.jsonl"
    eval_path = datasets / "eval.jsonl"

    sft = read_jsonl(sft_path)
    eval_ = read_jsonl(eval_path)
    if not sft and not eval_:
        print(f"no pairs found in {sft_path} / {eval_path}", file=sys.stderr)
        return 1

    pool = sft + eval_
    rng = random.Random(args.seed)
    rng.shuffle(pool)

    n_eval = max(1, int(round(len(pool) * args.eval_frac)))
    new_eval = pool[:n_eval]
    new_sft = pool[n_eval:]

    write_jsonl(eval_path, new_eval)
    write_jsonl(sft_path, new_sft)

    print(f"reshuffle done: {len(pool)} total -> {len(new_sft)} sft + {len(new_eval)} eval")
    print(f"eval fraction: {len(new_eval) / len(pool):.1%}")
    print(f"seed: {args.seed}")
    print(f"  sft:  {sft_path}")
    print(f"  eval: {eval_path}")
    print("Remember to update project.toml [synth] split_seed to record this split.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
