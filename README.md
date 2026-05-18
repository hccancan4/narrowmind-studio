# NarrowMind Studio

Open-source desktop IDE for building **Domain-Specific Language Models (DSLMs)** without writing training boilerplate. Orchestrates Unsloth, LlamaIndex, llama.cpp and HuggingFace behind a Claude Code–style agent loop. Goes from *"I have some PDFs and a domain idea"* to *"I have a deployed Ollama model serving my website"* on a single consumer GPU.

**Reference hardware:** RTX 3070 (8 GB VRAM) + 32 GB RAM.

> Status: **Phase 0 — Repo Bootstrap.** Not yet usable. See [docs/ROADMAP.md](docs/ROADMAP.md) for the build plan.

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
- **Tier 1 — LoRA / QLoRA fine-tune.** Adapter training on a 7B-class base via Unsloth.
- **Tier 2 — Hybrid.** Fine-tuned base + RAG retrieval. The default production-grade recipe.

The filesystem is the source of truth. Project state lives under `~/narrowmind/projects/<name>/` as TOML + JSONL + markdown; the UI is a view over those files.

---

## Repository layout

Monorepo: pnpm workspaces + Cargo workspace + uv-managed Python. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full breakdown.

```
apps/desktop/        Tauri v2 + React + TypeScript shell
crates/orchestrator/ Rust runtime: process mgmt, project state, IPC
crates/agent/        Rust agent loop: provider abstraction + tool dispatch
workers/py/          Python ML workers: ingestion, training, inference, rag, eval, export
packages/sdk-js/     @narrowmind/client — npm SDK for embedding a DSLM in a website
examples/            Drop-in templates: nextjs-chat, express-api
docs/                ARCHITECTURE.md, ROADMAP.md
```

---

## Development setup

Prerequisites: **Rust 1.80+**, **Node 20+**, **pnpm 9+**, **Python 3.11+**, **uv 0.4+**.

### Windows

Install toolchains via winget:

```powershell
winget install Rustlang.Rustup pnpm.pnpm astral-sh.uv GitHub.cli
winget install Microsoft.WindowsSDK.10.0.22621
winget install Microsoft.VisualStudio.2022.BuildTools  # if not already present
```

A local helper `.nm-env.ps1` (gitignored) sets up the MSVC build env for the current PowerShell session. Dot-source it before running `cargo` or `pnpm tauri dev`:

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
pnpm -r test
uv run pytest

# Lint / format
cargo clippy --workspace -- -D warnings
pnpm -r typecheck && pnpm -r lint
uv run ruff check . && uv run ruff format --check .

# Release bundles
pnpm tauri build
```

---

## License

To be set in Phase 7 (target: **Apache-2.0**).

---

## Contributing

This is a personal R&D project in early bootstrap. PRs and issues are welcome once Phase 1 lands. For now, see [AGENTS.md](AGENTS.md) for the design constraints any coding agent — human or LLM — must follow when touching this repo.
