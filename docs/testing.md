# Testing — Baseline & Policy

This file is the single source of truth for what's tested and how. Keep the
baseline numbers fresh: bump them whenever you add or remove tests, and
record the date alongside so a stranger can tell at a glance whether the
file is current.

---

## Current baseline (2026-06-11, post architecture-review hardening)

| Runtime | Command | Result |
|---|---|---|
| **Rust unit** | `cargo test --workspace --lib` | **68 passed** (0 failed) — 9 in `narrowmind-agent` (+1 TokenUsage accounting), 0 in `narrowmind-workers`, 59 in `narrowmind-orchestrator` (+5 retry/backoff) |
| **Rust integration** | `cargo test --workspace --tests` | **+11 passed** — 2 hello round-trip + **9 WorkerPool probes** (process reuse, 8-way concurrent serialization, in-flight timeout-kill-respawn, queued-timeout-spares-worker, external-kill recovery, crash-retry bounds, 1 MB stderr flood, shutdown reaping) |
| **Python** | `uv --directory workers/py run pytest` | **86 passed** (0 failed) — +7 dependency contract probes, +2 long-lived stream serving (in-order ids, multi-byte UTF-8 under reuse) |
| **TypeScript** | `pnpm -r typecheck` | **2 workspaces clean** (`apps/desktop`, `packages/sdk-js`) |

Total: **165 tests** across three runtimes. Workspace clean, no warnings,
no flakes.

### Earlier baseline (2026-05-18, post Phase 3.5 retrieval polish)

| Runtime | Command | Result |
|---|---|---|
| Rust unit | `cargo test --workspace --lib` | 62 passed (8 agent + 54 orchestrator) |
| Rust integration | `cargo test --workspace --tests` | +2 passed |
| Python | `uv ... pytest` | 77 passed |
| TypeScript | `pnpm -r typecheck` | 2 workspaces clean |
| Total | | 141 tests, 0 failed |

### Earlier baseline (2026-05-17, post Phase 3 + UI polish + synth_provider override)

| Runtime | Command | Result |
|---|---|---|
| Rust unit | `cargo test --workspace --lib` | 58 passed (8 agent + 0 workers + 50 orchestrator) |
| Rust integration | `cargo test --workspace --tests` | +2 passed |
| Python | `uv ... pytest` | 69 passed |
| TypeScript | `pnpm -r typecheck` | 2 workspaces clean |
| Total | | 129 tests, 0 failed |

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

## Smoke test history

A short ledger of when the full 10-step smoke last ran cleanly. Each
entry should link to the run_eval markdown report it produced, since
that's the only step with non-binary output worth versioning.

### 2026-05-18 (PM) — Phase 3.5 acceptance, multi-config retrieval

Run on commit `3a9560b` (post Phase 3.5 hybrid retrieval + 56-pair eval).
Build target: Tauri dev / debug, Windows 11, RTX 3070 + 32 GB RAM.

Eval set: **56 pairs** on `deneme1-faz2` (46 reshuffled from sft+eval at
seed `7340118592873561`, plus 10 proper-noun-targeted pairs generated
via Haiku — exactly the failure class Phase 3 missed).

Multi-config run via one agent prompt sweeping `mode ∈ {dense, sparse,
hybrid}`. Aggregated comparison archived at
`<project>/evals/2026-05-18-multiconfig.md`.

| metric | dense | sparse | **hybrid** | Phase 3 baseline (19 pairs) |
|---|---:|---:|---:|---:|
| recall@5    | 0.89 | 0.98 | **0.98** | 0.79 |
| judge mean  | 4.23 | 4.43 | **4.55** | 3.37 |
| score=1     | 3 | 1 | **0** | 4 |

Phase 4 gating thresholds (recall@5 ≥ 0.85 AND judge ≥ 3.8): **PASS**.
Hybrid vs dense delta: 5 pairs flipped MISS→HIT (including all three
proper-noun-rescue targets: C. D. Broad 1930, Frank Jackson 1982, Anne
Conway/Leibniz), 0 pairs lost recall, 12 judge scores up, 4 down. Zero
hybrid pairs scored at judge≤2.

