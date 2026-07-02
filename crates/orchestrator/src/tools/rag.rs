//! `rag_chat`, `query_index`, `open_chat_preview` — the user-facing RAG entry points.
//!
//! All three lean on the same two backends:
//! - the rag worker (`rag.query` RPC) for retrieval
//! - the llama.cpp HTTP server (`/v1/chat/completions`) for generation
//!
//! `rag_chat` is the blocking agent-callable path. `open_chat_preview` only emits a UI
//! action — the desktop layer interprets it and opens a separate window that handles its
//! own streaming chat via a dedicated Tauri command (see
//! `apps/desktop/src-tauri/src/commands/chat.rs`).

use std::fmt::Write as _;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{info, warn};

use super::context::{ToolContext, ToolEvent};
use super::registry::{Tool, ToolDef, ToolError, ToolResult};
use crate::retry::timeouts;
use crate::worker::{call_worker, WorkerCommand};

/// One hit returned by the rag worker. Mirrors the rag worker's query output.
/// Different retrieval modes populate different scoring fields:
///   - dense → `distance` (lower = closer)
///   - sparse → `score` (BM25, higher = better)
///   - hybrid → `rrf_score` + `rrf_sources` (which retrievers contributed)
///
/// Only one is filled for any given hit; the others stay at their default.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievedChunk {
    pub chunk_id: String,
    pub doc_id: String,
    pub source_id: String,
    pub text: String,
    #[serde(default)]
    pub token_count: u32,
    #[serde(default)]
    pub metadata: Value,
    #[serde(default, rename = "_distance")]
    pub distance: f32,
    #[serde(default, rename = "_score")]
    pub score: f32,
    #[serde(default, rename = "_rrf_score")]
    pub rrf_score: f32,
    #[serde(default, rename = "_rrf_sources")]
    pub rrf_sources: Vec<String>,
}

impl RetrievedChunk {
    /// Single human-readable scoring string for log/UI lines, regardless of
    /// which mode produced the hit. Tools use this so the citation footer
    /// reads uniformly across dense / sparse / hybrid results.
    #[must_use]
    pub fn score_label(&self) -> String {
        if !self.rrf_sources.is_empty() {
            // Hybrid: report fused score + which retrievers contributed.
            let srcs = self.rrf_sources.join("+");
            format!("rrf={:.4} ({srcs})", self.rrf_score)
        } else if self.score > 0.0 {
            format!("bm25={:.3}", self.score)
        } else {
            format!("dist={:.3}", self.distance)
        }
    }
}

// ---------------------------------------------------------------------------
// query_index — retrieve only, no generation
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct QueryIndexArgs {
    query: String,
    #[serde(default = "default_top_k")]
    top_k: u32,
    #[serde(default)]
    source_id: Option<String>,
    /// Override the project's `[rag] retrieval_mode`. One of `dense | sparse | hybrid`.
    /// Omit to use the project default (post-3.5 default is `hybrid`).
    #[serde(default)]
    mode: Option<String>,
}

fn default_top_k() -> u32 {
    5
}

/// Build the effective `RagConfig` for one tool invocation:
/// start from the project's persisted `[rag]` defaults, then apply the
/// optional per-call `mode` override. Returns an error for unknown modes.
fn effective_rag_cfg(
    project_cfg_rag: &crate::project::RagConfig,
    mode_override: Option<&str>,
    tool: &str,
) -> Result<crate::project::RagConfig, ToolError> {
    use crate::project::RetrievalMode;
    let mut out = project_cfg_rag.clone();
    if let Some(m) = mode_override {
        out.retrieval_mode = match m {
            "dense" => RetrievalMode::Dense,
            "sparse" => RetrievalMode::Sparse,
            "hybrid" => RetrievalMode::Hybrid,
            other => {
                return Err(ToolError::BadInput {
                    tool: tool.into(),
                    reason: format!(
                        "unknown retrieval mode `{other}` (expected dense | sparse | hybrid)"
                    ),
                });
            }
        };
    }
    Ok(out)
}

pub struct QueryIndex;

