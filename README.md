# NarrowMind Studio

Open-source desktop IDE for building **Domain-Specific Language Models (DSLMs)** without writing training boilerplate. Orchestrates Unsloth, llama.cpp, LanceDB and HuggingFace behind a Claude Code–style agent loop. Goes from *"I have some PDFs and a domain idea"* to *"I have a locally served GGUF model answering domain questions"* on a single consumer GPU.

**Reference hardware:** RTX 3070 (8 GB VRAM) + 32 GB RAM, Windows + WSL2.

> **Status:** working **produce → run → measure** pipeline through **Phase 4.8**. Ingest (PDF / EPUB / HTML / Wikipedia / URL / HuggingFace datasets) → grounded synthetic SFT → QLoRA fine-tune → merged + imatrix GGUF export → local serve (KV-cache quant + prompt cache) → RAG eval (recall + LLM-judge), all on a single 8 GB GPU. See [docs/ROADMAP.md](docs/ROADMAP.md).

---

## What makes it different

**💬 Local Chat (Zero-API)** — one button on the main view opens a chat
window backed entirely by your local hardware: llama.cpp inference,
BGE embeddings, LanceDB vector store. Zero outbound API calls, zero
tokens spent. Roughly three seconds from button click to first streamed
token on a consumer GPU.

This isn't a feature you turn on — it's an architectural guarantee.
Your DSLM is always reachable without speaking to anyone else's servers.

<!-- demo GIF placeholder — Phase 7'de eklenecek -->

---

## What's in the box

NarrowMind Studio ships three production techniques for shaping a small base model into a domain expert:

- **Tier 0 — RAG only.** Base model + local vector store. No fine-tuning, no GPU required.
- **Tier 1 — LoRA / QLoRA fine-tune.** Adapter training on a 0.5–7B base via Unsloth.
- **Tier 2 — Hybrid.** Fine-tuned base + RAG retrieval. The default production-grade recipe.

The filesystem is the source of truth. Project state lives under `projects/<name>/` (configurable via `NARROWMIND_PROJECTS_ROOT`) as TOML + JSONL + markdown; the UI is a view over those files.

---

## What works today

The full **produce → run → measure** loop, on a single RTX 3070 (8 GB):

1. **Ingest** — PDFs, EPUBs, HTML, Wikipedia categories, web crawls, and HuggingFace datasets → cleaned, sentence-chunked, MinHash-deduped.
2. **Dataset** — grounded synthetic SFT (`generate_sft`, completeness-tuned, chunk-grounded so recall stays measurable) *or* direct import of ready HF QA datasets; held-out eval split; embeddings in a local LanceDB store.
3. **Fine-tune** — QLoRA via Unsloth (inside WSL2 on Windows) on any registered base: the Qwen2.5 0.5B / 1.5B / 3B / 7B ladder, the newer Qwen3 0.6B / 1.7B / 4B ladder, or Gemma 4 12B (QAT). Training is hard-mutexed against the inference server so the 8 GB card never double-loads.
4. **Export** — merge adapter → one GGUF per domain, with optional domain-calibrated imatrix quantization (IQ3 / IQ4 / Q4_K). No PyTorch at run time.
5. **Serve** — llama.cpp with KV-cache quantization (Q8_0) + a prompt-prefix cache, so a 7B fits the 8 GB band with room for longer RAG context.
6. **Measure** — RAG eval with retrieval recall@k + an LLM-judge, sweepable across dense / sparse / hybrid retrieval.

### Headline result — *the smallest viable DSLM*

A philosophy DSLM built from the Stanford Encyclopedia of Philosophy taught the lesson that now shapes the pipeline: **data quality beats model size.** Raw encyclopedia passages as SFT targets gave a **7B** fine-tune a **2.00 / 5** judge score with 31 % fabrication; regenerating the targets as *grounded, completeness-tuned synthetic* answers took a **3B** fine-tune to **3.80 / 5** with retrieval recall at **0.98** — a smaller model beating a larger one on data quality alone, trained in a fraction of the 7B's ~6 h and served from a **1.8 GB** GGUF (vs 4.7 GB).

