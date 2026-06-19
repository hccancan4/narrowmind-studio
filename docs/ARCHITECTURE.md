# NarrowMind Studio — Architecture

## Vision

NarrowMind Studio is an open-source desktop IDE for building **Domain-Specific Language Models (DSLMs)** without writing training boilerplate. It orchestrates proven open-source tooling (Unsloth, LlamaIndex, llama.cpp, HuggingFace) behind a Claude Code–style agent loop, so a single user with a consumer GPU can go from *"I have some PDFs and a domain idea"* to *"I have a deployed Ollama model serving my website"* without leaving the app.

The reference development hardware is **RTX 3070 (8 GB VRAM) + 32 GB RAM**. Every default config shipped in the codebase must be feasible on that profile.

---

## Core Concepts

### DSLM Tiers

NarrowMind Studio supports three production techniques in increasing complexity:

- **Tier 0 — RAG only**: base model + domain corpus indexed in a local vector store. No fine-tuning, no GPU required for "training". Best for factual-recall domains (philosophy, law, technical reference, scientific lookups).
- **Tier 1 — LoRA / QLoRA fine-tune**: adapter training on a 7B-class base model via Unsloth. Gives the model the domain's style, format, jargon, persona. Requires a GPU.
- **Tier 2 — Hybrid**: fine-tuned base + RAG retrieval. Style and persona from the adapter, factual grounding from retrieval. The default production-grade recipe.

Tier 3 (continued pretraining) is out of scope for v1.

### Project

A "project" is a self-contained DSLM build. On disk:

```
~/narrowmind/projects/<name>/
  project.toml            # metadata, tier, base model, status
  sources/                # raw user uploads + scrape configs
  datasets/               # processed JSONL (rag.jsonl, sft.jsonl, eval.jsonl)
  vector_store/           # LanceDB tables (RAG)
  runs/                   # training runs (checkpoints, logs, metrics)
  models/                 # exported artifacts (gguf, hf, Modelfile)
  evals/                  # eval reports, ratings.jsonl
  agent.log               # full agent transcript
```

The filesystem is the source of truth. The orchestrator reads/writes files; the UI is a view over them. **If state isn't on disk, it doesn't exist.**

### Local Chat (Zero-API)

NarrowMind Studio'nun temel UX taahhüdü: kendi DSLM'inle konuşmak için
asla bir API çağrısına ihtiyaç duyulmaz. Ana ekrandaki "💬 Local chat"
butonu, hiçbir orkestrasyon, hiçbir agent loop, hiçbir LLM provider
olmadan bir chat penceresi açar:

- Local inference server (llama.cpp + GGUF base + opsiyonel LoRA adapter)
- Local embedding model (BGE-small, CPU)
- Local vector store (LanceDB, embedded)

Bootstrap path'i agent loop'unu tamamen bypass eden dedicated bir Tauri
command'tır (`chat_preview_bootstrap`). Bu mimari taahhüt katıdır: kullanıcı
Local Chat'i sıfır outbound API trafiğiyle açabilmelidir. Bu codebase'in
bir özelliği olarak korunur, pazarlama iddiası değildir.

Local Chat, NarrowMind Studio'yu komşu araçlardan ayırır:

| Tool | Local inference | Native RAG | One-click chat |
|---|---|---|---|
| Ollama | ✓ | ✗ | ✓ (no RAG) |
| LM Studio | ✓ | ✗ | ✓ (no RAG) |
| LangChain | ✓ | ✓ | ✗ (kod gerekir) |
| llama.cpp + custom RAG | ✓ | ✓ | ✗ (setup gerekir) |
| **NarrowMind Studio** | ✓ | ✓ | ✓ |

### Base Model Registry

`crates/orchestrator/src/models.rs` is the single static catalog of base
models the studio can serve and (Phase 4+) fine-tune. Each entry carries the
metadata the serving `ModelSpec` never did: training repo (Unsloth 4-bit
variant), GGUF source + file, tokenizer id, VRAM profile against the
reference RTX 3070, the model's real context window (distinct from
`SERVING_N_CTX = 4096`, our KV-cache budget on 8 GB), chat-template family,
license, quantization caveats, and `training_supported` / `default` flags.

Design rules:

- **Static Rust table, not a TOML manifest.** Filesystem-as-truth governs
  *project state*; a built-in catalog is a code constant (same reasoning as
  the `retry::timeouts` registry). User-defined models are a Phase 5+/7
  concern — a TOML overlay can extend the table then without changing shape.
