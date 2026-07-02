//! `run_eval` — measure how well the project's RAG + DSLM answer the held-out eval set.
//!
//! Two metrics per Phase 3 acceptance:
//! - **`retrieval_recall@k`**: for each eval pair (Q, A, `source_chunk_id`), does the rag
//!   worker's top-k include the `source_chunk_id` the synth-gen pass tagged? Only computed
//!   over *chunk-grounded* pairs — those whose `source_chunk_id` actually exists in the RAG
//!   corpus. SFT imported from a HF QA dataset (`import_sft_from_hf`) references a dataset
//!   row, not a chunk, so recall is reported as **N/A** there and the judge score stands alone.
//! - **`answer_relevance`**: the rag-assembled answer is scored 1-5 by the configured
//!   Anthropic provider against the gold answer.
//!
//! Writes a per-run markdown report to `evals/<run_id>.md`.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::StreamExt;
use narrowmind_agent::{AnthropicProvider, Message, Provider, ProviderEvent, StopReason};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Semaphore;
use tracing::{info, warn};
use uuid::Uuid;

use super::context::ToolContext;
use super::rag::{assemble_prompt, chat_completion, retrieve};
use super::registry::{Tool, ToolDef, ToolError, ToolResult};
use crate::secrets::SecretStore;

const JUDGE_PROMPT_TEMPLATE: &str = include_str!("eval_prompts/judge.md");

const DEFAULT_TOP_K: u32 = 5;
const DEFAULT_PARALLELISM: usize = 4;
const DEFAULT_MAX_TOKENS: u32 = 1024;
const DEFAULT_TEMPERATURE: f32 = 0.3;

#[derive(Debug, Deserialize)]
struct RunEvalArgs {
    /// Cap the number of eval pairs to score. Omit to use everything in eval.jsonl.
    #[serde(default)]
    limit: Option<usize>,
    /// Retrieval top-k for the recall metric.
    #[serde(default = "default_top_k")]
    top_k: u32,
    /// Parallel pair count (chat + judge).
    #[serde(default = "default_parallelism")]
    parallelism: usize,
    /// DSLM answer-generation parameters.
    #[serde(default = "default_max_tokens")]
    max_tokens: u32,
    #[serde(default = "default_temperature")]
    temperature: f32,
    /// Override the project's `[rag] retrieval_mode`. Lets one prompt sweep
    /// `dense | sparse | hybrid` for A/B comparison without editing project.toml.
    /// The mode tag is written into the markdown report header.
    #[serde(default)]
    mode: Option<String>,
}

fn default_top_k() -> u32 {
    DEFAULT_TOP_K
}
fn default_parallelism() -> usize {
    DEFAULT_PARALLELISM
}
fn default_max_tokens() -> u32 {
    DEFAULT_MAX_TOKENS
}
fn default_temperature() -> f32 {
    DEFAULT_TEMPERATURE
}

#[derive(Debug, Clone, Deserialize)]
struct EvalPair {
    question: String,
    answer: String,
    source_chunk_id: String,
}

#[derive(Debug, Clone, Serialize)]
struct PerPairResult {
    index: usize,
    question: String,
    gold: String,
    model_answer: String,
    source_chunk_id: String,
    retrieved_ids: Vec<String>,
    recall_hit: bool,
    judge_score: Option<u8>,
    judge_reason: Option<String>,
    error: Option<String>,
}

pub struct RunEval;

