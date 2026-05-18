# Testing — Baseline & Policy

This file is the single source of truth for what's tested and how. Keep the
baseline numbers fresh: bump them whenever you add or remove tests, and
record the date alongside so a stranger can tell at a glance whether the
file is current.

---

## Current baseline (2026-05-17, post Phase 3 + UI polish + synth_provider override)

| Runtime | Command | Result |
|---|---|---|
| **Rust unit** | `cargo test --workspace --lib` | **58 passed** (0 failed) — 8 in `narrowmind-agent`, 0 in `narrowmind-workers`, 50 in `narrowmind-orchestrator` |
| **Rust integration** | `cargo test --workspace --tests` | **+2 passed** in `crates/orchestrator/tests/` (hello round-trip) |
| **Python** | `uv --directory workers/py run pytest` | **69 passed** (0 failed) |
| **TypeScript** | `pnpm -r typecheck` | **2 workspaces clean** (`apps/desktop`, `packages/sdk-js`) |

Total: **129 tests** across three runtimes. Workspace clean, no warnings,
no flakes.

---

## Per-crate / per-package breakdown

### crates/orchestrator (50 unit + 2 integration)

| Module | What it covers |
|---|---|
| `project::config` | TOML round-trip, schema_version pin, name validation |
| `project::store` | create/get/list/delete/update lifecycle, status mutation |
| `inference::mod` | ModelSpec defaults, port selection, ensure_running idempotency |
| `tools::projects` | create_project / list_projects / project_status tool invokes |
| `tools::chunks` | listing, filtering, include/exclude |
| `tools::ingest` | local file ingest pipeline, source manifest |
| `tools::build_dataset` | rag.jsonl write, vector_store path |
| `tools::synth_gen` | parse_pairs (JSON + code fences), estimate_cost, **synth_model resolver** (3 tiers + edge cases) |
| `tools::eval` | recall@k math, judge prompt template |

### crates/agent (8 unit)

Provider abstraction, message types, tool dispatch core. Anthropic provider
tests use recorded fixtures (no live HTTP).

### workers/py (69)

| File | Count | Covers |
|---|---|---|
| `test_subprocess_round_trip` | 1 | JSON-RPC stdio framing end-to-end |
| `test_hello_handler` | small | smoke worker registration |
| `test_ingestion_*` | many | PDF / EPUB / DOCX / HTML / Wikipedia / HF datasets ingest paths |
| `test_cleaning_*` | several | quality filters, MinHash near-dup detection, language detection |
| `test_chunking_*` | several | sentence boundary, token-budgeted packing |
| `test_rag_index` | 10 | LanceDB upsert / count / query with the `_table_exists()` compat shim |
| `test_rag_embedder` | small | BGE-small lazy load |

### apps/desktop + packages/sdk-js

TypeScript type-check only (no runtime tests yet). React component tests
are a Phase 7 polish item.

---

## Test policy

### What MUST have a test

- Public Rust APIs in `narrowmind-orchestrator` and `narrowmind-agent`.
- JSON-RPC worker handlers in `workers/py/narrowmind_workers/**/rpc.py`.
- Filesystem-shape contracts (project.toml schema, chunks.jsonl rows).
- Any pure function with non-trivial branching (parsers, resolvers,
  cost estimators).
- Bug fixes — every fix lands with a regression test that fails before
  the fix and passes after. No exceptions.

### What does NOT need a test

- React component rendering (visual changes are caught by manual
  end-to-end smoke testing, see below).
- xterm.js wiring (DOM library; integration smoke covers it).
- Tauri command glue when it's a 1-line passthrough to a tested
  orchestrator function.

---

## Library version pinning + integration discipline

**Lesson from Phase 3 acceptance, bug #2** (lancedb 0.30+ `list_tables()`
silently returned a dataclass instead of `list[str]`, so the idiomatic
`name in db.list_tables()` evaluated to False, and every `upsert_chunks`
call fell through to `create_table` and lost its rows — `count_rows`
returned 0, `rag.query` returned `hits=0`, all unit tests still passed):

> Library version pinning + integration test discipline for any new
> third-party API surface — unit tests alone do not catch silent
> behavioral drift across library upgrades.

Concretely:

- **Pin transitive ML deps to compatible ranges** in `workers/py/pyproject.toml`.
  When a major version bump is forced, run the workers' integration test
  (`test_rag_index`'s full lifecycle, not just the happy-path schema test)
  before merging.
- **For any library call whose return type or semantics could change
  across minor versions**, write a probe test that exercises the
  actual contract you depend on. The unit test `count_rows == N after
  upserting N rows` would have caught the lancedb regression instantly;
  `create_table → list_tables → assert contains name` did not, because
  the bug lived in the False evaluation of `in`, not in the create call.
- **Manual end-to-end smoke remains required for every release** because
  no test matrix can cover GUI + native + GPU + remote API at once.

---

## Smoke test checklist

Run before every release tag, and after any change that touches inference,
ingestion, or the agent loop. Each step must produce the listed signal in
under the listed timeout, or the test fails and the cause is recorded
inline below.

| # | Step | Expected signal | Budget |
|---|---|---|---|
| a | `pnpm tauri dev` | NarrowMind window opens, banner shows "phase 1" | 30 s |
| b | Open Settings, paste Anthropic key, Save | `hasKey == true` tag flips to "stored" | 5 s |
| c | Select `test-philosophy` in left rail | Banner shows `project test-philosophy` | <1 s |
| d | Dataset Browser tab | 376 chunks listed, search box filters live | <1 s |
| e | Agent prompt: *"list chunks tagged with 'consciousness'"* | tool call → result table in terminal | 10 s |
| f | Agent prompt: *"start the inference server"* | log shows `assigned to device CUDA0` for every layer, port up | 60 s (first run downloads GGUF) |
| g | Click 💬 Local chat → ask "What is qualia?" | window opens, first token streams, citations appear | 5 s |
| h | Click a citation in the bubble | retrieved chunk text visible | <1 s |
| i | Agent prompt: *"stop the inference server"* | server exits cleanly, VRAM drops in nvidia-smi | 5 s |
| j | Agent prompt: *"run_eval"* (or run the eval script) | `evals/<run_id>.md` written, recall@5 + judge score logged | 5 min |

---

## How to refresh this file

When you add or remove tests, re-run the four baseline commands above and
update the table at the top. **Date the row** so future readers can
calibrate whether the numbers are still trustworthy. If a test starts
flaking, mark it explicitly here rather than silently `#[ignore]`ing it —
silent skipping is how regressions get back into shipped code.
