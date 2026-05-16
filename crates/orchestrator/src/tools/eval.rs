//! `run_eval` — measure how well the project's RAG + DSLM answer the held-out eval set.
//!
//! Two metrics per Phase 3 acceptance:
//! - **`retrieval_recall@k`**: for each eval pair (Q, A, `source_chunk_id`), does the rag
//!   worker's top-k include the `source_chunk_id` the synth-gen pass tagged?
//! - **`answer_relevance`**: the rag-assembled answer is scored 1-5 by the configured
//!   Anthropic provider against the gold answer.
//!
//! Writes a per-run markdown report to `evals/<run_id>.md`.

use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::time::Duration;

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

        let mut handles = Vec::with_capacity(total);
        for (i, pair) in pairs.into_iter().enumerate() {
            let sem = sem.clone();
            let judge = judge.clone();
            let project_root = project_root.clone();
            let endpoint = endpoint_for_tasks.clone();
            let runner = runner.clone();
            let store = store.clone();
            let selected = selected.clone();
            let top_k = args.top_k;
            let max_tokens = args.max_tokens;
            let temperature = args.temperature;
            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire_owned().await;
                let (sink, _drain) = tokio::sync::mpsc::unbounded_channel();
                let task_ctx = ToolContext::new(selected, store, sink).with_python_runner(runner);
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

        let aggregate = aggregate_metrics(&results);
        let run_id = Uuid::new_v4().simple().to_string();
        let report_path = project.root.join("evals").join(format!("{run_id}.md"));
        fs::create_dir_all(report_path.parent().expect("evals parent"))?;
        let report = render_report(&project.name, &status.repo_id.unwrap_or_default(), &aggregate, &results);
        fs::write(&report_path, &report)?;
        info!(run_id, total, recall = aggregate.recall_at_k, judge = aggregate.judge_mean, "run_eval");

        Ok(ToolResult::text(format!(
            "eval done: {} pairs, recall@{}={:.2}, judge_mean={:.2}/5 → {}",
            total,
            args.top_k,
            aggregate.recall_at_k,
            aggregate.judge_mean,
            report_path.display()
        ))
        .with_structured(json!({
            "run_id": run_id,
            "pairs": total,
            "top_k": args.top_k,
            "recall_at_k": aggregate.recall_at_k,
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
    let hits = match retrieve(ctx, project_root, &pair.question, top_k, None).await {
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
    recall_at_k: f64,
    judge_mean: f64,
    judge_distribution: [usize; 6], // index 1..=5; index 0 unused (none/error)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "aggregation only — eval pair counts and scores are small ints, mantissa loss harmless"
)]
fn aggregate_metrics(results: &[PerPairResult]) -> Aggregate {
    let total = results.len() as f64;
    let recall_hits = results.iter().filter(|r| r.recall_hit).count() as f64;
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
        recall_at_k: if total == 0.0 { 0.0 } else { recall_hits / total },
        judge_mean,
        judge_distribution: dist,
    }
}

fn render_report(
    project_name: &str,
    model: &str,
    agg: &Aggregate,
    results: &[PerPairResult],
) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# NarrowMind RAG eval");
    let _ = writeln!(s);
    let _ = writeln!(s, "- project: `{project_name}`");
    let _ = writeln!(s, "- model: `{model}`");
    let _ = writeln!(s, "- timestamp: {}", Utc::now().to_rfc3339());
    let _ = writeln!(s, "- eval pairs: {}", results.len());
    let _ = writeln!(s);
    let _ = writeln!(s, "## Aggregate");
    let _ = writeln!(s);
    let _ = writeln!(s, "| metric | value |");
    let _ = writeln!(s, "|---|---|");
    let _ = writeln!(s, "| retrieval recall@k | **{:.2}** |", agg.recall_at_k);
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

// Reserved for future tunables. Kept here so dead-code lint stays quiet.
#[allow(dead_code)]
const _DUR_MARKER: Duration = Duration::from_secs(0);