#[async_trait]
impl Tool for RunEval {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "run_eval".into(),
            description: "Score the project's RAG + DSLM against the held-out datasets/eval.jsonl. \
                          For each eval pair: retrieve top_k chunks (recall@k = is source_chunk_id \
                          in top_k), generate an answer via the running inference server, score \
                          relevance 1-5 with the configured Anthropic provider as judge. Writes \
                          evals/<run_id>.md and returns aggregate metrics."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "limit":       { "type": "integer", "minimum": 1, "maximum": 10000 },
                    "top_k":       { "type": "integer", "minimum": 1, "maximum": 20, "default": 5 },
                    "parallelism": { "type": "integer", "minimum": 1, "maximum": 16, "default": 4 },
                    "max_tokens":  { "type": "integer", "minimum": 32, "maximum": 4096, "default": 1024 },
                    "temperature": { "type": "number",  "minimum": 0.0, "maximum": 2.0, "default": 0.3 }
                }
            }),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one cohesive eval pipeline: load pairs → build providers → fan out → aggregate → report"
    )]
    async fn invoke(&self, ctx: &ToolContext, args: Value) -> Result<ToolResult, ToolError> {
        let args: RunEvalArgs = serde_json::from_value(args).map_err(|e| ToolError::BadInput {
            tool: "run_eval".into(),
            reason: e.to_string(),
        })?;
        let project = ctx.current_project().await.ok_or(ToolError::NoProject)?;
        let inference = ctx.inference.as_ref().ok_or_else(|| ToolError::Exec {
            tool: "run_eval".into(),
            message: "InferenceManager not configured on ToolContext".into(),
        })?;
        let status = inference.status().await;
        let endpoint = status.endpoint.ok_or_else(|| ToolError::Exec {
            tool: "run_eval".into(),
            message: "inference server not running — call start_inference_server first".into(),
        })?;

        // Resolve the effective retrieval mode for this run. The project's [rag]
        // section is the default; the `mode` arg overrides it. This is what makes
        // multi-config eval easy from one prompt: ask the agent to run_eval three
        // times with mode=dense, mode=sparse, mode=hybrid.
        let proj_cfg = ctx.project_store.get(&project.name).map_err(ToolError::Project)?;
        let rag_cfg = match args.mode.as_deref() {
            None => proj_cfg.rag.clone(),
            Some(m) => {
                use crate::project::RetrievalMode;
                let mut c = proj_cfg.rag.clone();
                c.retrieval_mode = match m {
                    "dense" => RetrievalMode::Dense,
                    "sparse" => RetrievalMode::Sparse,
                    "hybrid" => RetrievalMode::Hybrid,
                    other => {
                        return Err(ToolError::BadInput {
                            tool: "run_eval".into(),
                            reason: format!(
                                "unknown retrieval mode `{other}` (expected dense | sparse | hybrid)"
                            ),
                        });
                    }
                };
                c
            }
        };

        let eval_path = project.root.join("datasets").join("eval.jsonl");
        let pairs = load_pairs(&eval_path)?;
        if pairs.is_empty() {
            return Err(ToolError::Exec {
                tool: "run_eval".into(),
                message: format!("no pairs in {} — run generate_sft first", eval_path.display()),
            });
        }
        let pairs: Vec<EvalPair> = if let Some(n) = args.limit {
            pairs.into_iter().take(n).collect()
        } else {
            pairs
        };
        let total = pairs.len();

        // Build Anthropic judge provider once.
        let api_key = SecretStore::get_provider_key("anthropic")
            .map_err(|e| ToolError::Exec {
                tool: "run_eval".into(),
                message: format!("keychain: {e}"),
            })?
            .ok_or_else(|| ToolError::Exec {
                tool: "run_eval".into(),
                message: "Anthropic API key not set — needed for LLM-judge scoring".into(),
            })?;
        let judge: Arc<dyn Provider> = Arc::new(
            AnthropicProvider::builder()
                .api_key(api_key)
                .build()
                .map_err(|e| ToolError::Exec {
                    tool: "run_eval".into(),
                    message: format!("build judge provider: {e}"),
                })?,
        );

        let sem = Arc::new(Semaphore::new(args.parallelism.max(1)));
        let project_root = project.root.clone();
        let endpoint_for_tasks = endpoint.clone();
        let runner = ctx
            .python_runner
            .as_ref()
            .ok_or_else(|| ToolError::Exec {
                tool: "run_eval".into(),
                message: "PythonRunner not configured on ToolContext".into(),
            })?
            .clone();
        // Build a shadow context for each task that carries the python runner; we don't
        // need the agent's event sink for eval (it has its own report).
        let store = ctx.project_store.clone();
        let selected = ctx.selected.clone();
        // Share the outer context's WorkerPool across every task: all parallel
        // retrievals then hit ONE warm Python child (serialized ~50-300ms each)
        // instead of spawning `parallelism` cold processes each loading its own
        // copy of BGE-small.
        let pool = ctx.worker_pool.clone();

        let mut handles = Vec::with_capacity(total);
        for (i, pair) in pairs.into_iter().enumerate() {
            let sem = sem.clone();
            let judge = judge.clone();
            let project_root = project_root.clone();
            let endpoint = endpoint_for_tasks.clone();
            let runner = runner.clone();
            let store = store.clone();
            let selected = selected.clone();
            let pool = pool.clone();
            let top_k = args.top_k;
            let max_tokens = args.max_tokens;
            let temperature = args.temperature;
            let rag_cfg = rag_cfg.clone();
            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire_owned().await;
                let (sink, _drain) = tokio::sync::mpsc::unbounded_channel();
                let mut task_ctx =
                    ToolContext::new(selected, store, sink).with_python_runner(runner);
                if let Some(pool) = pool {
                    task_ctx = task_ctx.with_worker_pool(pool);
                }
                score_pair(
                    i,
                    pair,
                    &task_ctx,
                    &project_root,
                    &endpoint,
                    top_k,
                    max_tokens,
                    temperature,
                    &judge,
                    &rag_cfg,
                )
                .await
            }));
        }

        let mut results: Vec<PerPairResult> = Vec::with_capacity(total);
        for h in handles {
            match h.await {
                Ok(r) => results.push(r),
                Err(e) => warn!(error = %e, "eval task panicked"),
            }
        }
        results.sort_by_key(|r| r.index);

        let chunk_ids = load_chunk_ids(&project.root);
        let aggregate = aggregate_metrics(&results, &chunk_ids);
        let run_id = Uuid::new_v4().simple().to_string();
        // Tag the report filename with the retrieval mode so a sweep of three
        // configs produces three distinct, self-describing files instead of
        // three opaque UUIDs the user has to open one by one.
        let mode_tag = rag_cfg.retrieval_mode.as_str();
        let report_path = project
            .root
            .join("evals")
            .join(format!("{run_id}-{mode_tag}.md"));
        fs::create_dir_all(report_path.parent().expect("evals parent"))?;
        let report = render_report(
            &project.name,
            &status.repo_id.unwrap_or_default(),
            mode_tag,
            &aggregate,
            &results,
        );
        fs::write(&report_path, &report)?;
        info!(run_id, mode = mode_tag, total, recall = ?aggregate.recall_at_k, grounded = aggregate.grounded_pairs, judge = aggregate.judge_mean, "run_eval");

        // N/A when no eval pair is chunk-grounded (imported SFT) — distinct from a measured 0.00.
        let recall_str = aggregate
            .recall_at_k
            .map_or_else(|| "N/A".to_string(), |r| format!("{r:.2}"));
        Ok(ToolResult::text(format!(
            "eval done: {} pairs (mode={}), recall@{}={} ({} grounded), judge_mean={:.2}/5 → {}",
            total,
            mode_tag,
            args.top_k,
            recall_str,
            aggregate.grounded_pairs,
            aggregate.judge_mean,
            report_path.display()
        ))
        .with_structured(json!({
            "run_id": run_id,
            "mode": mode_tag,
            "pairs": total,
            "top_k": args.top_k,
            "recall_at_k": aggregate.recall_at_k,
            "grounded_pairs": aggregate.grounded_pairs,
            "judge_mean": aggregate.judge_mean,
            "judge_distribution": aggregate.judge_distribution,
            "report_path": report_path,
        })))
    }
}

