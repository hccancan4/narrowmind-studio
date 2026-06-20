//! `import_sft_from_hf` — import ready-made `HuggingFace` QA datasets directly as SFT data.
//!
//! Phase 5: the Phase 4.6 eval showed a small synthetic fine-tune doesn't beat base+RAG when
//! its answers are terse. This tool bypasses synthetic Q&A generation entirely and pulls
//! curated `question` / `answer` pairs straight from HF datasets (e.g. the SEP-derived
//! `ruggsea/...instruct` + category-filtered `sayhan/strix-philosophy-qa`), writing the same
//! `datasets/sft.jsonl` + `datasets/eval.jsonl` shape `generate_sft` produces. The reproducible
//! seeded split is shared with `generate_sft` (see [`super::synth_gen::split_pairs`]).
//!
//! Download + per-source filtering/sampling happens in the `narrowmind_workers.sft_import`
//! Python worker (uses the `datasets` lib, already a workers/py dep). The seed is resolved
//! here first and passed down so the worker's row sampling and the train/eval split agree.

use std::fs;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::info;

use super::context::ToolContext;
use super::registry::{Tool, ToolDef, ToolError, ToolResult};
use super::synth_gen::{resolve_seed, split_pairs, write_jsonl, QaPair};
use crate::error::WorkerError;
use crate::retry::{timeouts, with_backoff, BackoffPolicy, Retry};
use crate::worker::{call_worker, WorkerCommand};

const DEFAULT_EVAL_SPLIT: f32 = 0.10;