- **Exactly one `default: true` entry** (test-enforced). `models::default_model()`
  is what `ModelSpec::default_qwen2_5_7b_q4km()` now delegates to; a pinning
  test asserts the output is field-for-field identical to the pre-registry
  literals, so the refactor cannot silently change serving behavior.
- **`quantization_notes` are load-bearing documentation**, surfaced to the
  user before quant swaps (e.g. Gemma 4's QAT-vs-naive-Q4_0 trap — see
  `docs/quantization.md`).
- Registry ids (`qwen2.5-7b-instruct`, …) are wire-stable: from Phase 4 on,
  `project.toml`'s `base_model` field references them.

---

## Repository Layout

Monorepo. pnpm workspaces + Cargo workspace + uv-managed Python.

```
narrowmind-studio/
  apps/
    desktop/                     # Tauri + React + TS shell
  crates/
    orchestrator/                # Rust: process mgmt, project state, IPC
    agent/                       # Rust: LLM provider abstraction + tool dispatch
  workers/
    py/
      narrowmind_workers/
        ingestion/               # PDFs, Wikipedia, HF datasets, web scrape
        training/                # Unsloth wrapper (default), Axolotl (alt)
        inference/               # llama.cpp serving
        rag/                     # LlamaIndex + LanceDB
        eval/                    # lm-eval-harness + custom domain eval
        export/                  # GGUF conversion, Ollama Modelfile gen
        rpc/                     # JSON-RPC server over stdio
  packages/
    sdk-js/                      # @narrowmind/client — npm package
  examples/
    nextjs-chat/                 # site integration template
    express-api/                 # server-side template
  docs/
    ROADMAP.md
    ARCHITECTURE.md
  AGENTS.md
  README.md
```

---

## Component Overview

### 1. Desktop Shell (`apps/desktop`)

Tauri v2 + React + TypeScript (strict) + Tailwind + shadcn/ui.

Layout:
- **Center**: terminal pane (xterm.js) showing the agent transcript and accepting commands
- **Right sidebar** (collapsible, three tabs):
  - **Dataset Browser** — ingest sources, view chunks, filter, preview
  - **Training Monitor** — live loss / lr / grad-norm (Recharts), checkpoint list
  - **Eval Console** — A/B/C/D comparison, manual rating, eval reports
- **Left rail**: project switcher, settings, model library

The shell **never** runs ML code. It speaks to the Rust orchestrator over Tauri IPC only.

### 2. Rust Orchestrator (`crates/orchestrator`)

The runtime owner. Responsibilities:

- Project state machine (read/write `project.toml`, validate transitions)
- Python worker lifecycle: spawn as long-lived sidecar processes, manage stdio JSON-RPC, restart on crash, kill on app exit
- Filesystem watcher for live UI updates
- Typed IPC with the Tauri shell
- Secret management via `keyring` crate (OS keychain)

### 3. Agent (`crates/agent`)

The agent loop runs in Rust:

1. Read user message from the shell
2. Call the configured LLM provider with the tool schema
3. Dispatch `tool_use` blocks to orchestrator handlers (which may invoke a worker)
4. Stream `tool_result` and assistant text back to the shell

**Provider abstraction** is a `Provider` trait with adapters:

- `AnthropicProvider` — default in v1
- `OpenAIProvider` — Phase 7
- `OllamaProvider` — Phase 7 (enables fully local agent)
- `CustomProvider` — any OpenAI-compatible endpoint

The user picks the provider in Settings and supplies their own API key (stored in the OS keychain).

**Tool set (v1)**:

| Category | Tools |
|---|---|
| Files | `read_file`, `write_file`, `list_dir`, `run_command` (see trust model below) |
| Project | `project_status`, `create_project`, `update_project`, `list_projects` |
| Data | `ingest_source`, `list_chunks`, `filter_chunks`, `build_dataset`, `generate_sft` |
| Train | `start_training`, `stop_training`, `list_runs`, `select_checkpoint` |
| RAG | `build_index`, `query_index` |
| Inference | `start_inference_server`, `chat`, `rag_chat` |
| Eval | `run_eval`, `compare_models`, `rate_response`, `export_eval_report` |
| Export | `export_gguf`, `export_modelfile`, `register_with_ollama` |

