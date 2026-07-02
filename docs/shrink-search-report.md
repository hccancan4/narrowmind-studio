# Shrink-Search Report — Smallest-Viable DSLM (Phase 4.8)

Why this document exists: Phase 4.8 asked one question — *what is the smallest
base model that still clears the domain quality bar?* This is the experiment
record: method, the full size-vs-quality frontier, what the numbers say, and
the operational lessons that came out of running it end-to-end on the
reference card (RTX 3070 8 GB, 32 GB RAM). The roadmap entry is
`docs/ROADMAP.md → Phase 4.8`; per-run baselines live in `docs/testing.md`.

---

## Method

Everything varies **only** by base-model size. One project (`felsefe-sep`),
one corpus, one dataset, one eval:

- **Corpus**: 14,537 SEP chunks (`datasets/rag.jsonl`) + hybrid retrieval
  (dense BGE-small + BM25, RRF fusion, top-k 5).
- **SFT data**: 2,700 grounded synthetic pairs (Haiku `synth_model`,
  completeness-tuned prompt — see the data lever below), split 2,430 train /
  270 held-out eval. Generated **once**, reused for every size.
- **Training**: QLoRA via Unsloth in WSL2, `rtx-3070-8gb` preset unchanged
  (batch 1, accum 8, seq 2048, lr 2e-4, 3 epochs, bf16, adamw_8bit) — only
  `base_model_id` changes per run.
- **Serving**: merge → GGUF `Q4_K_M` → `llama_cpp.server` (KV cache `q8_0`,
  flash-attn, `n_ctx` 4096).
- **Eval**: `run_eval` over all 270 pairs — recall@5 against the tagged source
  chunk + 1–5 LLM-judge (claude-sonnet-4-6) against the gold answer.
- **Reference arm**: each size also evaluated as **base+RAG** (same GGUF
  family, no fine-tune) so the fine-tune's contribution is isolated.

## The data lever (why any of this scored above 2)

The first `felsefe-sep` attempt imported a ready-made HF QA set whose
"answers" were raw SEP passages loosely aligned to auto-generated questions.
Result: **judge 2.00/5, 31 % fabrication, recall unmeasurable** — the model
learned the *style* of encyclopedia prose, not the knowledge.

Regenerating SFT targets from our own chunks with a completeness-tuned prompt
(complete + accurate + self-contained, synthesized not copied, no
fabrication) lifted the same 3B pipeline to **3.80** — a **+1.80** jump that
dwarfs every model-size effect measured below. Data quality beats parameter
count; that is the single most transferable finding of the phase.

## The frontier

All rows: 270 grounded eval pairs, hybrid retrieval, recall@5 = **0.98**
throughout (retrieval is corpus-driven, not model-driven — as designed).

| Base (Qwen2.5-Instruct) | Fine-tune + RAG | Base + RAG | Δ fine-tune | GGUF size | Verdict |
|---|---:|---:|---:|---:|---|
| 7B (Phase 3 dogfood corpus)¹ | — | 4.55 | — | 4.7 GB | reference ceiling |
| **3B** | **3.80** | 3.65 | **+0.15** | 1.8 GB | **smallest viable** |
| 1.5B | 3.29 | 3.64 | **−0.35** | 940 MB | fine-tune *hurts* |
| 0.5B | 2.18 | 2.72 | **−0.54** | 379 MB | both arms collapse — floor found |

¹ different corpus and eval set (56 pairs, Phase 3) — a ceiling reference, not
a same-data comparison.

Score distributions (fine-tune arm, scores 1→5): 3B = 4/25/62/108/71;
1.5B = 10/47/95/88/29; 0.5B = 72/107/66/21/4. The trend is monotone and
accelerating — the 1.5B fine-tune doubles the score-1/2 tail, and the 0.5B
fine-tune inverts the distribution entirely (two-thirds of answers at 1–2).
Small models lose general instruction-following faster than the domain data
adds knowledge, and the loss compounds as parameters shrink.

