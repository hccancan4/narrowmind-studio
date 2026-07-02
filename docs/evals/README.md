# Eval artifacts — primary evidence

`projects/` is not tracked (it holds gigabytes of per-user corpora, vector
stores and GGUFs), so the eval reports the docs cite would otherwise be
unreachable from a clone. This folder mirrors the **headline** reports
verbatim from their per-project `evals/` directories; the filename is the
eval run id.

## Shrink-search frontier (Phase 4.8 — `felsefe-sep`, 270-pair grounded eval)

| File | Arm | Judge mean | Note |
|---|---|---:|---|
| [9323d65b…](9323d65ba16a4a7f9d6d8856b3f2a535-hybrid.md) | **3B fine-tune + RAG** | **3.80** | the shipped DSLM size |
| [227ff95c…](227ff95cc6e740e3aa96d7ec38e1b81c-hybrid.md) | 3B base + RAG | 3.65 | ⚠ only 40/270 pairs judged (API errors); full re-run queued |
| [56b86c8f…](56b86c8f03224e288e9053da8dbdb42f-hybrid.md) | 1.5B fine-tune + RAG | 3.29 | 269 judged (one pair unjudged) |
| [e3cced43…](e3cced439bc647279ad4bdff5bb5e40a-hybrid.md) | 1.5B base + RAG | 3.64 | |
| [6f0238ef…](6f0238efac8740398bdda0bfbbf31864-hybrid.md) | 0.5B fine-tune + RAG | 2.18 | distribution inverted — collapse |
| [a6a7e6d3…](a6a7e6d363124fedbde4448a7d870cd8-hybrid.md) | 0.5B base + RAG | 2.72 | RAG floor gives out below 1.5B |

Interpretation: `docs/shrink-search-report.md`.

## Earlier milestones

| File | What | Result |
|---|---|---|
| [eb700905…](eb700905313a450f9be060a394550df4-hybrid.md) | Phase 4.7 raw-import 7B fine-tune (`felsefe-sep`, 300-pair eval) | judge **2.00** — the data-quality failure that motivated Phase 4.8 |
| [2026-05-18-multiconfig.md](2026-05-18-multiconfig.md) | Phase 3.5 dense/sparse/hybrid comparison (`deneme1-faz2`, 56-pair eval) | hybrid: recall **0.98** / judge **4.55** |