**`run_command` trust model.** The file tools (`read_file` / `write_file` /
`list_dir`) enforce a strict path sandbox (`sandbox::resolve_within` rejects
absolute paths, `..`, drive prefixes). `run_command` deliberately does NOT:
its working directory is pinned to the project root, but the executable
itself is unrestricted — the agent legitimately runs `python`, `uv`, `git`,
and training scripts, and an executable allowlist is infeasible. This
sandbox is **spatial by convention, not a security boundary**: it trusts the
local user and the model acting on their behalf. Compensating controls:
every invocation is appended to the project's `agent.log` audit trail, UNC
(`\\server\share`) executables are rejected outright, and no shell is ever
invoked (argv passes through verbatim — no quoting/injection surface).
Real containment (Windows Job Objects, per-executable confirmation) is a
Phase 7 pre-OSS decision; see `docs/audit-pre-phase4.md` § 1b.

### 4. Python Workers (`workers/py`)

Each worker is a Python 3.11+ process speaking JSON-RPC 2.0 over stdio. The orchestrator multiplexes requests. Workers are **stateless between requests**; all project state lives on disk.

**Why subprocess + RPC instead of PyO3?** Isolation. ML code crashes, leaks CUDA contexts, hogs memory. A subprocess boundary means a training crash does not take down the app. Trade-off: serialization cost — accepted, because we **never pass tensors across the boundary**, only file paths and metadata.

Workers:

- **`ingestion`** — PyMuPDF (PDFs), EbookLib (EPUB), trafilatura (web), wikipedia-api (categorized scrape), `datasets` (HF), python-docx. Outputs cleaned chunks to `sources/<id>/chunks.jsonl`.
- **`training`** — Unsloth as the default backend (4-bit QLoRA, fits 7B on 8 GB VRAM). Axolotl as the "power user" alternative for config-driven runs. Reads SFT JSONL, writes adapter to `runs/<id>/adapter/`. Streams metrics to the orchestrator via RPC notifications.
- **`inference`** — llama.cpp server (via `llama-cpp-python` or subprocess to `llama-server`). Loads GGUF + optional LoRA. Exposes an OpenAI-compatible local endpoint used by the chat preview and the eval worker. Phase 4.6 adds 8 GB-oriented serving levers: KV-cache quantization (`q8_0` default) and prompt-prefix caching — see `docs/quantization.md → Phase 4.6`.
- **`rag`** — LlamaIndex + LanceDB (embedded, file-backed, no service). BGE-small as default embedding model. Optional BGE-reranker-base.
- **`eval`** — auto path: held-out chunks → synthesize Q&A via base model → run candidate models → LLM-judge scoring. Manual path: serves comparison samples to the UI. Standard path: optional `lm-evaluation-harness` integration for benchmarks.
- **`export`** — GGUF conversion via vendored `convert_hf_to_gguf.py`, quantization (Q4_K_M default, Q5_K_M, Q8_0), Ollama `Modelfile` generation. The merge→convert→quantize (incl. domain imatrix) steps land in **Phase 4.6**, superseding the Phase 4 KARAR 8 "no GGUF" deferral; Phase 6 reuses that GGUF for packaging.

### 5. SDK (`packages/sdk-js`)

Tiny TypeScript package: `@narrowmind/client`. Wraps the Ollama HTTP API plus optional retrieval against an exported LanceDB snapshot. Embeds a DSLM in a Next.js / Astro / Express app in ~10 lines of code. Dual ESM/CJS build.

---

## Data Flow

### Ingestion → Dataset

```
[user uploads / scrape config]
   │
   ▼
ingestion worker
   │
   ├── raw extraction (text + metadata)
   ├── cleaning (boilerplate, dedup, lang filter, quality heuristics)
   └── chunking (semantic or token-window)
   │
   ▼
sources/<id>/chunks.jsonl
   │
   ▼
[user reviews + filters in Dataset Browser]
   │
   ▼
build_dataset:
   ├── rag.jsonl     (chunks + embeddings, indexed in vector_store/)
   ├── sft.jsonl     (instruction pairs, synth-generated via base model)
   └── eval.jsonl    (held-out ~10%)
```

### Training (Tier 1)

```
sft.jsonl + base_model_id + preset
   │
   ▼
training worker (Unsloth QLoRA)
   │
   ├── checkpoints/  in runs/<id>/
   └── adapter/      in runs/<id>/
   │
   ▼
eval worker auto-runs domain Q&A
   │
   ▼
run report (markdown) in evals/
```

### Inference (Tier 0 / 1 / 2)

```
user query
   │
   ▼
[Tier 2: rag worker retrieves top-k chunks]
   │
   ▼
inference worker (base + optional adapter, GGUF or HF)
   │
   ▼
streamed tokens to UI / SDK
```

