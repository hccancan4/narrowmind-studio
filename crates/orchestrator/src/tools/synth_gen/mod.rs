//! `generate_sft` — synthesize Q&A pairs from chunks via the configured provider.
//!
//! Phase 2 decision F summary:
//! - Provider reuse: spins up an `AnthropicProvider` from the project's provider config
//!   + the keychain-stored API key. No new transport abstraction.
//! - Concurrency: Tokio semaphore, default 4 parallel chunks.
//! - Retry: exponential backoff on 429 / 5xx (1s → 2s → 4s → 8s → 16s, max 5 attempts).
//! - Schema validation: model output is parsed as `Vec<QaPair>`; parse failures are
//!   logged as warnings and the chunk is skipped, the pipeline keeps running.
//! - Cost estimate: char-count ≈ token-count / 4 heuristic, logged before any HTTP call.
//! - Progress: stream `ToolEvent::Progress` messages per completed chunk so the UI sees
//!   live throughput.
//! - Output: `datasets/sft.jsonl` (train) + `datasets/eval.jsonl` (held-out, default 10%).
//!   Split is reproducible via a seed persisted to `project.toml [synth] split_seed`.

use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write as _IoWrite};
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use narrowmind_agent::{
    AnthropicProvider, Message, Provider, ProviderError, ProviderEvent, StopReason,
};
use rand::prelude::*;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Semaphore;
use tracing::{info, warn};

use super::context::{ToolContext, ToolEvent};
use super::registry::{Tool, ToolDef, ToolError, ToolResult};
use crate::project::SynthConfig;
use crate::secrets::SecretStore;

const PROMPT_TEMPLATE: &str = include_str!("prompts/sft_pair.md");

const DEFAULT_PARALLELISM: usize = 4;
const DEFAULT_EVAL_SPLIT: f32 = 0.10;
const DEFAULT_TARGET_PAIRS: u32 = 200;
const PROGRESS_EVERY_N: usize = 5;

// Char-to-token heuristic (decision F). Conservative on the high side so cost estimates
// don't under-budget.
const CHARS_PER_TOKEN: f64 = 4.0;
// USD / 1M tokens, picked to roughly match Claude Sonnet pricing. Exposed so the user can
// override per call as the catalogue shifts.
const DEFAULT_INPUT_USD_PER_MTOK: f64 = 3.0;
const DEFAULT_OUTPUT_USD_PER_MTOK: f64 = 15.0;

#[derive(Debug, Deserialize)]
struct GenerateSftArgs {
    /// Optional source filter. When omitted, chunks from every source in the project are used.
    #[serde(default)]
    source_id: Option<String>,
    /// Total Q&A pairs to *aim* for across train+eval. Hit-rate depends on how many
    /// chunks survive the include filter and parse cleanly; the tool may finish under.
    #[serde(default = "default_target_pairs")]
    target_pairs: u32,
    /// Fraction reserved for held-out eval (0.0..=0.5). Default 0.10.
    #[serde(default = "default_eval_split")]
    eval_split: f32,
    /// Parallel provider calls (default 4).
    #[serde(default = "default_parallelism")]
    parallelism: usize,
    /// Optional fixed seed. When omitted, the existing project seed is reused; if none
    /// has been set, a fresh one is generated and persisted to `project.toml [synth]`.
    #[serde(default)]
    seed: Option<u64>,
    /// Override the agent model. When omitted, the project's provider.model is used.
    #[serde(default)]
    model: Option<String>,
    /// Pricing overrides for the cost estimate. USD per 1M tokens.
    #[serde(default)]
    input_usd_per_mtok: Option<f64>,
    #[serde(default)]
    output_usd_per_mtok: Option<f64>,
}

fn default_target_pairs() -> u32 {
    DEFAULT_TARGET_PAIRS
}
fn default_eval_split() -> f32 {
    DEFAULT_EVAL_SPLIT
}
fn default_parallelism() -> usize {
    DEFAULT_PARALLELISM
}

