# Datasets — HuggingFace import (Phase 4.7)

NarrowMind builds a domain from two kinds of data:

- **SFT** — `question` / `answer` pairs that fine-tune the model (`datasets/sft.jsonl`
  + held-out `datasets/eval.jsonl`).
- **RAG corpus** — text chunked + embedded into the vector store for retrieval
  (`sources/<id>/chunks.jsonl` → `datasets/rag.jsonl` + `vector_store/`).

Historically both came from **synthetic generation** (`generate_sft` prompts the
provider; `ingest_source` scrapes local files / Wikipedia / URLs). Phase 4.7 adds two
paths to consume **ready-made HuggingFace datasets** directly — useful when a curated,
high-quality corpus already exists (e.g. the Stanford Encyclopedia of Philosophy).

> **Why this exists.** The Phase 4.6 eval showed a small *synthetic* fine-tune lost to
> base+RAG (judge 4.29 vs 4.46) because its answers were terse. Curated datasets with
> long, formal answers are the fix. See `docs/ROADMAP.md → Phase 4.7`.

Both paths run in the **CPU `py` worker env** and reuse the existing `datasets`
dependency — no new packages, and the `py` (Windows/CPU) vs `py-training` (WSL/CUDA)
split is unchanged. Public HF datasets load without auth.

---

## `import_sft_from_hf` — HF QA → `sft.jsonl`

Imports `question` / `answer` rows directly as SFT data, skipping synthesis. Multiple
sources are concatenated, then split into train/eval with the same reproducible seed
`generate_sft` uses (persisted to `project.toml [synth] split_seed`).

Per-source knobs:

| field | meaning |
|---|---|
| `repo_id` | HF dataset id (required) |
| `split` | dataset split (default `train`) |
| `question_col` / `answer_col` | column names (default `question` / `answer`) |
| `category_col` + `categories` | keep only these topics (case-insensitive) |
| `min_answer_chars` | drop short answers — guards thoroughness |
| `max_rows` | seeded (SHA-256-keyed, `PYTHONHASHSEED`-independent) per-source cap |

Top-level: `eval_split` (default 0.10), `seed`, `max_total` (cap on the combined pool).

Example (agent tool call):

```json
{
  "sources": [
    { "repo_id": "ruggsea/stanford-encyclopedia-of-philosophy_instruct",
      "max_rows": 2300, "min_answer_chars": 250 },
    { "repo_id": "sayhan/strix-philosophy-qa", "category_col": "category",
      "categories": ["abduction", "ethics", "logic"], "max_rows": 700, "min_answer_chars": 150 }
  ],
  "eval_split": 0.10
}
```

---

## `ingest_source type=hf_dataset` — HF text → RAG corpus

Fills the reserved `HF_DATASET` source type. Pulls one **text column** from a HF dataset
into the project as documents, then runs the **existing** cleaning/chunking pipeline
(chunk 512/64, language + quality filter, MinHash dedup) — only the document-fetch
front-end is new.

| field | meaning |
|---|---|
| `repo_id` / `text_column` | dataset id + the column to chunk (required) |
| `split` | default `train` |
| `category_column` + `categories` | keep only these topics (case-insensitive) |
| `max_rows` | cap on documents — an **even, order-spanning subsample** so every category survives the cap (no alphabetical bias) |

Example:

```json
{ "type": "hf_dataset",
  "repo_id": "AiresPucrs/stanford-encyclopedia-philosophy",
  "text_column": "text", "category_column": "category",
  "categories": ["abduction", "ethics", "logic"], "max_rows": 18000 }
```

After ingesting, run `build_dataset` + `rag.embed_chunks` as usual to populate the
vector store.

---

## The SEP philosophy datasets

All three derive from the Stanford Encyclopedia of Philosophy and share a `category`
field, so SFT and RAG can be matched by topic:

| dataset | role | rows | key columns |
|---|---|---|---|
| `ruggsea/stanford-encyclopedia-of-philosophy_instruct` | SFT (quality) | 11.9K | question, answer |
| `sayhan/strix-philosophy-qa` | SFT (breadth) | 134K | category, question, answer |
| `AiresPucrs/stanford-encyclopedia-philosophy` | RAG corpus | 182.5K | text, category, metadata(URL) |

**Licensing.** ruggsea / strix are untagged (SEP-derived); AiresPucrs is `other`. Fine
for **local dev / research**; do **not** redistribute the imported data, and review SEP's
terms before any production / public use.

---

## End-to-end: build a SEP domain (Phase 4.7 slice C)

1. Create a project (e.g. `felsefe-sep`), base `unsloth/Qwen2.5-7B-Instruct`.
2. Pick a category set (drives both the ingest filter and the strix filter).
3. `ingest_source type=hf_dataset` AiresPucrs (`text_column="text"`, categories,
   `max_rows≈15–20K`) → `build_dataset` → `rag.embed_chunks`.
4. `import_sft_from_hf` ruggsea + category-filtered strix (~3K combined pairs).
5. Train (QLoRA, rtx-3070-8gb preset) → `export_domain_gguf q4_k_m` → serve.
6. `run_eval` and compare fine-tune+RAG judge against base+RAG's **4.46**.

The eval judge score is the acceptance gate — the single number that says whether the
bigger, more thorough dataset actually improved the model.