#[allow(clippy::too_many_arguments)]
async fn score_pair(
    index: usize,
    pair: EvalPair,
    ctx: &ToolContext,
    project_root: &std::path::Path,
    endpoint: &str,
    top_k: u32,
    max_tokens: u32,
    temperature: f32,
    judge: &Arc<dyn Provider>,
    rag_cfg: &crate::project::RagConfig,
) -> PerPairResult {
    let mut out = PerPairResult {
        index,
        question: pair.question.clone(),
        gold: pair.answer.clone(),
        model_answer: String::new(),
        source_chunk_id: pair.source_chunk_id.clone(),
        retrieved_ids: vec![],
        recall_hit: false,
        judge_score: None,
        judge_reason: None,
        error: None,
    };

    // 1. Retrieve.
    let hits = match retrieve(ctx, project_root, &pair.question, top_k, None, rag_cfg).await {
        Ok(h) => h,
        Err(e) => {
            out.error = Some(format!("retrieve: {e}"));
            return out;
        }
    };
    out.retrieved_ids = hits.iter().map(|h| h.chunk_id.clone()).collect();
    out.recall_hit = out.retrieved_ids.contains(&pair.source_chunk_id);

    // 2. Generate answer via local DSLM.
    let prompt = assemble_prompt(&hits, &pair.question);
    let answer = match chat_completion(endpoint, &prompt, max_tokens, temperature).await {
        Ok(a) => a,
        Err(e) => {
            out.error = Some(format!("dslm chat: {e}"));
            return out;
        }
    };
    out.model_answer.clone_from(&answer);

    // 3. Judge.
    match judge_answer(judge, &pair.question, &pair.answer, &answer).await {
        Ok((score, reason)) => {
            out.judge_score = Some(score);
            out.judge_reason = Some(reason);
        }
        Err(e) => out.error = Some(format!("judge: {e}")),
    }
    out
}

