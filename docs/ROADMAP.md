# NarrowMind Studio — Roadmap

This roadmap is the build plan for v1. Phases are sequential. Each phase has explicit deliverables and acceptance criteria. **Do not advance a phase until acceptance criteria pass.**

## Dogfooding Goal

By end of Phase 7, build **Nous Philosophy DSLM** using NarrowMind Studio itself and deploy it to the Nous website via the published SDK. A secondary dogfood target is an **op-amp tutor DSLM** built from electronics coursework material.

The Nous Philosophy DSLM must be reachable via the Local Chat button — no agent intermediation. The product story stands or falls on this single click; if the dogfood DSLM can't be reached this way, the architecture is wrong, not the demo.

---

## Phase 0 — Repo Bootstrap (½ day)

**Goal**: Empty but well-structured monorepo, all toolchains working end-to-end.

**Deliverables**:
- Monorepo skeleton matching `ARCHITECTURE.md → Repository Layout`
- `package.json` (pnpm workspaces), `Cargo.toml` (workspace), `pyproject.toml` (uv-managed)
- Tauri v2 app bootstrap; `pnpm tauri dev` opens an empty React shell
- Python `narrowmind_workers` package skeleton with a `hello` RPC method
- Rust orchestrator crate with stub `spawn_worker` that launches Python `hello` and returns the result to the Tauri shell
- `AGENTS.md`, `ARCHITECTURE.md`, `ROADMAP.md`, `README.md` committed
- CI (GitHub Actions): `cargo check`, `pnpm typecheck`, `pytest -q` on every push

**Acceptance**: `pnpm tauri dev` opens the window. A debug button in the UI triggers the Rust → Python `hello` round-trip; the response appears in a log pane.

---

## Phase 1 — Shell + Agent Loop (~1 week)

**Goal**: Claude Code–style agent works end-to-end against the Anthropic API with a minimal tool set.

**Deliverables**:
- xterm.js terminal in the center pane (streaming output, command input)
- Three side panels (Dataset / Training / Eval) wired to mock state (placeholder content only)
- Left rail: project switcher (create / open / delete)
- `crates/agent`:
  - `Provider` trait + `AnthropicProvider` implementation
  - Tool dispatcher: receives `tool_use` blocks, routes to orchestrator handlers, returns `tool_result`
  - Streaming response handler forwarded to the UI
- Settings dialog: provider selection (Anthropic only in v1.0), API key input stored via OS keychain (`keyring` crate)
- v0 tool set, end-to-end:
  - `read_file`, `write_file`, `list_dir`, `run_command` (sandboxed to project dir)
  - `project_status`, `create_project`, `list_projects`
- `project.toml` schema validated on read/write

**Acceptance**: User opens app, sets the Anthropic key, types: *"Create a project called test-philosophy and write a hello file in it"*. Agent calls tools, project appears in the rail, file exists on disk, transcript visible in the terminal pane.

---

## Phase 2 — Dataset Studio (~1–2 weeks)

**Goal**: Ingest from many sources → reviewable, exportable dataset. The most product-defining phase.

**Deliverables**:
- `ingestion` worker with handlers for:
  - Local files: PDF (PyMuPDF), EPUB (EbookLib), TXT, MD, DOCX (python-docx), HTML
  - URL ingest: trafilatura (single page + bounded crawl)
  - Wikipedia: category-based scrape (specify category, max depth, max pages)
  - HuggingFace datasets: search and import with split selection
- Cleaning pipeline:
  - Boilerplate removal
  - Near-duplicate filtering (MinHash)
  - Language filter (fastText)
  - Quality heuristics (length, optional perplexity gate)
- Chunking: token-window (default) and semantic (sentence-boundary), size/overlap configurable
- Dataset Browser panel:
  - Virtualized chunk list (TanStack Virtual)
  - Per-source filter, search, per-chunk include/exclude toggle
  - Source metadata pane
- Synth Q&A generation: base-model-powered pipeline producing SFT pairs from chunks
- Outputs: `rag.jsonl`, `sft.jsonl`, `eval.jsonl` (held-out ~10%)
- New tools: `ingest_source`, `list_chunks`, `filter_chunks`, `build_dataset`, `generate_sft`

**Acceptance**: From the terminal: *"Ingest English Wikipedia category 'Philosophy of mind' to depth 2, max 500 pages. Clean and chunk it. Generate 2000 SFT pairs and a held-out eval set."* Result: project contains reviewable chunks, generated pairs, and a held-out eval — all visible in the Dataset Browser.

---

## Phase 3 — RAG End-to-End (~3–5 days)

**Goal**: First working DSLM technique. Tier 0 (RAG-only) chat against the user's domain.

