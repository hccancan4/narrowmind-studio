"""Phase 3.5 — assemble the side-by-side comparison report from three
run_eval markdown outputs (dense / sparse / hybrid).

Each input report has a "## Per-pair" table with rows like:
  | 1 | ✓ | 4 | What is the title of ... |

We parse those, align by question index, and emit one combined markdown
that shows the per-pair score matrix plus an improvement breakdown
(which Phase 3 baseline misses got fixed in hybrid, what the residual
failure modes look like).
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


PAIR_RE = re.compile(r"^\|\s*(\d+)\s*\|\s*([✓✗\?])\s*\|\s*(\d|-)\s*\|\s*(.+?)\s*\|\s*$")
AGG_RE = re.compile(r"\|\s*retrieval recall@k\s*\|\s*\*\*([\d.]+)\*\*\s*\|")
JUDGE_RE = re.compile(r"\|\s*LLM-judge mean\s*\|\s*\*\*([\d.]+)\s*/\s*5\*\*\s*\|")
DIST_RE = re.compile(r"\|\s*judge score = (\d)\s*\|\s*(\d+) pairs\s*\|")


def parse_report(path: Path) -> dict:
    pairs: list[dict] = []
    recall = judge_mean = None
    distribution: dict[int, int] = {}
    in_per_pair = False
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.startswith("## Per-pair"):
            in_per_pair = True
            continue
        if line.startswith("## Detail"):
            in_per_pair = False
            continue
        if in_per_pair:
            m = PAIR_RE.match(line)
            if m:
                pairs.append({
                    "idx": int(m.group(1)),
                    "recall": m.group(2) == "✓",
                    "score": None if m.group(3) == "-" else int(m.group(3)),
                    "question": m.group(4),
                })
        if recall is None:
            mr = AGG_RE.search(line)
            if mr:
                recall = float(mr.group(1))
        if judge_mean is None:
            mj = JUDGE_RE.search(line)
            if mj:
                judge_mean = float(mj.group(1))
        md = DIST_RE.search(line)
        if md:
            distribution[int(md.group(1))] = int(md.group(2))
    return {
        "path": path,
        "recall": recall,
        "judge_mean": judge_mean,
        "distribution": distribution,
        "pairs": pairs,
    }


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--dense", required=True, type=Path)
    p.add_argument("--sparse", required=True, type=Path)
    p.add_argument("--hybrid", required=True, type=Path)
    p.add_argument("--out", required=True, type=Path)
    p.add_argument("--baseline-19pair-recall", type=float, default=0.79,
                   help="recall@5 from the pre-3.5 19-pair baseline")
    p.add_argument("--baseline-19pair-judge", type=float, default=3.37,
                   help="judge mean from the pre-3.5 19-pair baseline")
    args = p.parse_args()

    dense = parse_report(args.dense)
    sparse = parse_report(args.sparse)
    hybrid = parse_report(args.hybrid)

    # Align pairs by idx — all three runs evaluated the same 56 pairs in the
    # same order (the agent passes only `mode`, all other params stay default).
    by_idx: dict[int, dict] = {}
    for run_name, run in [("dense", dense), ("sparse", sparse), ("hybrid", hybrid)]:
        for pr in run["pairs"]:
            row = by_idx.setdefault(pr["idx"], {"idx": pr["idx"], "question": pr["question"]})
            row[f"{run_name}_recall"] = pr["recall"]
            row[f"{run_name}_score"] = pr["score"]

    s: list[str] = []
    s.append("# NarrowMind RAG — multi-config eval (Phase 3.5)\n")
    s.append("Side-by-side comparison of three retrieval modes against the same\n"
             "56-pair eval set (46 random + 10 proper-noun-targeted). Same Qwen2.5-7B\n"
             "GGUF, same top_k=5, same Sonnet judge.\n")
    s.append("")
    s.append("## Aggregate\n")
    s.append("| metric | dense | sparse | hybrid | Phase 3 baseline (19 pairs) |")
    s.append("|---|---:|---:|---:|---:|")
    s.append(f"| retrieval recall@5  | {dense['recall']:.2f} | {sparse['recall']:.2f} | **{hybrid['recall']:.2f}** | {args.baseline_19pair_recall:.2f} |")
    s.append(f"| LLM-judge mean      | {dense['judge_mean']:.2f} | {sparse['judge_mean']:.2f} | **{hybrid['judge_mean']:.2f}** | {args.baseline_19pair_judge:.2f} |")
    for k in [5, 4, 3, 2, 1]:
        s.append(
            f"| judge score = {k}    | {dense['distribution'].get(k, 0)} | "
            f"{sparse['distribution'].get(k, 0)} | "
            f"{hybrid['distribution'].get(k, 0)} | — |"
        )
    s.append("")

    # Phase 4 gating thresholds from the consolidation prompt
    s.append("## Phase 4 gating thresholds\n")
    s.append("| threshold | required | hybrid actual | verdict |")
    s.append("|---|---|---|---|")
    recall_ok = hybrid["recall"] >= 0.85
    judge_ok = hybrid["judge_mean"] >= 3.8
    s.append(f"| recall@5 >= 0.85       | 0.85 | {hybrid['recall']:.2f} | {'PASS' if recall_ok else 'FAIL'} |")
    s.append(f"| judge mean >= 3.8     | 3.80 | {hybrid['judge_mean']:.2f} | {'PASS' if judge_ok else 'FAIL'} |")
    s.append("")
    if recall_ok and judge_ok:
        s.append("**Both thresholds passed -> Phase 4 (LoRA fine-tune) is unblocked.**\n")
    else:
        s.append("**At least one threshold failed -> re-chunking discussion before Phase 4.**\n")
    s.append("")

    # Improvement table — counts pairs where hybrid > dense, etc.
    flipped_to_recall = sum(1 for r in by_idx.values()
                            if not r.get("dense_recall") and r.get("hybrid_recall"))
    flipped_lost_recall = sum(1 for r in by_idx.values()
                              if r.get("dense_recall") and not r.get("hybrid_recall"))
    judge_up = sum(1 for r in by_idx.values()
                   if (r.get("hybrid_score") or 0) > (r.get("dense_score") or 0))
    judge_down = sum(1 for r in by_idx.values()
                     if (r.get("hybrid_score") or 0) < (r.get("dense_score") or 0))
    s.append("## Dense -> Hybrid delta\n")
    s.append("| change | pairs |")
    s.append("|---|---:|")
    s.append(f"| recall flipped MISS -> HIT in hybrid | {flipped_to_recall} |")
    s.append(f"| recall flipped HIT -> MISS in hybrid | {flipped_lost_recall} |")
    s.append(f"| judge score increased in hybrid     | {judge_up} |")
    s.append(f"| judge score decreased in hybrid     | {judge_down} |")
    s.append("")

    s.append("## Per-pair score matrix\n")
    s.append("Recall ✓/✗ then judge score (1-5). Cells where hybrid beats dense are bolded.\n")
    s.append("")
    s.append("| # | dense | sparse | hybrid | question |")
    s.append("|---:|:---:|:---:|:---:|---|")
    for idx in sorted(by_idx.keys()):
        r = by_idx[idx]
        def cell(prefix: str) -> str:
            rec = r.get(f"{prefix}_recall")
            sc = r.get(f"{prefix}_score")
            rec_mark = "✓" if rec else "✗"
            sc_str = "-" if sc is None else str(sc)
            return f"{rec_mark}{sc_str}"
        d_cell = cell("dense")
        sp_cell = cell("sparse")
        hy_cell = cell("hybrid")
        d_score = r.get("dense_score") or 0
        h_score = r.get("hybrid_score") or 0
        if (h_score > d_score) or (not r.get("dense_recall") and r.get("hybrid_recall")):
            hy_cell = f"**{hy_cell}**"
        q = r["question"][:72]
        s.append(f"| {idx} | {d_cell} | {sp_cell} | {hy_cell} | {q} |")
    s.append("")

    # Residual failure narrative — pairs hybrid still gets wrong (judge <= 2)
    residuals = [r for r in by_idx.values() if (r.get("hybrid_score") or 5) <= 2]
    s.append("## Residual hybrid failures (judge <= 2)\n")
    if not residuals:
        s.append("None. Every pair scored 3+ under hybrid retrieval.\n")
    else:
        s.append("| # | question | hybrid recall | hybrid judge |")
        s.append("|---:|---|:---:|:---:|")
        for r in sorted(residuals, key=lambda x: x["idx"]):
            rec_mark = "✓" if r.get("hybrid_recall") else "✗"
            sc = r.get("hybrid_score")
            s.append(f"| {r['idx']} | {r['question'][:80]} | {rec_mark} | {sc} |")
    s.append("")

    s.append("---\n")
    s.append(f"- dense report:  `{dense['path'].name}`")
    s.append(f"- sparse report: `{sparse['path'].name}`")
    s.append(f"- hybrid report: `{hybrid['path'].name}`")

    args.out.write_text("\n".join(s), encoding="utf-8")
    print(f"wrote {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