#[async_trait]
impl Tool for QueryIndex {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "query_index".into(),
            description: "Retrieve the top_k most relevant chunks from the project's LanceDB \
                          index for a natural-language query. Pure retrieval, no LLM generation. \
                          Useful for inspecting what the rag_chat tool will see as context. \
                          The optional `mode` arg overrides the project's [rag] retrieval_mode \
                          for A/B comparison between dense, sparse (BM25), and hybrid (RRF)."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query":     { "type": "string", "description": "Natural-language query." },
                    "top_k":     { "type": "integer", "minimum": 1, "maximum": 20, "default": 5 },
                    "source_id": { "type": "string", "description": "Restrict to one source." },
                    "mode":      { "type": "string", "enum": ["dense", "sparse", "hybrid"], "description": "Retrieval mode override; defaults to the project's [rag] retrieval_mode." }
                },
                "required": ["query"]
            }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, args: Value) -> Result<ToolResult, ToolError> {
        let args: QueryIndexArgs =
            serde_json::from_value(args).map_err(|e| ToolError::BadInput {
                tool: "query_index".into(),
                reason: e.to_string(),
            })?;
        let project = ctx.current_project().await.ok_or(ToolError::NoProject)?;
        let cfg = ctx.project_store.get(&project.name).map_err(ToolError::Project)?;
        let rag_cfg = effective_rag_cfg(&cfg.rag, args.mode.as_deref(), "query_index")?;
        let hits = retrieve(
            ctx,
            &project.root,
            &args.query,
            args.top_k,
            args.source_id.as_deref(),
            &rag_cfg,
        )
        .await?;

        let mut text = String::new();
        let _ = writeln!(
            text,
            "{} hits for `{}` (mode={})",
            hits.len(),
            args.query,
            rag_cfg.retrieval_mode.as_str()
        );
        for (i, h) in hits.iter().enumerate() {
            let snippet: String = h.text.chars().take(160).collect();
            let _ = writeln!(
                text,
                "{}. [{}] {} src={} :: {}",
                i + 1,
                h.chunk_id,
                h.score_label(),
                h.source_id,
                snippet.replace('\n', " ")
            );
        }
        Ok(ToolResult::text(text).with_structured(serde_json::to_value(&hits).unwrap_or(Value::Null)))
    }
}

// ---------------------------------------------------------------------------
// rag_chat — retrieve + generate (blocking, agent-callable)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RagChatArgs {
    query: String,
    #[serde(default = "default_top_k")]
    top_k: u32,
    #[serde(default = "default_max_tokens")]
    max_tokens: u32,
    #[serde(default = "default_temperature")]
    temperature: f32,
    #[serde(default)]
    source_id: Option<String>,
    /// Override the project's `[rag] retrieval_mode`. Same semantics as
    /// `query_index.mode`. Lets the agent A/B between modes mid-conversation
    /// without editing project.toml.
    #[serde(default)]
    mode: Option<String>,
}

fn default_max_tokens() -> u32 {
    1024
}
fn default_temperature() -> f32 {
    0.7
}

pub struct RagChat;

#[async_trait]
impl Tool for RagChat {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "rag_chat".into(),
            description: "Retrieve top_k chunks via BGE-small + LanceDB, assemble them as \
                          context, ask the running llama.cpp inference server, return the \
                          assistant's answer with citations. Requires start_inference_server \
                          to have been called first. Blocking — for interactive token-by-token \
                          streaming use the chat preview window (open_chat_preview)."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query":       { "type": "string" },
                    "top_k":       { "type": "integer", "minimum": 1, "maximum": 20, "default": 5 },
                    "max_tokens":  { "type": "integer", "minimum": 32, "maximum": 4096, "default": 1024 },
                    "temperature": { "type": "number",  "minimum": 0.0, "maximum": 2.0, "default": 0.7 },
                    "source_id":   { "type": "string", "description": "Restrict retrieval to one source." },
                    "mode":        { "type": "string", "enum": ["dense", "sparse", "hybrid"], "description": "Retrieval mode override; defaults to the project's [rag] retrieval_mode." }
                },
                "required": ["query"]
            }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, args: Value) -> Result<ToolResult, ToolError> {
        let args: RagChatArgs = serde_json::from_value(args).map_err(|e| ToolError::BadInput {
            tool: "rag_chat".into(),
            reason: e.to_string(),
        })?;
        let project = ctx.current_project().await.ok_or(ToolError::NoProject)?;
        let cfg = ctx.project_store.get(&project.name).map_err(ToolError::Project)?;
        let rag_cfg = effective_rag_cfg(&cfg.rag, args.mode.as_deref(), "rag_chat")?;
        let inference = ctx.inference.as_ref().ok_or_else(|| ToolError::Exec {
            tool: "rag_chat".into(),
            message: "InferenceManager not configured on ToolContext".into(),
        })?;
        let status = inference.status().await;
        let endpoint = status.endpoint.ok_or_else(|| ToolError::Exec {
            tool: "rag_chat".into(),
            message: "inference server not running — call start_inference_server first".into(),
        })?;
        inference.mark_used().await;

        let hits = retrieve(
            ctx,
            &project.root,
            &args.query,
            args.top_k,
            args.source_id.as_deref(),
            &rag_cfg,
        )
        .await?;
        let prompt = assemble_prompt(&hits, &args.query);
        let answer = chat_completion(&endpoint, &prompt, args.max_tokens, args.temperature)
            .await
            .map_err(|e| ToolError::Exec {
                tool: "rag_chat".into(),
                message: format!("llama.cpp server: {e}"),
            })?;

        let mut text = String::new();
        let _ = writeln!(text, "{answer}");
        let _ = writeln!(text);
        let _ = writeln!(text, "--- citations (mode={}) ---", rag_cfg.retrieval_mode.as_str());
        for (i, h) in hits.iter().enumerate() {
            let snippet: String = h.text.chars().take(120).collect();
            let _ = writeln!(
                text,
                "[{}] {} ({}) :: {}…",
                i + 1,
                h.chunk_id,
                h.score_label(),
                snippet.replace('\n', " ")
            );
        }

        info!(query = %args.query, mode = rag_cfg.retrieval_mode.as_str(), hits = hits.len(), answer_chars = answer.len(), "rag_chat");
        Ok(ToolResult::text(text).with_structured(json!({
            "answer": answer,
            "hits": hits,
            "endpoint": endpoint,
            "mode": rag_cfg.retrieval_mode.as_str(),
        })))
    }
}