This is the run that unblocks Phase 4 (LoRA fine-tune).

### 2026-05-17 — pre Phase 4 consolidation (superseded by the 3.5 run above)

Run on commit `2090cfb` (post Phase 3 + UI polish + synth_provider).
Build target: Tauri dev / debug profile, Windows 11, RTX 3070 + 32 GB RAM.

| # | Step | Outcome |
|---|---|---|
| a | `pnpm tauri dev` | ✓ window opened |
| b | Settings → API key already stored from prior session | ✓ hasKey == true |
| c | Select `deneme1-faz2` in left rail | ✓ banner showed project name |
| d | Dataset Browser tab | ✓ 376 / 376 chunks listed |
| e | (covered by `f` below — agent tool dispatch path is the same) | ✓ |
| f | Agent prompt `run_eval` → start_inference_server fired implicitly | ✓ all 29 layers `assigned to device CUDA0` |
| g | 💬 Local chat (validated independently on commit `fee2ed6`) | ✓ previously validated |
| h | Citations expand-on-click (validated on `fee2ed6`) | ✓ previously validated |
| i | stop_inference_server (idle TTL watchdog covers this; explicit stop validated on prior sessions) | ✓ previously validated |
| j | run_eval — see report below | ✓ wrote `evals/1bab695ad8a646c29fa7e72196a65c52.md` |

run_eval aggregate on the Phase 3 dogfood dataset (19 hand-curated
philosophy-of-mind Q&A pairs, Qwen2.5-7B-Instruct Q4_K_M + BGE-small +
LanceDB, top_k=5):

| metric | value | threshold | verdict |
|---|---|---|---|
| retrieval recall@5 | **0.79** (15/19) | ≥ 0.85 to ship | below ship bar, above revise bar |
| LLM judge mean | **3.37 / 5** | ≥ 4.0 to ship, ≥ 3.5 to skip revise | **below revise bar — dataset/retrieval needs work before fine-tuning** |
| judge score = 1 | 4 pairs (all four are retrieval misses → "I don't know" — model behaves correctly by refusing to hallucinate, but the gold answers exist in the corpus, so the chunker / embedder is leaving them unreachable) |
| judge score = 5 | 5 pairs (clean retrieval + accurate answer + good grounding) |

The four recall failures (pairs 7, 11, 15, 19) all share a pattern:
the gold chunk discusses a specific named entity (Adam robot 2009 /
Doleantie movement / Iamblichus / Guerizoli's 2006 study) that didn't
surface in BGE-small's top-5 even though it exists in the corpus.
Hypotheses worth testing before Phase 4 fine-tuning:

- Re-chunk with smaller chunk sizes and more overlap so a single chunk
  carries denser proper-noun signal.
- Try a stronger embedder (BGE-large or e5-large) to see if recall@5
  reaches 0.9+ on the same eval set without other changes.
- Add a sparse-vector / BM25 sidecar so exact name matches don't lose
  to dense semantic matches.

The 5 pairs scoring 3 share a different pattern: retrieval was fine,
but the model paraphrased or added plausible-sounding details not in
the gold answer (pair 1 inverted Turing test → AI tradition mapping;
pair 3 fabricated hostility motivation; pair 12 muddled Searle's
position). These are exactly the failure mode that LoRA fine-tuning
on the SFT split is designed to fix, so the Phase 4 plan isn't
invalid — but it should follow, not precede, the retrieval fixes.

---

## How to refresh this file

When you add or remove tests, re-run the four baseline commands above and
update the table at the top. **Date the row** so future readers can
calibrate whether the numbers are still trustworthy. If a test starts
flaking, mark it explicitly here rather than silently `#[ignore]`ing it —
silent skipping is how regressions get back into shipped code.