**Deliverables**:
- `rag` worker: LlamaIndex pipeline + LanceDB (embedded, file-backed) + BGE-small embeddings
- `inference` worker: llama.cpp server wrapper, model loader, OpenAI-compatible local endpoint
- Chat preview surface (modal or terminal-embedded): query → retrieve top-k → assemble prompt → stream tokens
- New tools: `build_index`, `query_index`, `start_inference_server`, `rag_chat`
- Eval scaffold: `run_eval` computing retrieval recall + answer-relevance via LLM-judge over the held-out eval set
- Auto-generated run report (markdown) written to `evals/`

**Acceptance**: User says *"Use Qwen2.5-7B and the dataset I just built. Stand up RAG and answer: 'What is the hard problem of consciousness?'"*. A reasoned answer streams with citations to retrieved chunks. Eval produces a report with recall@5 and judge scores.

### Phase 3.5 follow-up — Retrieval Polish (1–2 days)

Run between Phase 3 and Phase 4 once the 19-pair acceptance eval showed
the dense-only retriever missing too many proper-noun-heavy questions
(recall 0.79, judge 3.37 — both below the bar to start fine-tuning on
the dataset). Not a numbered phase; the deliverables ride on top of
Phase 3's vector store and Phase 3's tooling shape.

**Scope**:
- BM25 sparse retriever (LanceDB FTS) alongside the existing dense
  index, fused with Reciprocal Rank Fusion (k=60, Cormack/Clarke 2009)
- `[rag]` section in `project.toml` (`retrieval_mode`, `top_k`,
  `hybrid_k_dense`, `hybrid_k_sparse`, `rrf_k`) with hybrid as default
- `mode` arg threaded through `query_index` / `rag_chat` / `run_eval`
  agent tools for one-prompt A/B sweeps
- Eval-set expansion: re-split sft+eval at 75/25 (140 + 46) and
  generate 10 proper-noun-targeted pairs via cheap-model synth, for a
  total 56-pair eval

**Acceptance**: hybrid retrieval reaches recall@5 ≥ 0.85 AND judge
mean ≥ 3.8 on the expanded eval set. On the dogfood project
(2026-05-18): recall **0.98** / judge **4.55**, zero catastrophic
(score≤2) failures. Multi-config comparison archived at
`evals/2026-05-18-multiconfig.md`.

---

## Phase 4 — Fine-Tuning Path (~1–2 weeks)

**Goal**: Tier 1 working. QLoRA on 7B with Unsloth, fully observable from the UI, runs on RTX 3070.

**Deliverables**:
- `training` worker: Unsloth backend, QLoRA config presets keyed to hardware profile (the **rtx-3070-8gb** preset is the reference)
- Training launch flow: validate dataset → confirm config → spawn worker → stream metrics
- Training Monitor panel: live loss / lr / grad-norm (Recharts), step counter, ETA, GPU mem watch, log tail
- Checkpoint manager: list, mark "best" by eval loss, mark for export
- Resume support (re-attach to a running worker after app restart)
- New tools: `start_training`, `stop_training`, `list_runs`, `select_checkpoint`
- Axolotl as an optional power-user backend, gated behind a flag in `project.toml`

**Acceptance**: User runs a QLoRA fine-tune of Qwen2.5-7B on the philosophy SFT dataset. Run completes without OOM on RTX 3070 8 GB, produces an adapter, and appears in the Training Monitor with full metrics history.

### Phase 4.5 follow-up — Second Base Model: Gemma 4 12B (~2-3 days)

Gemma 4 12B (released 2026-06-03, official QAT checkpoints 2026-06-05) is
registered in the base model registry as a non-default entry. Phase 4 ships
and is accepted on Qwen2.5-7B; Phase 4.5 then answers "does a newer, larger
QAT base beat our tuned 7B on the same data?" with evidence instead of vibes.

**Scope**:
- QLoRA fine-tune of Gemma 4 12B on the same SFT dataset used in Phase 4
  (registry id `gemma-4-12b-it`; QAT-aware GGUF serving via
  `unsloth/gemma-4-12B-it-qat-GGUF` — see `docs/quantization.md` for why a
  generic conversion is forbidden)
- Eval Console comparison: Qwen2.5-7B fine-tune vs Gemma 4 12B fine-tune on
  the same eval set (judge + recall + manual ratings)
- VRAM watch: 12B QLoRA sits at 8-10 GB — the RTX 3070's 8 GB is the floor;
  document OOM behavior and any required gradient-checkpointing/seq-len
  trade-offs in the run report

**Entry criteria**: Phase 4 accepted + Unsloth Gemma 4 support re-confirmed
at entry (confirmed as of 2026-06-11: text/vision/audio/RL fine-tuning,
12B QLoRA on consumer GPUs) + exact bnb-4bit training-variant and GGUF
filename re-verified against HF.

**Acceptance**: both fine-tunes complete on the reference card, the Eval
Console comparison report ranks them on the shared eval set, and the registry
entry's `quantization_notes` (QAT trap, Turkish-eval caveat) are surfaced in
the run report.