// ---------------------------------------------------------------------------
// open_chat_preview — UI action, frontend opens the window
// ---------------------------------------------------------------------------

pub struct OpenChatPreview;

#[async_trait]
impl Tool for OpenChatPreview {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "open_chat_preview".into(),
            description: "Ask the desktop UI to open a floating chat preview window pointed at \
                          the running inference server. Returns immediately — the streaming \
                          chat happens entirely in that window. Requires start_inference_server \
                          to have been called first."
                .into(),
            input_schema: json!({ "type": "object" }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, _args: Value) -> Result<ToolResult, ToolError> {
        let project = ctx.current_project().await.ok_or(ToolError::NoProject)?;
        let inference = ctx.inference.as_ref().ok_or_else(|| ToolError::Exec {
            tool: "open_chat_preview".into(),
            message: "InferenceManager not configured on ToolContext".into(),
        })?;
        let status = inference.status().await;
        if !status.running {
            return Err(ToolError::Exec {
                tool: "open_chat_preview".into(),
                message: "inference server not running — call start_inference_server first".into(),
            });
        }
        let payload = json!({
            "action": "open_chat_preview",
            "project": project.name,
            "endpoint": status.endpoint,
            "model": status.repo_id,
            "filename": status.filename,
        });
        let _ = ctx.events.send(ToolEvent::UiAction(payload.clone()));
        Ok(ToolResult::text("chat preview window opening").with_structured(payload))
    }
}

// ---------------------------------------------------------------------------
// Helpers — re-used by tools/rag.rs *and* by the chat_preview Tauri command
// (see apps/desktop/src-tauri/src/commands/chat.rs)
// ---------------------------------------------------------------------------

/// Hit retrieval via the rag worker. Public so the chat-preview command can reuse it.
///
/// `rag_cfg` carries the retrieval mode + RRF parameters. Callers that want the
/// project's persisted defaults pass the project's `cfg.rag` straight through;
/// the agent-tool `mode` arg overrides the persisted default when present.
///
/// Routing: prefers the long-lived [`WorkerPool`](crate::worker_pool::WorkerPool)
/// (warm BGE-small — retrieval in tens of ms instead of a ~5-8 s cold spawn per
/// call), falling back to the one-shot `call_worker` when no pool is configured
/// (older tests, headless tools). The first pooled call still pays the cold
/// start, so the timeout stays at the one-shot ceiling.
pub async fn retrieve(
    ctx: &ToolContext,
    project_root: &std::path::Path,
    query: &str,
    top_k: u32,
    source_filter: Option<&str>,
    rag_cfg: &crate::project::RagConfig,
) -> Result<Vec<RetrievedChunk>, ToolError> {
    let params = json!({
        "project_root": project_root,
        "query": query,
        "top_k": top_k,
        "source_id": source_filter,
        "mode": rag_cfg.retrieval_mode.as_str(),
        "k_dense": rag_cfg.hybrid_k_dense,
        "k_sparse": rag_cfg.hybrid_k_sparse,
        "rrf_k": rag_cfg.rrf_k,
    });
    let cmd = WorkerCommand {
        module: "narrowmind_workers.rag".into(),
        method: "rag.query".into(),
        params,
        timeout: Some(timeouts::RAG_QUERY),
    };

    let value = if let Some(pool) = ctx.worker_pool.as_ref() {
        pool.call(&cmd).await
    } else {
        let runner = ctx
            .python_runner
            .as_ref()
            .ok_or_else(|| ToolError::Exec {
                tool: "rag.query".into(),
                message: "neither WorkerPool nor PythonRunner configured on ToolContext".into(),
            })?
            .clone();
        call_worker(&runner, &cmd).await
    }
    .map_err(|e| ToolError::Exec {
        tool: "rag.query".into(),
        message: e.to_string(),
    })?;

    let hits_value = value.get("hits").cloned().unwrap_or(Value::Array(vec![]));
    let hits: Vec<RetrievedChunk> =
        serde_json::from_value(hits_value).map_err(|e| ToolError::Exec {
            tool: "rag.query".into(),
            message: format!("malformed hits payload: {e}"),
        })?;
    Ok(hits)
}