#[derive(Debug, Deserialize)]
struct ImportArgs {
    /// One or more HF QA datasets to pull + concatenate. At least one required.
    sources: Vec<HfSource>,
    /// Held-out eval fraction (0.0..=0.5). Default 0.10.
    #[serde(default = "default_eval_split")]
    eval_split: f32,
    /// Optional fixed seed. Omit to reuse the project seed (or generate + persist one).
    #[serde(default)]
    seed: Option<u64>,
    /// Optional cap on the *combined* pair count after concatenation (seeded sample).
    #[serde(default)]
    max_total: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct HfSource {
    /// HF dataset repo id, e.g. `ruggsea/stanford-encyclopedia-of-philosophy_instruct`.
    repo_id: String,
    #[serde(default = "default_split")]
    split: String,
    #[serde(default = "default_question_col")]
    question_col: String,
    #[serde(default = "default_answer_col")]
    answer_col: String,
    /// Column holding a topic label (e.g. strix's `category`). Required for `categories`.
    #[serde(default)]
    category_col: Option<String>,
    /// Keep only rows whose `category_col` value is in this list (case-insensitive).
    #[serde(default)]
    categories: Option<Vec<String>>,
    /// Drop rows whose answer is shorter than this (chars). Guards answer thoroughness.
    #[serde(default)]
    min_answer_chars: Option<u32>,
    /// Cap rows kept from THIS source (seeded sample). Omit to keep all that pass filters.
    #[serde(default)]
    max_rows: Option<u32>,
}

fn default_eval_split() -> f32 {
    DEFAULT_EVAL_SPLIT
}
fn default_split() -> String {
    "train".into()
}
fn default_question_col() -> String {
    "question".into()
}
fn default_answer_col() -> String {
    "answer".into()
}

pub struct ImportSftFromHf;

#[async_trait]
impl Tool for ImportSftFromHf {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "import_sft_from_hf".into(),
            description: "Import ready-made supervised fine-tuning Q&A pairs directly from one or \
                          more HuggingFace datasets (columns question/answer), bypassing synthetic \
                          generation. Per source: optional category filter, minimum answer length, \
                          and a seeded row cap. Concatenates all sources, then writes \
                          datasets/sft.jsonl + datasets/eval.jsonl with the same reproducible \
                          seeded split as generate_sft. Use for curated corpora (e.g. SEP-derived \
                          philosophy QA). Public HF datasets load without auth."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "sources": {
                        "type": "array",
                        "minItems": 1,
                        "description": "HF QA datasets to import + concatenate.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "repo_id":      { "type": "string", "description": "HF dataset id, e.g. 'ruggsea/stanford-encyclopedia-of-philosophy_instruct'." },
                                "split":        { "type": "string", "default": "train" },
                                "question_col": { "type": "string", "default": "question" },
                                "answer_col":   { "type": "string", "default": "answer" },
                                "category_col": { "type": "string", "description": "Topic column (e.g. strix's 'category'). Required for `categories`." },
                                "categories":   { "type": "array", "items": { "type": "string" }, "description": "Keep only these categories (case-insensitive)." },
                                "min_answer_chars": { "type": "integer", "minimum": 0, "description": "Drop rows with shorter answers." },
                                "max_rows":     { "type": "integer", "minimum": 1, "description": "Seeded cap on rows kept from this source." }
                            },
                            "required": ["repo_id"]
                        }
                    },
                    "eval_split": { "type": "number", "minimum": 0.0, "maximum": 0.5, "default": 0.10 },
                    "seed":       { "type": "integer", "description": "Override the project's split seed." },
                    "max_total":  { "type": "integer", "minimum": 1, "description": "Seeded cap on the combined pair count." }
                },
                "required": ["sources"]
            }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, args: Value) -> Result<ToolResult, ToolError> {
        let args: ImportArgs = serde_json::from_value(args).map_err(|e| ToolError::BadInput {
            tool: "import_sft_from_hf".into(),
            reason: e.to_string(),
        })?;
        if args.sources.is_empty() {
            return Err(ToolError::BadInput {
                tool: "import_sft_from_hf".into(),
                reason: "at least one source is required".into(),
            });
        }
        if !(0.0..=0.5).contains(&args.eval_split) {
            return Err(ToolError::BadInput {
                tool: "import_sft_from_hf".into(),
                reason: format!("eval_split must be in [0.0, 0.5], got {}", args.eval_split),
            });
        }

        let project = ctx.current_project().await.ok_or(ToolError::NoProject)?;
        let runner = ctx
            .python_runner
            .as_ref()
            .ok_or_else(|| ToolError::Exec {
                tool: "import_sft_from_hf".into(),
                message: "PythonRunner not configured on ToolContext".into(),
            })?
            .clone();

        // Resolve / persist the split seed BEFORE the download so the per-source row sampling
        // (in the worker) and the train/eval split (here) share one reproducible seed.
        let seed = resolve_seed(ctx, &project.name, args.seed)?;

        let sources_json = serde_json::to_value(&args.sources).map_err(|e| ToolError::Exec {
            tool: "import_sft_from_hf".into(),
            message: format!("serialize sources: {e}"),
        })?;
        let cmd = WorkerCommand {
            module: "narrowmind_workers.sft_import".into(),
            method: "sft_import.import_hf_qa".into(),
            params: json!({
                "sources": sources_json,
                "seed": seed,
                "max_total": args.max_total,
            }),
            timeout: Some(timeouts::INGEST),
        };

        // Pulling a HF dataset is idempotent (cached after the first fetch), so one retry on a
        // transport-level failure is safe; bad repo ids / column names are permanent.
        let value = with_backoff(&BackoffPolicy::single_retry(), "import_sft_from_hf", |_| {
            let runner = runner.clone();
            let cmd = cmd.clone();
            async move {
                call_worker(&runner, &cmd).await.map_err(|e| match e {
                    e @ (WorkerError::Spawn { .. }
                    | WorkerError::Io(_)
                    | WorkerError::EarlyExit { .. }) => Retry::Transient(e),
                    e => Retry::Permanent(e),
                })
            }
        })
        .await
        .map_err(|e| ToolError::Exec {
            tool: "import_sft_from_hf".into(),
            message: e.to_string(),
        })?;

        // The worker returns { pairs: [{question, answer, source_chunk_id}], meta: {...} }.
        let pairs: Vec<QaPair> = match value.get("pairs") {
            Some(v) => serde_json::from_value(v.clone()).map_err(|e| ToolError::Exec {
                tool: "import_sft_from_hf".into(),
                message: format!("decode worker pairs: {e}"),
            })?,
            None => Vec::new(),
        };
        if pairs.is_empty() {
            return Err(ToolError::Exec {
                tool: "import_sft_from_hf".into(),
                message: "no pairs imported — check repo ids, column names, and filters".into(),
            });
        }

        let (train_pairs, eval_pairs) = split_pairs(pairs, args.eval_split, seed);
        let datasets_dir = project.root.join("datasets");
        fs::create_dir_all(&datasets_dir).map_err(ToolError::Io)?;
        write_jsonl(&datasets_dir.join("sft.jsonl"), &train_pairs)?;
        write_jsonl(&datasets_dir.join("eval.jsonl"), &eval_pairs)?;

        let meta = value.get("meta").cloned().unwrap_or(Value::Null);
        let msg = format!(
            "import_sft_from_hf done: {} train pairs, {} eval pairs from {} source(s) (seed={seed})",
            train_pairs.len(),
            eval_pairs.len(),
            args.sources.len(),
        );
        info!("{msg}");
        Ok(ToolResult::text(msg).with_structured(json!({
            "train_pairs": train_pairs.len(),
            "eval_pairs":  eval_pairs.len(),
            "seed":        seed,
            "sources":     meta,
            "sft_path":    datasets_dir.join("sft.jsonl"),
            "eval_path":   datasets_dir.join("eval.jsonl"),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_apply_defaults() {
        let args: ImportArgs = serde_json::from_value(json!({
            "sources": [{ "repo_id": "ruggsea/x" }]
        }))
        .unwrap();
        assert_eq!(args.sources.len(), 1);
        assert_eq!(args.sources[0].split, "train");
        assert_eq!(args.sources[0].question_col, "question");
        assert_eq!(args.sources[0].answer_col, "answer");
        assert!((args.eval_split - 0.10).abs() < f32::EPSILON);
        assert!(args.seed.is_none());
        assert!(args.max_total.is_none());
    }

    #[test]
    fn args_accept_full_source_spec() {
        let args: ImportArgs = serde_json::from_value(json!({
            "sources": [{
                "repo_id": "sayhan/strix-philosophy-qa",
                "category_col": "category",
                "categories": ["abduction", "ethics"],
                "min_answer_chars": 150,
                "max_rows": 700
            }],
            "eval_split": 0.2,
            "seed": 42,
            "max_total": 3000
        }))
        .unwrap();
        let s = &args.sources[0];
        assert_eq!(s.category_col.as_deref(), Some("category"));
        assert_eq!(s.categories.as_ref().unwrap().len(), 2);
        assert_eq!(s.min_answer_chars, Some(150));
        assert_eq!(s.max_rows, Some(700));
        assert_eq!(args.seed, Some(42));
        assert_eq!(args.max_total, Some(3000));
    }

    #[test]
    fn def_has_expected_name_and_requires_sources() {
        let def = ImportSftFromHf.def();
        assert_eq!(def.name, "import_sft_from_hf");
        let req = def.input_schema["required"].as_array().unwrap();
        assert!(req.iter().any(|v| v == "sources"));
    }
}