async fn judge_answer(
    judge: &Arc<dyn Provider>,
    question: &str,
    gold: &str,
    model_answer: &str,
) -> Result<(u8, String), String> {
    let prompt_body = JUDGE_PROMPT_TEMPLATE
        .replace("{{QUESTION}}", question)
        .replace("{{GOLD_ANSWER}}", gold)
        .replace("{{MODEL_ANSWER}}", model_answer);
    let messages = vec![Message::user(prompt_body)];

    let mut stream = judge.stream(messages, &[]).await.map_err(|e| e.to_string())?;
    let mut text = String::new();
    while let Some(ev) = stream.next().await {
        match ev {
            ProviderEvent::Text(t) => text.push_str(&t),
            ProviderEvent::Stop { reason: StopReason::Error(e) } => {
                return Err(format!("judge stream error: {e}"));
            }
            ProviderEvent::Stop { .. } => break,
            _ => {}
        }
    }
    let parsed: JudgeResult = parse_judge(&text)?;
    if !(1..=5).contains(&parsed.score) {
        return Err(format!("judge returned score {} (expected 1-5)", parsed.score));
    }
    Ok((parsed.score, parsed.reason))
}

#[derive(Debug, Deserialize)]
struct JudgeResult {
    score: u8,
    #[serde(default)]
    reason: String,
}

fn parse_judge(text: &str) -> Result<JudgeResult, String> {
    let mut s = text.trim();
    if s.starts_with("```") {
        s = s.trim_start_matches("```json").trim_start_matches("```").trim();
        if let Some(idx) = s.rfind("```") {
            s = &s[..idx];
        }
    }
    serde_json::from_str(s.trim())
        .map_err(|e| format!("malformed judge JSON: {e}; raw: {}", &text[..text.len().min(200)]))
}