The shrink-search then walked the ladder down and found the floor (270 grounded eval pairs, hybrid retrieval, recall@5 = 0.98 throughout):

| Base | Fine-tune + RAG | Base + RAG | Verdict |
|---|---:|---:|---|
| Qwen2.5-3B | **3.80** | 3.65¹ | **smallest viable DSLM** |
| Qwen2.5-1.5B | 3.29 | 3.64 | fine-tune *hurts* |
| Qwen2.5-0.5B | 2.18 | 2.72 | both arms collapse |

¹ 40 of 270 pairs judged (rest hit API errors); full re-run queued.

Fine-tuning pays at 3B and subtracts below it — and no amount of retrieval rescues a 0.5B reader. Full experiment record: [docs/shrink-search-report.md](docs/shrink-search-report.md).

> The product stays **100 % local**: Local Chat and the exported GGUF never call a cloud API. The Anthropic API is used only in the offline build steps (synthetic-data generation and the eval judge) — like a compiler, not a runtime dependency.

---

## Repository layout

Monorepo: pnpm workspaces + Cargo workspace + uv-managed Python. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full breakdown.

```
apps/desktop/        Tauri v2 + React + TypeScript shell
crates/orchestrator/ Rust runtime: process mgmt, project state, IPC
crates/agent/        Rust agent loop: provider abstraction + tool dispatch
workers/py/          Python ML workers: ingestion, rag, inference, export (CPU-torch env)
workers/py-training/ WSL2 training env: Unsloth QLoRA (CUDA torch, standalone uv project)
packages/sdk-js/     @narrowmind/client — npm SDK (placeholder, lands in Phase 6)
examples/            Integration templates: nextjs-chat, express-api (planned, Phase 6)
docs/                ARCHITECTURE, ROADMAP, dev-setup, testing, datasets,
                     quantization, shrink-search-report, evals/ (primary eval artifacts)
```

---

## Development setup

Prerequisites: **Rust 1.80+**, **Node 20+**, **pnpm 11+**, **Python 3.11+**, **uv 0.4+**. On Windows, fine-tuning additionally needs **WSL2** (Ubuntu) with the NVIDIA driver's CUDA passthrough — serving/RAG/eval run natively. Full walkthrough: [docs/dev-setup.md](docs/dev-setup.md).

### Windows

Install toolchains via winget:

```powershell
winget install Rustlang.Rustup OpenJS.NodeJS.LTS astral-sh.uv GitHub.cli
winget install Microsoft.WindowsSDK.10.0.22621
winget install Microsoft.VisualStudio.2022.BuildTools  # if not already present
corepack enable  # provides the pinned pnpm from package.json
```

Create a local `.nm-env.ps1` helper (gitignored; template in [docs/dev-setup.md](docs/dev-setup.md)) that sets up the MSVC build env, then dot-source it before running `cargo` or `pnpm tauri dev`:

```powershell
. .\.nm-env.ps1
```

The helper auto-detects MSVC via `vswhere` and falls back to the cross-compile `vcvarsx86_amd64.bat` when only the x86 host toolchain is installed.

### macOS / Linux

Install Rust via `rustup`, Node via your package manager, then:

```bash
corepack enable && corepack prepare pnpm@latest --activate
curl -LsSf https://astral.sh/uv/install.sh | sh
```

### First build

```bash
pnpm install
pnpm tauri dev
```

---

## Commands

```bash
# Tests
cargo test --workspace
uv run pytest

# Lint / format
cargo clippy --workspace --all-targets -- -D warnings -A clippy::pedantic
pnpm -r typecheck
uv run ruff check . && uv run ruff format --check .

# Release bundles
pnpm tauri build
```

---

## License

**Apache-2.0** — see [LICENSE](LICENSE).

---

## Contributing

PRs and issues are welcome. See [AGENTS.md](AGENTS.md) for the design constraints any contributor — human or LLM — must follow when touching this repo, and [docs/testing.md](docs/testing.md) for the test baseline a change must keep green.
