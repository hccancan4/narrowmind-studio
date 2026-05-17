//! Chat preview Tauri commands.
//!
//! `chat_preview_send` does retrieval + streaming chat against the running llama.cpp
//! server, firing `chat-preview:*` Tauri events the floating window subscribes to. It
//! bypasses the agent loop entirely — the chat preview is a direct surface onto the
//! DSLM, distinct from the agent terminal.

#![allow(clippy::needless_pass_by_value)]

use std::sync::Arc;

use narrowmind_orchestrator::tools::rag::{
    assemble_prompt, chat_completion_stream, retrieve, RetrievedChunk,
};
use narrowmind_orchestrator::{ModelSpec, ToolContext};
use serde_json::json;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::mpsc;

use crate::state::AppState;

pub const TOKEN_EVENT: &str = "chat-preview:token";
pub const HITS_EVENT: &str = "chat-preview:hits";
pub const DONE_EVENT: &str = "chat-preview:done";
pub const ERROR_EVENT: &str = "chat-preview:error";

#[tauri::command]
pub async fn chat_preview_send(
    app: AppHandle,
    state: State<'_, AppState>,
    query: String,
    top_k: Option<u32>,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
) -> Result<(), String> {
    let project = state
        .selected_project
        .lock()
        .await
        .clone()
        .ok_or_else(|| "no project selected".to_string())?;

    let status = state.inference.status().await;
    let endpoint = status
        .endpoint
        .ok_or_else(|| "inference server not running — call start_inference_server first".to_string())?;
    state.inference.mark_used().await;

    // Build a minimal ToolContext so we can reuse the retrieve() helper.
    let (sink, _drain) = mpsc::unbounded_channel();
    let ctx = ToolContext::new(
        state.selected_project.clone(),
        state.project_store.clone(),
        sink,
    )
    .with_python_runner(state.python_runner.clone())
    .with_inference(state.inference.clone());

    let hits: Vec<RetrievedChunk> =
        retrieve(&ctx, &project.root, &query, top_k.unwrap_or(5), None)
            .await
            .map_err(|e| {
                let msg = e.to_string();
                let _ = app.emit(ERROR_EVENT, json!({ "stage": "retrieve", "error": &msg }));
                msg
            })?;
    let _ = app.emit(HITS_EVENT, &hits);

    let prompt = assemble_prompt(&hits, &query);

    // Channel to ferry tokens from the streaming callback (sync FnMut) over to the
    // async Tauri emit call — keeps the on_token closure non-async.
    let (tok_tx, mut tok_rx) = mpsc::unbounded_channel::<String>();
    let app_for_tokens = app.clone();
    let pump = tokio::spawn(async move {
        while let Some(t) = tok_rx.recv().await {
            let _ = app_for_tokens.emit(TOKEN_EVENT, &t);
        }
    });

    let final_text = chat_completion_stream(
        &endpoint,
        &prompt,
        max_tokens.unwrap_or(1024),
        temperature.unwrap_or(0.7),
        move |tok| {
            let _ = tok_tx.send(tok.to_string());
        },
    )
    .await;

    let _ = pump.await;

    match final_text {
        Ok(text) => {
            let _ = app.emit(DONE_EVENT, json!({ "answer": text }));
            Ok(())
        }
        Err(e) => {
            let _ = app.emit(ERROR_EVENT, json!({ "stage": "stream", "error": &e }));
            Err(e)
        }
    }
}

/// Zero-API bootstrap for the chat preview window. The "Local chat" banner button
/// calls this so the user can start a purely-local conversation without first
/// asking Sonnet to run the `open_chat_preview` agent tool — that detour costs
/// one round-trip of Anthropic spend just to open a window that then talks only
/// to the local Qwen.
///
/// Idempotent: if the inference server is already up for any project, we reuse
/// it (one server per app per Phase 3 decision F). Otherwise we spin up the
/// default Qwen2.5-7B GGUF using the same path the agent tool uses.
///
/// Returns the same shape as `chat_preview_context` so the front-end can route
/// it straight into the WebviewWindow that loads `#/chat-preview`.
#[tauri::command]
pub async fn chat_preview_bootstrap(
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    // Reject early if no project is selected so the window doesn't open into an
    // empty-context error state — the user wouldn't know what to do.
    if state.selected_project.lock().await.is_none() {
        return Err("select a project first".into());
    }

    let model = ModelSpec::default_qwen2_5_7b_q4km();
    let endpoint = state
        .inference
        .ensure_running(state.python_runner.as_ref(), &model)
        .await
        .map_err(|e| format!("start inference server: {e}"))?;
    state.inference.mark_used().await;

    let status = state.inference.status().await;
    let project = state
        .selected_project
        .lock()
        .await
        .as_ref()
        .map(|s| s.name.clone());
    Ok(json!({
        "project": project,
        "endpoint": endpoint,
        "model": status.repo_id,
        "filename": status.filename,
        "running": status.running,
    }))
}

/// Provides the chat preview window the same project + endpoint info the parent passed via
/// the `UiAction` event. UI calls this on first render rather than parsing URL params, which
/// keeps the routing simple.
#[tauri::command]
pub async fn chat_preview_context(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let project = state
        .selected_project
        .lock()
        .await
        .clone()
        .map(|s| s.name);
    let status = state.inference.status().await;
    Ok(json!({
        "project": project,
        "endpoint": status.endpoint,
        "model": status.repo_id,
        "filename": status.filename,
        "running": status.running,
    }))
}

// Suppress unused-imports warning when Arc isn't otherwise referenced.
#[allow(dead_code)]
fn _arc_marker(_: Arc<u8>) {}
