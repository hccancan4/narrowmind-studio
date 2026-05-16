# AGENTS.md

This file is read by coding agents (Claude Code, opencode, others) working in this repository.

## What This Project Is

NarrowMind Studio is an open-source desktop IDE for building Domain-Specific Language Models (DSLMs). It orchestrates open-source ML tooling behind a Claude Code–style agent loop.

**Read first, always:**
- `docs/ARCHITECTURE.md` — system design, repo layout, component responsibilities
- `docs/ROADMAP.md` — current phase, deliverables, acceptance criteria

Do not start work on a feature before locating it on the roadmap and confirming the prior phase's acceptance criteria are satisfied. If a request falls outside the current phase, ask the human reviewer (Hasancan) before proceeding.

---

## Reference Hardware

The reference development machine is **RTX 3070 (8 GB VRAM) + 32 GB RAM**.

Every default config and every preset shipped in the codebase **must** be feasible on that profile. If a feature requires more, gate it behind a hardware-profile check and provide a graceful message. Hardware profiles live under `crates/orchestrator/src/hardware/`.

---

## Tech Stack (Authoritative)

- **Desktop shell**: Tauri v2, React, TypeScript (strict), Tailwind, shadcn/ui, xterm.js, Recharts, TanStack Virtual
- **Orchestrator**: Rust 2021, Tokio, serde, thiserror, `keyring` crate for secrets
- **Agent**: Rust, in `crates/agent`. `Provider` trait + adapters; Anthropic is the v1 default.
- **Python workers**: Python 3.11+, `uv` for env management, JSON-RPC 2.0 over stdio
- **ML libraries**: Unsloth (default training), Axolotl (alt), llama.cpp / llama-cpp-python (inference + export), LlamaIndex + LanceDB (RAG), sentence-transformers (embeddings), datasets / PyMuPDF / trafilatura / wikipedia-api / EbookLib (ingestion), lm-evaluation-harness (eval)
- **SDK**: TypeScript, dual ESM/CJS, published as `@narrowmind/client`
- **Build/CI**: pnpm workspaces, Cargo workspace, uv, GitHub Actions

Do not introduce a new top-level dependency category without updating `ARCHITECTURE.md` first.

---

## Coding Conventions

### Rust

- Edition 2021. `clippy::pedantic` clean (with documented `#[allow(...)]` where pedantic is wrong).
- No `.unwrap()` or `.expect()` in non-test code paths. Use `thiserror` for typed errors; `anyhow` only at binary edges.
- Async via Tokio throughout. No blocking I/O on the runtime.
- Public APIs documented with `///` doc comments.

### TypeScript

- `strict: true`. No `any` without an inline `// reason:` comment.
- React function components only, hooks for state. No class components.
- Co-locate component, styles, and test: `Foo.tsx`, `Foo.test.tsx`.
- IPC types shared with Rust via generated bindings (`tauri-specta` preferred; document the final choice in `ARCHITECTURE.md`).

### Python

- 3.11+. Format with `ruff format`, lint with `ruff check`. Type-hint everything public.
- Workers expose JSON-RPC methods. No shared mutable state between requests.
- No long-running async loops inside a single RPC handler — return promptly and stream progress via RPC notifications.

---

## Working Principles

1. **Test as you build.** Each phase adds unit tests (`cargo test`, `vitest`, `pytest -q`). Acceptance criteria in `ROADMAP.md` should map to integration tests where feasible.
2. **No premature complexity.** Resist adding abstractions before the second concrete use case appears.
3. **Filesystem is the source of truth.** Project state lives on disk in human-readable formats (TOML, JSONL, markdown). The UI is a view over files. If state isn't on disk, it doesn't exist.
4. **Workers are crash-isolated.** Never call ML code in-process with the orchestrator. If a worker hangs, the orchestrator must be able to kill and restart it without taking down the app.
5. **Never pass tensors across the worker/orchestrator boundary.** Always pass file paths and metadata.
6. **Secrets via keychain only.** API keys never touch project files, env files committed to the repo, or logs. Use the `keyring` crate.
7. **Streaming over polling.** Long operations (training, ingestion, inference) emit progress via RPC notifications.
8. **When stuck, stop and ask.** If the right call isn't covered by `ARCHITECTURE.md` or `ROADMAP.md`, ask Hasancan rather than guessing on a load-bearing design decision.

---

## Commands (finalize during Phase 0)

```bash
# Dev
pnpm install
pnpm tauri dev

# Tests
cargo test --workspace
pnpm -r test
uv run pytest

# Lint / format
cargo clippy --workspace -- -D warnings
pnpm -r typecheck && pnpm -r lint
uv run ruff check . && uv run ruff format --check .

# Build release bundles
pnpm tauri build
```

---

## Commit Conventions

Conventional Commits. Examples:

- `feat(agent): add OpenAI provider adapter`
- `fix(training): clamp learning rate in QLoRA preset for 8GB profile`
- `docs(arch): clarify worker process model`
- `chore(deps): bump tauri to 2.1.3`

Reference the phase number in the body when relevant: `Refs: ROADMAP Phase 4`.

---

## What NOT to Do

- Do **not** add cloud-only features in v1. The tool must work fully offline once base models are downloaded.
- Do **not** pass tensors across the worker/orchestrator boundary.
- Do **not** modify the project filesystem layout (`project.toml` schema, directory names) without updating `ARCHITECTURE.md` and adding a migration.
- Do **not** introduce a second UI state store. Project state = filesystem; transient UI state = local component state or a single Zustand store, max.
- Do **not** call out to external services for telemetry. Telemetry, if added, is opt-in and documented in `ARCHITECTURE.md`.
- Do **not** auto-merge dependency bumps. Major version bumps need human review.

---

## Human Reviewer

All meaningful design changes are reviewed by **Hasancan**. If a PR or commit modifies any of:

- `ARCHITECTURE.md`
- `ROADMAP.md`
- Public Rust traits in `crates/agent` or `crates/orchestrator`
- The RPC schema between orchestrator and workers
- The `project.toml` schema

then request explicit confirmation in chat before proceeding.