### Export

```
adapter + base_model + system_prompt
   │
   ▼
export worker
   ├── merge adapter into base (HF format)
   ├── convert to GGUF
   ├── quantize (Q4_K_M default)
   └── generate Modelfile
   │
   ▼
models/<tag>/
```

---

## Hardware Calibration

The **rtx-3070-8gb** preset is the reference. Every default config must work on it.

| Operation | Config | Status |
|---|---|---|
| QLoRA fine-tune 7B (Qwen2.5, Llama-3.1-8B, Mistral) | Unsloth, 4-bit, BS=2, grad_accum=8, seq_len=2048 | ✅ |
| QLoRA fine-tune 13B | n/a — out of profile | ❌ |
| Inference 7B GGUF Q4_K_M | ~4 GB VRAM | ✅ |
| Inference 7B + LoRA | merged at export → same as above | ✅ |
| Embedding (BGE-small) | <1 GB VRAM, CPU also OK | ✅ |
| RAG over <1M chunks | LanceDB on disk, query <100 ms | ✅ |
| Synth Q&A generation | local 7B in GPU, ~10 pairs/min | ✅ |

For users with more VRAM (e.g. RTX 4090 24 GB), presets unlock 13B fine-tuning and longer sequence lengths. For users without a GPU, Tier 0 (RAG only) is always available. Cloud runners (Modal / RunPod) for Tier 1 are post-v1.

**Default base models**, picked per profile and domain heuristics:

- **Qwen2.5-7B-Instruct** — strongest multilingual including Turkish; default for v1
- **Llama-3.1-8B-Instruct** — strong English, large community
- **Mistral-7B-Instruct-v0.3** — efficient, permissively licensed

---

## Provider Configuration & Secrets

- API keys stored in OS keychain via `keyring`:
  - macOS Keychain
  - Windows Credential Manager
  - libsecret on Linux
- Never written to disk in plaintext, never logged, never committed.
- Project files reference providers by ID, not by key.

---

## Worker Process Model (resolved post-3.5)

Originally Open Question #2 ("long-lived per worker type, or spawn-per-task?").
Resolved as a deliberate **split** — both, scoped by workload:

- **`WorkerPool`** (`crates/orchestrator/src/worker_pool.rs`) — one persistent
  Python child per worker module, spawned lazily, calls serialized behind a
  per-worker async mutex. Serves **fast repeated queries**: today that is
  `rag.query` only. The win: BGE-small loads once per app session instead of
  once per call (~5-8 s of interpreter + import + model load per retrieval in
  the old spawn-per-task model — paid by every Local Chat message and ×4
  during parallel eval). Failure semantics: in-flight timeout kills the child
  (state unknown, next caller respawns lazily); queued timeout leaves the
  busy-but-healthy worker untouched; dead children respawn transparently with
  exactly one retry; stderr is drained continuously into a ring buffer so
  tracebacks survive and the 64 KB pipe buffer can't deadlock a chatty child.
  Shut down deterministically on Tauri `RunEvent::Exit` (stdin EOF → grace →
  tree kill — no orphan `python.exe`).
- **One-shot `call_worker`** (`crates/orchestrator/src/worker.rs`) — spawn,
  one request, one response, exit. Serves **long batch jobs**: ingestion
  (600 s), corpus embed + FTS build (900 s), model downloads (60 min), and
  the `hello` connectivity probe. Pooling these would head-of-line-block
  Local Chat retrieval behind multi-minute jobs, and per-job crash isolation
  is a feature for jobs that parse hostile PDFs.

Phase 4 hook: pooled calls are serialized, so an id-less JSON-RPC frame
arriving mid-call unambiguously belongs to the in-flight request. Training
progress streaming therefore needs only a notification callback on the read
loop (which already skips such frames) — no protocol redesign.

---

## Open Architecture Questions

To be resolved during Phase 1:

1. **Agent planner location** — should planning happen client-side in the LLM (Claude Code style) or in a lightweight Rust planner? *Lean: LLM-only.*
2. **Worker process model** — ~~long-lived per worker type, or spawn-per-task?~~ **Resolved post-3.5** — see "Worker Process Model" above: pool for fast repeated queries, one-shot for batch jobs.
3. **IPC type-sharing** — `tauri-specta` for auto-generated TS types from Rust, or hand-written? *Lean: tauri-specta if stable on Tauri v2.*
4. **Telemetry** — opt-in anonymous run metadata (not data) to power future model-recommendation features. *Default off, document clearly.*