/// One Q&A pair the model produces. The on-disk shape of `sft.jsonl` lines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QaPair {
    pub question: String,
    pub answer: String,
    /// Backreference for auditing; the model never sees this field.
    pub source_chunk_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChunkRecord {
    chunk_id: String,
    doc_id: String,
    source_id: String,
    text: String,
    token_count: u32,
    sentence_range: (u32, u32),
    #[serde(default = "default_true")]
    include: bool,
    #[serde(default)]
    metadata: Value,
}
fn default_true() -> bool {
    true
}

pub struct GenerateSft;

#[async_trait]
impl Tool for GenerateSft {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "generate_sft".into(),
            description: "Generate supervised fine-tuning Q&A pairs from the project's chunks by \
                          prompting the configured provider. Writes datasets/sft.jsonl + \
                          datasets/eval.jsonl with a reproducible seeded split. Honours chunk \
                          include flags. Logs a cost estimate before any HTTP call, streams \
                          progress per completed chunk, retries 429 / 5xx with exponential \
                          backoff, skips chunks whose response doesn't parse as the expected \
                          JSON array."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "source_id":    { "type": "string", "description": "Restrict to one source. Omit for all sources." },
                    "target_pairs": { "type": "integer", "minimum": 1, "maximum": 100_000, "default": 200 },
                    "eval_split":   { "type": "number", "minimum": 0.0, "maximum": 0.5, "default": 0.10 },
                    "parallelism":  { "type": "integer", "minimum": 1, "maximum": 16, "default": 4 },
                    "seed":         { "type": "integer", "description": "Override the project's synth seed." },
                    "model":        { "type": "string", "description": "Override the project's agent model." },
                    "input_usd_per_mtok":  { "type": "number" },
                    "output_usd_per_mtok": { "type": "number" }
                }
            }),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one cohesive workflow: parse args → load chunks → build provider → fan out → collect → split → write"
    )]
    async fn invoke(&self, ctx: &ToolContext, args: Value) -> Result<ToolResult, ToolError> {
        let args: GenerateSftArgs =
            serde_json::from_value(args).map_err(|e| ToolError::BadInput {
                tool: "generate_sft".into(),
                reason: e.to_string(),
            })?;
        if !(0.0..=0.5).contains(&args.eval_split) {
            return Err(ToolError::BadInput {
                tool: "generate_sft".into(),
                reason: format!("eval_split must be in [0.0, 0.5], got {}", args.eval_split),
            });
        }

        let project = ctx.current_project().await.ok_or(ToolError::NoProject)?;

        // Load chunks (include=true only) from one source or all sources.
        let chunks = load_included_chunks(&project.root, args.source_id.as_deref())?;
        if chunks.is_empty() {
            return Err(ToolError::Exec {
                tool: "generate_sft".into(),
                message: "no chunks found (or all chunks excluded). ingest a source first."
                    .into(),
            });
        }

        // Provider config from project, key from keychain.
        let cfg = ctx
            .project_store
            .get(&project.name)
            .map_err(ToolError::Project)?;
        let (model, model_source) = resolve_synth_model(args.model.as_deref(), &cfg.provider);
        if cfg.provider.name != "anthropic" {
            return Err(ToolError::Exec {
                tool: "generate_sft".into(),
                message: format!(
                    "only the `anthropic` provider supports generate_sft in v1.0; got `{}`",
                    cfg.provider.name
                ),
            });
        }
        let api_key = SecretStore::get_provider_key("anthropic")
            .map_err(|e| ToolError::Exec {
                tool: "generate_sft".into(),
                message: format!("read anthropic key from keychain: {e}"),
            })?
            .ok_or_else(|| ToolError::Exec {
                tool: "generate_sft".into(),
                message: "Anthropic API key not set — open Settings and paste it".into(),
            })?;
        let provider: Arc<dyn Provider> = Arc::new(
            AnthropicProvider::builder()
                .api_key(api_key)
                .model(&model)
                .build()
                .map_err(|e| ToolError::Exec {
                    tool: "generate_sft".into(),
                    message: format!("build provider: {e}"),
                })?,
        );

        // How many pairs per chunk to aim for. Clamp to [1, 5] so we don't ask one chunk
        // to produce 50 — quality collapses.
        let target_total = args.target_pairs as usize;
        let pairs_per_chunk = target_total.div_ceil(chunks.len()).clamp(1, 5);
        // Pick top-N chunks if asking for fewer pairs than chunks * 1.
        let chunks_needed = target_total.div_ceil(pairs_per_chunk).min(chunks.len());

        // Persist / reuse the split seed.
        let seed = resolve_seed(ctx, &project.name, args.seed)?;

        // Cost estimate.
        let input_rate = args.input_usd_per_mtok.unwrap_or(DEFAULT_INPUT_USD_PER_MTOK);
        let output_rate = args.output_usd_per_mtok.unwrap_or(DEFAULT_OUTPUT_USD_PER_MTOK);
        let (est_in_tokens, est_out_tokens, est_usd) = estimate_cost(
            &chunks[..chunks_needed],
            pairs_per_chunk,
            input_rate,
            output_rate,
        );
        let cost_msg = format!(
            "synth_gen cost estimate: ~{est_in_tokens} input tokens, ~{est_out_tokens} output tokens, ~${est_usd:.2} USD ({chunks_needed} chunks × {pairs_per_chunk} pairs) using model `{model}` (source: {model_source})"
        );
        emit_progress(&ctx.events, &cost_msg);
        log_to_agent_log(&project.root, &cost_msg);
        info!("{cost_msg}");

        // Run synth with bounded concurrency.
        let semaphore = Arc::new(Semaphore::new(args.parallelism.max(1)));
        let chunk_slice = chunks[..chunks_needed].to_vec();
        let total = chunk_slice.len();

        let mut handles = Vec::with_capacity(total);
        for (i, chunk) in chunk_slice.into_iter().enumerate() {
            let permit_sem = semaphore.clone();
            let provider_h = provider.clone();
            let events_h = ctx.events.clone();
            let root_h = project.root.clone();
            handles.push(tokio::spawn(async move {
                let _permit = permit_sem.acquire_owned().await;
                let result = synth_one_chunk(&provider_h, &chunk, pairs_per_chunk).await;
                // Per-N progress notification.
                if (i + 1) % PROGRESS_EVERY_N == 0 || i + 1 == total {
                    let msg = format!("synth_gen progress: {} / {} chunks done", i + 1, total);
                    emit_progress(&events_h, &msg);
                    log_to_agent_log(&root_h, &msg);
                }
                (chunk.chunk_id, result)
            }));
        }

        let mut all_pairs: Vec<QaPair> = Vec::with_capacity(total * pairs_per_chunk);
        let mut chunk_failures = 0usize;
        for h in handles {
            let (chunk_id, result) = match h.await {
                Ok(t) => t,
                Err(e) => {
                    warn!(error = %e, "synth task join failed");
                    chunk_failures += 1;
                    continue;
                }
            };
            match result {
                Ok(pairs) => all_pairs.extend(pairs.into_iter().map(|p| QaPair {
                    question: p.question,
                    answer: p.answer,
                    source_chunk_id: chunk_id.clone(),
                })),
                Err(e) => {
                    warn!(chunk = chunk_id, error = ?e, "synth_gen chunk failed");
                    chunk_failures += 1;
                }
            }
        }

        // Cap to target, shuffle with seed, split.
        all_pairs.truncate(target_total);
        let mut rng = ChaCha20Rng::seed_from_u64(seed);
        all_pairs.shuffle(&mut rng);
        // clamp + cast: eval_split is in [0, 0.5] and pair count is bounded by target_pairs (≤100k).
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let eval_n_raw = ((all_pairs.len() as f32) * args.eval_split).round() as usize;
        let eval_n = eval_n_raw.min(all_pairs.len());
        let eval_pairs: Vec<QaPair> = all_pairs.drain(..eval_n).collect();
        let train_pairs = all_pairs;

        let datasets_dir = project.root.join("datasets");
        fs::create_dir_all(&datasets_dir).map_err(ToolError::Io)?;
        write_jsonl(&datasets_dir.join("sft.jsonl"), &train_pairs)?;
        write_jsonl(&datasets_dir.join("eval.jsonl"), &eval_pairs)?;

        let final_msg = format!(
            "synth_gen done: {} train pairs, {} eval pairs (seed={}, chunk_failures={})",
            train_pairs.len(),
            eval_pairs.len(),
            seed,
            chunk_failures
        );
        emit_progress(&ctx.events, &final_msg);
        log_to_agent_log(&project.root, &final_msg);
        info!("{final_msg}");

        Ok(ToolResult::text(final_msg).with_structured(json!({
            "train_pairs":   train_pairs.len(),
            "eval_pairs":    eval_pairs.len(),
            "chunk_failures": chunk_failures,
            "seed":          seed,
            "model":         model,
            "cost_estimate_usd": est_usd,
            "sft_path":      datasets_dir.join("sft.jsonl"),
            "eval_path":     datasets_dir.join("eval.jsonl"),
        })))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawPair {
    question: String,
    answer: String,
}

/// Internal-only; fields are read via `Debug` in `warn!(error = ?e, ...)`.
#[derive(Debug)]
#[allow(dead_code, reason = "fields surfaced via Debug in tracing macros")]
enum SynthChunkError {
    Provider(String),
    ExhaustedRetries(String),
    ParseFail(String),
}

async fn synth_one_chunk(
    provider: &Arc<dyn Provider>,
    chunk: &ChunkRecord,
    pairs_per_chunk: usize,
) -> Result<Vec<RawPair>, SynthChunkError> {
    let prompt = PROMPT_TEMPLATE
        .replace("{{N_PAIRS}}", &pairs_per_chunk.to_string())
        .replace("{{CHUNK_TEXT}}", &chunk.text);
    let messages = vec![Message::user(prompt)];

    // Default policy = the Phase 2 decision-F schedule (1→2→4→8→16 s, 5 tries).
    // Classification: 429/5xx + mid-stream drops are transport (transient);
    // other provider errors and unparseable model output are permanent — a
    // sixth identical prompt won't fix malformed JSON.
    crate::retry::with_backoff(
        &crate::retry::BackoffPolicy::default(),
        "synth_gen chunk",
        |_attempt| {
            let messages = messages.clone();
            let provider = provider.clone();
            async move {
                use crate::retry::Retry;
                let mut stream = match provider.stream(messages, &[]).await {
                    Ok(s) => s,
                    Err(ProviderError::Status { status, body })
                        if status == 429 || (500..600).contains(&status) =>
                    {
                        return Err(Retry::Transient(SynthChunkError::ExhaustedRetries(
                            format!("retryable status {status}: {}", truncate(&body, 200)),
                        )));
                    }
                    Err(e) => {
                        return Err(Retry::Permanent(SynthChunkError::Provider(e.to_string())))
                    }
                };

                let mut response = String::new();
                while let Some(event) = stream.next().await {
                    match event {
                        ProviderEvent::Text(t) => response.push_str(&t),
                        ProviderEvent::Stop { reason: StopReason::Error(e) } => {
                            return Err(Retry::Transient(SynthChunkError::ExhaustedRetries(
                                format!("stream error: {e}"),
                            )));
                        }
                        ProviderEvent::Stop { .. } => break,
                        _ => {}
                    }
                }

                parse_pairs(&response).map_err(|e| {
                    Retry::Permanent(SynthChunkError::ParseFail(format!(
                        "{e}; raw start: {}",
                        truncate(&response, 240)
                    )))
                })
            }
        },
    )
    .await
}

fn parse_pairs(text: &str) -> Result<Vec<RawPair>, String> {
    // Tolerate ```json ... ``` fences in case the model ignored the system prompt.
    let mut s = text.trim();
    if s.starts_with("```") {
        s = s.trim_start_matches("```json").trim_start_matches("```").trim();
        if let Some(idx) = s.rfind("```") {
            s = &s[..idx];
        }
    }
    serde_json::from_str::<Vec<RawPair>>(s.trim()).map_err(|e| e.to_string())
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "rough cost estimate; precise tokenisation isn't worth the dependency"
)]
fn estimate_cost(
    chunks: &[ChunkRecord],
    pairs_per_chunk: usize,
    input_rate: f64,
    output_rate: f64,
) -> (u64, u64, f64) {
    let prompt_overhead_chars = PROMPT_TEMPLATE.len();
    let input_chars: usize = chunks
        .iter()
        .map(|c| c.text.len() + prompt_overhead_chars)
        .sum();
    // Per pair: ~100 chars question + ~250 chars answer = ~350 chars.
    let output_chars = chunks.len() * pairs_per_chunk * 350;

    let in_tokens = (input_chars as f64 / CHARS_PER_TOKEN).round() as u64;
    let out_tokens = (output_chars as f64 / CHARS_PER_TOKEN).round() as u64;
    let usd = (in_tokens as f64 / 1_000_000.0) * input_rate
        + (out_tokens as f64 / 1_000_000.0) * output_rate;
    (in_tokens, out_tokens, usd)
}

