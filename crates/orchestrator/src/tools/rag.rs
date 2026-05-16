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
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{info, warn};

use super::context::{ToolContext, ToolEvent};
use super::registry::{Tool, ToolDef, ToolError, ToolResult};
use crate::worker::{call_worker, WorkerCommand};

const RAG_QUERY_TIMEOUT_SECS: u64 = 60;
const CHAT_TIMEOUT_SECS: u64 = 180;

/// One hit returned by the rag worker. Mirrors the rag worker's query output.
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
}

fn default_top_k() -> u32 {
    5
}

pub struct QueryIndex;

#[async_trait]
impl Tool for QueryIndex {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "query_index".into(),
            description: "Retrieve the top_k most relevant chunks from the project's LanceDB \
                          index for a natural-language query. Pure retrieval, no LLM generation. \
                          Useful for inspecting what the rag_chat tool will see as context."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query":     { "type": "string", "description": "Natural-language query." },
                    "top_k":     { "type": "integer", "minimum": 1, "maximum": 20, "default": 5 },
                    "source_id": { "type": "string", "description": "Restrict to one source." }
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
        let hits = retrieve(ctx, &project.root, &args.query, args.top_k, args.source_id.as_deref())
            .await?;

        let mut text = String::new();
        let _ = writeln!(text, "{} hits for `{}`", hits.len(), args.query);
        for (i, h) in hits.iter().enumerate() {
            let snippet: String = h.text.chars().take(160).collect();
            let _ = writeln!(
                text,
                "{}. [{}] dist={:.3} src={} :: {}",
                i + 1,
                h.chunk_id,
                h.distance,
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
                    "source_id":   { "type": "string", "description": "Restrict retrieval to one source." }
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

        let hits = retrieve(ctx, &project.root, &args.query, args.top_k, args.source_id.as_deref())
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
        let _ = writeln!(text, "--- citations ---");
        for (i, h) in hits.iter().enumerate() {
            let snippet: String = h.text.chars().take(120).collect();
            let _ = writeln!(
                text,
                "[{}] {} (dist={:.3}) :: {}…",
                i + 1,
                h.chunk_id,
                h.distance,
                snippet.replace('\n', " ")
            );
        }

        info!(query = %args.query, hits = hits.len(), answer_chars = answer.len(), "rag_chat");
        Ok(ToolResult::text(text).with_structured(json!({
            "answer": answer,
            "hits": hits,
            "endpoint": endpoint,
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
pub async fn retrieve(
    ctx: &ToolContext,
    project_root: &std::path::Path,
    query: &str,
    top_k: u32,
    source_filter: Option<&str>,
) -> Result<Vec<RetrievedChunk>, ToolError> {
    let runner = ctx
        .python_runner
        .as_ref()
        .ok_or_else(|| ToolError::Exec {
            tool: "rag.query".into(),
            message: "PythonRunner not configured on ToolContext".into(),
        })?
        .clone();
    let params = json!({
        "project_root": project_root,
        "query": query,
        "top_k": top_k,
        "source_id": source_filter,
    });
    let cmd = WorkerCommand {
        module: "narrowmind_workers.rag".into(),
        method: "rag.query".into(),
        params,
        timeout: Some(Duration::from_secs(RAG_QUERY_TIMEOUT_SECS)),
    };
    let value = call_worker(&runner, &cmd).await.map_err(|e| ToolError::Exec {
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
        .timeout(Duration::from_secs(CHAT_TIMEOUT_SECS))
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
        .timeout(Duration::from_secs(CHAT_TIMEOUT_SECS))
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