**Reading the frontier:**

- **3B is the smallest size where fine-tuning still pays.** +0.15 over its
  base, and its base already trails the fine-tune of nothing else at that
  budget. The shipped DSLM for this corpus is the 3B fine-tune (1.8 GB, fits
  the 8 GB card with room for KV + embedder).
- **Below 3B, RAG carries the quality and the fine-tune subtracts.** The
  1.5B base+RAG (3.64) is statistically the 3B base+RAG (3.65) — but
  fine-tuning it *costs* 0.35. On small models, 3 epochs of narrow-domain
  LoRA erodes the instruct behaviors the RAG prompt depends on, and the
  penalty grows monotonically as parameters shrink: +0.15 → −0.35 → −0.54.
- **The RAG-carried floor itself gives out below 1.5B.** Base+RAG holds
  ~3.65 from 3B down to 1.5B, then collapses to 2.72 at 0.5B (base
  distribution 37/87/81/45/20) — at that size the model can no longer
  reliably read the retrieved chunks into a coherent answer, retrieval
  quality notwithstanding (recall stayed 0.98).
- Corollary: on a tight budget, **1.5B base + good RAG ≈ 3B base + good
  RAG** — if you can't afford to fine-tune well, don't fine-tune at all.
  But don't go below 1.5B: no amount of retrieval rescues a 0.5B reader.

## Operational lessons (8 GB card, 32 GB host)

The 0.5B leg crashed the host twice before completing; both failures were
**host-RAM commit pressure**, not VRAM:

1. `llama-cpp-python` allocates an `(n_ctx × n_vocab)` f32 logits buffer at
   load — Qwen's 151,936-token vocab makes that **2.3 GB host RAM** at
   `n_ctx` 4096 regardless of model size. Orphaned servers from failed
   retries accumulate exactly this.
2. `run_eval parallelism=4` + the RAM prompt cache produced a worker
   die→respawn spiral under pressure (each respawn reloads torch+BGE), which
   outran pagefile expansion → `STATUS_COMMITMENT_LIMIT` → app webview death.

Guardrails now standard for eval runs: **`parallelism=2`**, serve with
**`prompt_cache=false`** (270 unique prompts get no reuse from it; it only
buys commit), and check for orphaned `llama_cpp.server` processes before any
serve. A post-OOM zombie app process can block *all* process enumeration —
`taskkill /F /T /IM narrowmind-desktop.exe` clears it without a reboot.

## Forward paths

Ranked by expected return:

1. **Big-corpus round** (`docs/ROADMAP.md → Phase 5`): merge HF philosophy
   sets + public-domain books + SEP/IEP into one large corpus; retrain the
   smallest-viable size on it. Add the shared `corpora/<name>/` library +
   `import_dataset` tool so the expensive prepared corpus (embedding hours +
   synth $) is built once and reused across projects.
2. **Qwen3 ladder** (0.6B / 1.7B / 4B-Instruct-2507): near-drop-in upgrade
   (arch `qwen3` is already inside the pinned llama.cpp; official Unsloth
   bnb-4bit + GGUFs exist; Apache-2.0). Qwen3-1.7B is the natural candidate
   to retest the "fine-tuning hurts below 3B" boundary — a generation-newer
   base may move it. **Qwen3.5 stays skipped**: multimodal `qwen3_5` arch, no
   official 4-bit training checkpoints, would force a rebuild across both
   Python envs.
3. **Codebase cleanup** (audit already done, findings queued): unused
   `tokenizers` dep, dead `_DUR_MARKER`, 3-way chunk-loading duplication,
   stale Phase 3.5 scripts. Apply when no training/serve is live (Rust edits
   trigger a rebuild that would orphan them).
4. **Sub-1B floor probe** (optional): SmolLM2-360M has a philosophy fine-tune
   precedent on HF (Llama arch = zero migration) if the 0.5B result argues
   for probing lower.