### Phase 4.6 — Efficiency (Tier 1)

**Goal**: run a domain model under tight VRAM on the RTX 3070 (8 GB, Ampere —
GGUF k-/IQ-quant lane, no FP8). *Quantization is the memory backbone;
everything else is secondary for "fit it in memory."* See
`docs/quantization.md → Phase 4.6` for the full rationale.

**Supersession note**: this pulls the GGUF-production half of Phase 6 forward
(merge→GGUF + imatrix), overriding the Phase 4 KARAR 8 deferral ("adapter HF
format only"). Phase 6 keeps the *packaging* layer (Modelfile, SDK, templates).

**Deliverables** (incremental, one reviewable slice each):
- *Run side (shipped):*
  - **KV-cache quantization** — `kv_cache_type` on `ModelSpec` (default `q8_0`,
    near-lossless, ~half the KV VRAM so longer RAG context fits); `f16` opt-out,
    `q4_0` aggressive opt-in; quantized cache auto-enables Flash Attention.
  - **Prompt-prefix cache** — reuse llama.cpp's host-RAM prompt cache for the
    shared system prompt + repeated RAG context (no VRAM cost).
- *Produce side (gated on a trained adapter + provisioned llama.cpp binaries):*
  - **Merge adapter → one GGUF per domain**, managed as files under
    `projects/<name>/models/` (replaces runtime multi-LoRA paging — out of scope).
  - **Domain-calibrated imatrix quantization** — use the project corpus
    (`datasets/rag.jsonl`) as the `llama-imatrix` calibration set → low-bit GGUF
    (Q4_K_M default, IQ4_XS / IQ3 for headroom). Output GGUF, no PyTorch.

**Tier 2 (conditional, not built — triggers noted)**: attention-sink
preservation (only if KV compression / a sliding window lands); ExLlamaV3 (only
if a fixed small model fully fits 8 GB and hits a speed wall — it loses
llama.cpp's CPU offload, so never a default).

**Tier 3 (research, gated, hooks only)**: STE-trained retrieval/budget gate
(Gumbel-softmax + straight-through estimator, trained in py-training, ties to
hybrid retrieval); D-LLM-style dynamic layer skipping (needs a PyTorch
inference path — GATE: first prove FLOPs/KV savings survive on a 4-bit base,
since weights dominate the 8 GB budget and quantization already shrinks them).

**Out of scope (hard guardrails)**: engine swaps (vLLM / TensorRT-LLM / SGLang
/ TGI — llama.cpp stays), llm-d / Kubernetes / distributed serving, runtime
multi-LoRA adapter paging, tensor-network / quantum-mechanics compression, FP8
(no Ampere support).

**Acceptance**: serving the default Qwen GGUF with `q8_0` KV + prompt cache
passes a GPU smoke (health + Local-Chat generation + reduced KV footprint) and
holds the 56-pair eval (recall ≈ 0.98 / judge ≈ 4.55, no score≤2); a domain
fine-tune merges to a single imatrix-quantized GGUF and serves through Local
Chat.

---

### Phase 4.7 — Dataset Expansion (HF datasets)

**Goal**: lift fine-tune quality past base+RAG by training on bigger, more
thorough domain data. The Phase 4.6 eval found the 140-pair *synthetic* fine-tune
**lost** to base+RAG (judge 4.29 vs 4.46) because its answers were terse — the
lever is SFT data quality + breadth, not retrieval.

**Approach**: a real Stanford Encyclopedia of Philosophy (SEP) domain built from
ready-made HuggingFace datasets (all SEP-derived, sharing a `category` key):
- *SFT* — `ruggsea/...instruct` (long, formal answers) + category-filtered
  `sayhan/strix-philosophy-qa` for topic breadth.
- *RAG* — `AiresPucrs/stanford-encyclopedia-philosophy` (the raw SEP passages).

**Deliverables** (incremental, one reviewable slice each):
- **`import_sft_from_hf`** *(slice A)* — import ready HF QA datasets directly as
  `sft.jsonl` + `eval.jsonl`, bypassing synthetic generation. Per source: column
  mapping, category filter, min-answer-length, seeded cap; reuses `generate_sft`'s
  seeded split.
- **`ingest_source type=hf_dataset`** *(slice B)* — fills the reserved `HF_DATASET`
  handler: pull a text column from a HF dataset → the existing chunk/clean/dedup
  pipeline → RAG corpus. Even, order-spanning subsample under a cap.
- **SEP re-fine-tune experiment** *(slice C)* — new `felsefe-sep` project, ingest
  SEP for RAG + import the QA datasets for SFT, train, export GGUF, eval.

Both new paths run in the CPU `py` env and reuse the existing `datasets` dep — zero
new dependencies, the py / py-training split intact. See `docs/datasets.md`.

**Acceptance**: the SEP fine-tune+RAG judge score **beats base+RAG's 4.46** (no
score≤2, recall ≥ ~0.95) — the single number proving the thorough-SFT thesis.
Leaves `deneme1-faz2` intact as the A/B baseline.

---

## Phase 5 — Hybrid + Eval Console (~1 week)

**Goal**: Tier 2 (LoRA + RAG) usable. Eval Console makes "which technique is best" answerable.

**Deliverables**:
- Hybrid inference: load base + LoRA + RAG pipeline together in `inference` worker
- Eval Console panel:
  - A/B/C/D side-by-side: base / RAG / LoRA / hybrid
  - Same prompt fans out to all four, responses tile
  - Manual rating UI (1–5 + free-text), ratings persisted to `evals/ratings.jsonl`
  - Auto LLM-judge across all four configs with summary report
- Comparison report export (markdown + JSON) suitable for a blog post or PR
- New tools: `compare_models`, `rate_response`, `export_eval_report`

**Acceptance**: User runs a comparison over 50 eval questions. Eval Console shows the matrix, judge scores all four configs, user rates 20 manually, and a markdown report is exported.

---

## Phase 6 — Deployment + Integration (~3–5 days)

**Goal**: Ship the model out of the app and into a real website.

> **Scope moved**: the GGUF-production steps below (merge → convert → quantize)
> were pulled forward to **Phase 4.6 — Efficiency** so domain models can be
> served under 8 GB earlier. Phase 6 now consumes the GGUF that 4.6 produces and
> focuses on packaging + distribution (Modelfile, Ollama registration, SDK,
> templates). The export-worker bullets are kept here for continuity; treat them
> as "wire 4.6's GGUF output into the Modelfile/Ollama flow."

**Deliverables**:
- `export` worker (GGUF production now lands in Phase 4.6; here it is reused):
  - Merge LoRA into base (HF format) — *Phase 4.6*
  - Convert to GGUF (vendored `convert_hf_to_gguf.py`) — *Phase 4.6*
  - Quantize (Q4_K_M default; Q5_K_M, Q8_0 optional; domain imatrix) — *Phase 4.6*
  - Generate Ollama `Modelfile` with the project's system prompt baked in
- One-click "Export to Ollama" flow: writes to `models/<tag>/`, optionally runs `ollama create <tag>` if Ollama CLI is present
- `packages/sdk-js` published as `@narrowmind/client` on npm:
  - `NarrowMindClient` class wrapping Ollama HTTP API
  - Optional `withRetrieval(snapshotPath)` for sites bundling a vector snapshot
  - TS types, dual ESM/CJS builds
- Template repos under `examples/`:
  - `examples/nextjs-chat/` — drop-in chatbot (target: Nous site)
  - `examples/express-api/` — server-side endpoint
- New tools: `export_gguf`, `export_modelfile`, `register_with_ollama`

**Acceptance**: User runs *"Export the hybrid philosophy model as Q4_K_M and register it with Ollama as `nous-philosophy:v1`"*. In a fresh terminal, `ollama run nous-philosophy:v1` works. The Next.js template repo connects and chats with it.

---

## Phase 7 — OSS Release (~1 week)

**Goal**: Public release that a stranger can succeed with end-to-end.

**Deliverables**:
- Docs site (VitePress): getting-started, "Build your first DSLM in 30 minutes", architecture reference, troubleshooting, hardware compatibility matrix
- Pre-built demo project: `nous-philosophy` (public dataset, fine-tuned + RAG)
- Pre-built demo project: `opamp-tutor` (electronics op-amp Q&A model)
- License: **Apache-2.0**
- Contribution guide, code of conduct, issue templates
- GitHub Actions: lint + test + Tauri release bundles (macOS, Windows, Linux)
- Additional providers shipped: `OpenAIProvider`, `OllamaProvider`, `CustomProvider` (OpenAI-compatible endpoint)
- **Local Chat hero positioning in README** (top of file, before any tier explanation), plus an animated demo (GIF or short video) showing: click button → window opens → first token streams → all in under 5 seconds.

**Acceptance**: A stranger clones the repo, follows getting-started, and produces a working DSLM from their own PDFs within an hour. The two demo projects ship as downloadable artifacts on the docs site.

---

## Out of Scope (v1)

Tracked in a future `FUTURE.md`:

- Tier 3 — continued pretraining
- Multi-node distributed training
- DPO / RLHF beyond storing manual ratings
- Cloud-runner integration (Modal, RunPod) for users without a GPU
- Web/SaaS deployment of the Studio itself
- Mobile UI
- Auto-mode agent (single goal → end-to-end without confirmations)

---

## Effort Estimate

~6–8 weeks for v1 with Claude Code as the implementer and Hasancan as architect/reviewer.