fn load_included_chunks(
    project_root: &std::path::Path,
    source_filter: Option<&str>,
) -> Result<Vec<ChunkRecord>, ToolError> {
    let sources_dir = project_root.join("sources");
    if !sources_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&sources_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(filter) = source_filter {
            if name != filter {
                continue;
            }
        }
        let chunks_path = entry.path().join("chunks.jsonl");
        if !chunks_path.is_file() {
            continue;
        }
        let file = fs::File::open(&chunks_path)?;
        for line in BufReader::new(file).lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<ChunkRecord>(trimmed) {
                Ok(rec) if rec.include => out.push(rec),
                Ok(_) => { /* skip excluded */ }
                Err(e) => warn!(error = %e, "skipping malformed chunk line"),
            }
        }
    }
    Ok(out)
}

fn resolve_seed(
    ctx: &ToolContext,
    project_name: &str,
    user_seed: Option<u64>,
) -> Result<u64, ToolError> {
    // Priority: explicit user_seed > existing project synth.split_seed > newly generated.
    let mut cfg = ctx.project_store.get(project_name).map_err(ToolError::Project)?;
    if let Some(s) = user_seed {
        cfg.synth = Some(SynthConfig { split_seed: s });
        ctx.project_store.update(&cfg).map_err(ToolError::Project)?;
        return Ok(s);
    }
    if let Some(SynthConfig { split_seed }) = cfg.synth {
        return Ok(split_seed);
    }
    // TOML serialisation rejects u64 values above i64::MAX (the spec only mandates i64).
    // Mask the top bit when generating a fresh seed — loses 1 bit of entropy, irrelevant
    // for split reproducibility, keeps the on-disk value safe to round-trip.
    let s = rand::thread_rng().next_u64() & (u64::MAX >> 1);
    cfg.synth = Some(SynthConfig { split_seed: s });
    ctx.project_store.update(&cfg).map_err(ToolError::Project)?;
    Ok(s)
}