/// Build the chat prompt: short system instructions + retrieved chunks as context + user.
#[must_use]
pub fn assemble_prompt(hits: &[RetrievedChunk], query: &str) -> ChatPrompt {
    let mut context = String::new();
    for (i, h) in hits.iter().enumerate() {
        let _ = writeln!(context, "[chunk {}] {}", i + 1, h.text.trim());
        let _ = writeln!(context);
    }
    let system = "You are a helpful assistant answering questions about a domain. \
                  Ground your answer in the provided context chunks. If the chunks do not \
                  contain the answer, say so explicitly rather than guessing. Cite chunks \
                  inline as [chunk N] when you use them."
        .to_string();
    let user = if context.trim().is_empty() {
        format!("Question: {query}\n\n(No context chunks retrieved.)")
    } else {
        format!("Context:\n\n{context}\nQuestion: {query}")
    };
    ChatPrompt { system, user }
}

/// Two-field chat prompt the OpenAI-shaped POST body uses.
#[derive(Debug, Clone)]
pub struct ChatPrompt {
    pub system: String,
    pub user: String,
}

/// One-shot non-streaming chat completion against llama.cpp's OpenAI-compatible endpoint.
pub async fn chat_completion(
    endpoint: &str,
    prompt: &ChatPrompt,
    max_tokens: u32,
    temperature: f32,
) -> Result<String, String> {
    let url = format!("{}/v1/chat/completions", endpoint.trim_end_matches('/'));
    let body = json!({
        "model": "default",
        "messages": [
            { "role": "system", "content": prompt.system },
            { "role": "user",   "content": prompt.user },
        ],
        "temperature": temperature,
        "max_tokens": max_tokens,
        "stream": false,
    });
    let client = reqwest::Client::builder()
        .timeout(timeouts::LOCAL_CHAT_COMPLETION)
        .build()
        .map_err(|e| format!("reqwest client: {e}"))?;
    let resp = client.post(&url).json(&body).send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let payload: Value = resp.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("status {status}: {payload}"));
    }
    let answer = payload
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            warn!(?payload, "chat completion missing choices[0].message.content");
            "chat response missing assistant content".to_string()
        })?
        .to_string();
    Ok(answer)
}

/// Streaming variant — invokes `on_token` for each new chunk of assistant text. Returns
/// the full concatenated answer when the stream ends (or on `[DONE]`). Used by the
/// chat-preview Tauri command (`apps/desktop/src-tauri/src/commands/chat.rs`); the agent
/// loop uses the non-streaming `chat_completion` above.
pub async fn chat_completion_stream<F>(
    endpoint: &str,
    prompt: &ChatPrompt,
    max_tokens: u32,
    temperature: f32,
    mut on_token: F,
) -> Result<String, String>
where
    F: FnMut(&str) + Send,
{
    use futures::StreamExt;

    let url = format!("{}/v1/chat/completions", endpoint.trim_end_matches('/'));
    let body = json!({
        "model": "default",
        "messages": [
            { "role": "system", "content": prompt.system },
            { "role": "user",   "content": prompt.user },
        ],
        "temperature": temperature,
        "max_tokens": max_tokens,
        "stream": true,
    });
    let client = reqwest::Client::builder()
        .timeout(timeouts::LOCAL_CHAT_COMPLETION)
        .build()
        .map_err(|e| format!("reqwest client: {e}"))?;
    let resp = client.post(&url).json(&body).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("status {status}: {body}"));
    }

    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut stream = resp.bytes_stream();
    let mut full = String::new();

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| format!("stream byte error: {e}"))?;
        buf.extend_from_slice(&bytes);
        // SSE events end with \n\n.
        while let Some(pos) = find_subsequence(&buf, b"\n\n") {
            let event_bytes: Vec<u8> = buf.drain(..pos + 2).collect();
            let event_str = std::str::from_utf8(&event_bytes).unwrap_or("");
            for line in event_str.lines() {
                let Some(data) = line.strip_prefix("data:") else { continue };
                let data = data.trim();
                if data == "[DONE]" {
                    return Ok(full);
                }
                let Ok(val) = serde_json::from_str::<Value>(data) else { continue };
                if let Some(tok) = val.pointer("/choices/0/delta/content").and_then(Value::as_str) {
                    full.push_str(tok);
                    on_token(tok);
                }
            }
        }
    }
    Ok(full)
}

fn find_subsequence(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}