struct Aggregate {
    /// `None` = N/A: not a single eval pair is chunk-grounded (its `source_chunk_id`
    /// doesn't exist in the RAG corpus), so recall is unmeasurable. This is the normal
    /// case for SFT imported from a HF QA dataset — its pairs carry an external
    /// `hf:<repo>#<row>` reference, not a project chunk id. Judge score is the metric there.
    recall_at_k: Option<f64>,
    /// How many eval pairs were chunk-grounded — the denominator recall is computed over.
    grounded_pairs: usize,
    judge_mean: f64,
    judge_distribution: [usize; 6], // index 1..=5; index 0 unused (none/error)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "aggregation only — eval pair counts and scores are small ints, mantissa loss harmless"
)]
fn aggregate_metrics(results: &[PerPairResult], chunk_ids: &HashSet<String>) -> Aggregate {
    // Recall is only meaningful for pairs whose `source_chunk_id` is an actual chunk in the
    // RAG corpus (the synth-gen path tags each pair with the chunk it was generated from).
    // Imported-SFT pairs reference a HF dataset row, not a chunk, so they aren't groundable;
    // counting them as misses would report a misleading 0.00 instead of an honest N/A.
    let grounded: Vec<&PerPairResult> = results
        .iter()
        .filter(|r| chunk_ids.contains(&r.source_chunk_id))
        .collect();
    let recall_at_k = if grounded.is_empty() {
        None
    } else {
        let hits = grounded.iter().filter(|r| r.recall_hit).count() as f64;
        Some(hits / grounded.len() as f64)
    };

    let mut judge_total: u64 = 0;
    let mut judge_count: u64 = 0;
    let mut dist = [0usize; 6];
    for r in results {
        if let Some(score) = r.judge_score {
            judge_total += u64::from(score);
            judge_count += 1;
            if (1..=5).contains(&score) {
                dist[score as usize] += 1;
            }
        }
    }
    let judge_mean = if judge_count == 0 {
        0.0
    } else {
        judge_total as f64 / judge_count as f64
    };
    Aggregate {
        recall_at_k,
        grounded_pairs: grounded.len(),
        judge_mean,
        judge_distribution: dist,
    }
}

/// Load the chunk ids present in the project's RAG corpus, so [`aggregate_metrics`] can tell
/// whether each eval pair is chunk-grounded (recall measurable) or imported (recall N/A).
fn load_chunk_ids(project_root: &std::path::Path) -> HashSet<String> {
    let mut ids = HashSet::new();
    let rag = project_root.join("datasets").join("rag.jsonl");
    let Ok(file) = fs::File::open(&rag) else {
        return ids;
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if let Ok(v) = serde_json::from_str::<Value>(line.trim()) {
            if let Some(id) = v.get("chunk_id").and_then(Value::as_str) {
                ids.insert(id.to_string());
            }
        }
    }
    ids
}

fn render_report(
    project_name: &str,
    model: &str,
    retrieval_mode: &str,
    agg: &Aggregate,
    results: &[PerPairResult],
) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# NarrowMind RAG eval");
    let _ = writeln!(s);
    let _ = writeln!(s, "- project: `{project_name}`");
    let _ = writeln!(s, "- model: `{model}`");
    let _ = writeln!(s, "- retrieval mode: `{retrieval_mode}`");
    let _ = writeln!(s, "- timestamp: {}", Utc::now().to_rfc3339());
    let _ = writeln!(s, "- eval pairs: {}", results.len());
    let _ = writeln!(s);
    let _ = writeln!(s, "## Aggregate");
    let _ = writeln!(s);
    let _ = writeln!(s, "| metric | value |");
    let _ = writeln!(s, "|---|---|");
    match agg.recall_at_k {
        Some(r) => {
            let _ = writeln!(
                s,
                "| retrieval recall@k | **{r:.2}** (over {} chunk-grounded pairs) |",
                agg.grounded_pairs
            );
        }
        None => {
            let _ = writeln!(
                s,
                "| retrieval recall@k | **N/A** — no chunk-grounded eval pairs (e.g. imported SFT); judge is the metric |"
            );
        }
    }
    let _ = writeln!(s, "| LLM-judge mean | **{:.2} / 5** |", agg.judge_mean);
    for (i, n) in agg.judge_distribution.iter().enumerate().skip(1) {
        let _ = writeln!(s, "| judge score = {i} | {n} pairs |");
    }
    let _ = writeln!(s);
    let _ = writeln!(s, "## Per-pair");
    let _ = writeln!(s);
    let _ = writeln!(s, "| # | recall | score | question |");
    let _ = writeln!(s, "|---:|:---:|:---:|---|");
    for r in results {
        let recall_mark = if r.recall_hit { "✓" } else { "✗" };
        let score = r.judge_score.map_or_else(|| "-".to_string(), |s| s.to_string());
        let q: String = r.question.chars().take(80).collect();
        let _ = writeln!(s, "| {} | {recall_mark} | {score} | {q} |", r.index + 1);
    }
    let _ = writeln!(s);
    let _ = writeln!(s, "## Detail");
    for r in results {
        let _ = writeln!(s);
        let _ = writeln!(s, "### Pair {}", r.index + 1);
        let _ = writeln!(s, "- **question**: {}", r.question);
        let _ = writeln!(s, "- **gold**: {}", r.gold);
        let _ = writeln!(s, "- **model answer**:\n\n  {}\n", r.model_answer.replace('\n', "\n  "));
        let _ = writeln!(s, "- **expected source chunk**: `{}`", r.source_chunk_id);
        let _ = writeln!(s, "- **retrieved chunks**: {}", r.retrieved_ids.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(", "));
        let _ = writeln!(s, "- **recall hit**: {}", r.recall_hit);
        if let Some(score) = r.judge_score {
            let _ = writeln!(s, "- **judge score**: {score} / 5");
        }
        if let Some(reason) = &r.judge_reason {
            let _ = writeln!(s, "- **judge reason**: {reason}");
        }
        if let Some(error) = &r.error {
            let _ = writeln!(s, "- **error**: {error}");
        }
    }
    let _ = writeln!(s);
    s
}