fn write_jsonl(path: &std::path::Path, pairs: &[QaPair]) -> Result<(), ToolError> {
    let tmp = path.with_extension("jsonl.tmp");
    let f = fs::File::create(&tmp)?;
    let mut w = BufWriter::new(f);
    for p in pairs {
        serde_json::to_writer(&mut w, p).map_err(|e| ToolError::Exec {
            tool: "generate_sft".into(),
            message: format!("jsonl write: {e}"),
        })?;
        w.write_all(b"\n")?;
    }
    w.flush()?;
    drop(w);
    fs::rename(&tmp, path)?;
    Ok(())
}

// Audit-log + progress helpers moved to `super::util` once run_command became
// the second consumer (Phase 4 pre-work). Re-imported here to keep call sites
// unchanged.
use super::util::{emit_progress, log_to_agent_log};

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        let mut out = String::with_capacity(n + 1);
        let _ = write!(out, "{}…", &s[..n]);
        out
    }
}

// ---------------------------------------------------------------------------
/// Pick which model to use for synthesis. Three-tier fallback:
///   1. Explicit `model` tool arg (caller-supplied, wins everything).
///   2. `project.provider.synth_model` override (per-project Haiku-class cost hack).
///   3. `project.provider.model` (the agent loop model — last resort, expensive).
/// Returns the chosen model id plus a short string explaining which tier won; the
/// caller logs it so users can see which budget pocket their `generate_sft` run hit.
#[must_use]
pub(crate) fn resolve_synth_model(
    arg: Option<&str>,
    provider: &crate::project::ProviderConfig,
) -> (String, &'static str) {
    if let Some(m) = arg {
        if !m.is_empty() {
            return (m.to_string(), "explicit arg");
        }
    }
    if !provider.synth_model.is_empty() {
        return (provider.synth_model.clone(), "project.provider.synth_model override");
    }
    (provider.model.clone(), "project.provider.model")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ProviderConfig;

    fn provider(model: &str, synth_model: &str) -> ProviderConfig {
        ProviderConfig {
            name: "anthropic".into(),
            model: model.into(),
            synth_model: synth_model.into(),
        }
    }

    #[test]
    fn synth_model_falls_back_to_primary_when_unset() {
        let p = provider("claude-sonnet-4-6", "");
        let (m, src) = resolve_synth_model(None, &p);
        assert_eq!(m, "claude-sonnet-4-6");
        assert_eq!(src, "project.provider.model");
    }

    #[test]
    fn synth_model_uses_per_project_override_when_set() {
        let p = provider("claude-sonnet-4-6", "claude-haiku-4-5-20251001");
        let (m, src) = resolve_synth_model(None, &p);
        assert_eq!(m, "claude-haiku-4-5-20251001");
        assert_eq!(src, "project.provider.synth_model override");
    }

    #[test]
    fn synth_model_explicit_arg_beats_project_override() {
        let p = provider("claude-sonnet-4-6", "claude-haiku-4-5-20251001");
        let (m, src) = resolve_synth_model(Some("claude-opus-4-7"), &p);
        assert_eq!(m, "claude-opus-4-7");
        assert_eq!(src, "explicit arg");
    }

    #[test]
    fn synth_model_empty_explicit_arg_treated_as_unset() {
        let p = provider("claude-sonnet-4-6", "claude-haiku-4-5-20251001");
        let (m, src) = resolve_synth_model(Some(""), &p);
        assert_eq!(m, "claude-haiku-4-5-20251001");
        assert_eq!(src, "project.provider.synth_model override");
    }

    #[test]
    fn parse_pairs_accepts_bare_array() {
        let raw = r#"[{"question":"q1","answer":"a1"},{"question":"q2","answer":"a2"}]"#;
        let pairs = parse_pairs(raw).unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].question, "q1");
    }

    #[test]
    fn parse_pairs_strips_code_fences() {
        let raw = "```json\n[{\"question\":\"q\",\"answer\":\"a\"}]\n```";
        let pairs = parse_pairs(raw).unwrap();
        assert_eq!(pairs.len(), 1);
    }

    #[test]
    fn parse_pairs_returns_empty_for_empty_array() {
        assert_eq!(parse_pairs("[]").unwrap().len(), 0);
    }

    #[test]
    fn parse_pairs_errors_on_malformed_json() {
        assert!(parse_pairs("not json").is_err());
        assert!(parse_pairs("[{\"q\": \"missing answer\"}]").is_err());
    }

    #[test]
    fn estimate_cost_scales_with_chunks() {
        let one = ChunkRecord {
            chunk_id: "c1".into(),
            doc_id: "d".into(),
            source_id: "s".into(),
            text: "a".repeat(2000),
            token_count: 500,
            sentence_range: (0, 0),
            include: true,
            metadata: Value::Null,
        };
        let two = vec![one.clone(), one.clone()];
        let (i1, _, u1) = estimate_cost(&[one], 2, 3.0, 15.0);
        let (i2, _, u2) = estimate_cost(&two, 2, 3.0, 15.0);
        assert!(i2 > i1);
        assert!(u2 > u1);
    }

    #[test]
    fn prompt_template_has_placeholders() {
        assert!(PROMPT_TEMPLATE.contains("{{N_PAIRS}}"));
        assert!(PROMPT_TEMPLATE.contains("{{CHUNK_TEXT}}"));
    }
}