fn load_pairs(path: &std::path::Path) -> Result<Vec<EvalPair>, ToolError> {
    if !path.is_file() {
        return Err(ToolError::Exec {
            tool: "run_eval".into(),
            message: format!("eval set missing at {}", path.display()),
        });
    }
    let file = fs::File::open(path)?;
    let mut out = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<EvalPair>(trimmed) {
            Ok(p) => out.push(p),
            Err(e) => warn!(error = %e, "skipping malformed eval line"),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(source_chunk_id: &str, recall_hit: bool, judge: Option<u8>) -> PerPairResult {
        PerPairResult {
            index: 0,
            question: "q".into(),
            gold: "g".into(),
            model_answer: "a".into(),
            source_chunk_id: source_chunk_id.into(),
            retrieved_ids: vec![],
            recall_hit,
            judge_score: judge,
            judge_reason: None,
            error: None,
        }
    }

    fn chunk_set(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn recall_is_na_when_no_pair_is_chunk_grounded() {
        // Imported-SFT pairs reference HF rows, not chunks → nothing grounds → N/A, not 0.00.
        let results = vec![
            pair("hf:repo#1", false, Some(2)),
            pair("hf:repo#2", false, Some(3)),
        ];
        let agg = aggregate_metrics(&results, &chunk_set(&["ck_a", "ck_b"]));
        assert_eq!(agg.recall_at_k, None);
        assert_eq!(agg.grounded_pairs, 0);
        // Judge is unaffected by grounding: (2+3)/2 = 2.5.
        assert!((agg.judge_mean - 2.5).abs() < f64::EPSILON);
    }

    #[test]
    fn recall_computed_only_over_grounded_pairs() {
        let results = vec![
            pair("ck_a", true, Some(5)),    // grounded + hit
            pair("ck_b", false, Some(4)),   // grounded + miss
            pair("hf:x#1", false, Some(1)), // ungrounded — excluded from recall, kept in judge
        ];
        let agg = aggregate_metrics(&results, &chunk_set(&["ck_a", "ck_b"]));
        assert_eq!(agg.grounded_pairs, 2);
        assert_eq!(agg.recall_at_k, Some(0.5)); // 1 hit / 2 grounded
        // Judge mean is over ALL pairs: (5+4+1)/3.
        assert!((agg.judge_mean - (10.0 / 3.0)).abs() < 1e-9);
    }

    #[test]
    fn recall_full_when_all_grounded_hit() {
        let results = vec![pair("ck_a", true, Some(5)), pair("ck_b", true, Some(5))];
        let agg = aggregate_metrics(&results, &chunk_set(&["ck_a", "ck_b"]));
        assert_eq!(agg.recall_at_k, Some(1.0));
        assert_eq!(agg.grounded_pairs, 2);
    }
}
