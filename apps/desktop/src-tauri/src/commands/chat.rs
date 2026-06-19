//! Chat preview Tauri commands.
//!
//! `chat_preview_send` does retrieval + streaming chat against the running llama.cpp
//! server, firing `chat-preview:*` Tauri events the floating window subscribes to. It
//! bypasses the agent loop entirely — the chat preview is a direct surface onto the
//! DSLM, distinct from the agent terminal.

#![allow(clippy::needless_pass_by_value)]

use std::sync::Arc;

use std::path::Path;

use narrowmind_orchestrator::tools::rag::{
    assemble_prompt, chat_completion_stream, retrieve, RetrievedChunk,
};
use narrowmind_orchestrator::{models, InferenceStatus, ModelSpec, ToolContext};
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

    // Build a minimal ToolContext so we can reuse the retrieve() helper. The worker
    // pool matters here more than anywhere: this is the zero-API Local Chat path,
    // and the warm BGE child is what keeps click-to-first-token inside the budget.
    let (sink, _drain) = mpsc::unbounded_channel();
    let ctx = ToolContext::new(
        state.selected_project.clone(),
        state.project_store.clone(),
        sink,
    )
    .with_python_runner(state.python_runner.clone())
    .with_inference(state.inference.clone())
    .with_worker_pool(state.worker_pool.clone());

    // Local Chat reads the project's persisted retrieval mode — same defaults
    // the agent tools use. No per-call override here; if the user wants to A/B
    // sparse vs hybrid they edit project.toml or use the agent's query_index tool.
    let proj_cfg = state.project_store.get(&project.name).map_err(|e| e.to_string())?;
    let hits: Vec<RetrievedChunk> = retrieve(
        &ctx,
        &project.root,
        &query,
        top_k.unwrap_or(5),
        None,
        &proj_cfg.rag,
    )
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

// ---------------------------------------------------------------------------
// Selectable serving models (Phase 4.6): base registry + domain GGUFs
// ---------------------------------------------------------------------------

/// The models the chat preview can serve for a project: the registry base
/// models plus any exported domain GGUFs in `models/`. Stable keys
/// (`base:<id>` / `domain:<filename>`) round-trip a choice from the UI.
fn serveable_models(project_root: &Path) -> Vec<serde_json::Value> {
    let mut out: Vec<serde_json::Value> = models::all()
        .iter()
        .map(|m| {
            json!({
                "key": format!("base:{}", m.id),
                "label": format!("{} (base)", m.display_name),
                "kind": "base",
            })
        })
        .collect();
    if let Ok(entries) = std::fs::read_dir(project_root.join("models")) {
        let mut ggufs: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("gguf") {
                    return None;
                }
                p.file_name().and_then(|n| n.to_str()).map(str::to_string)
            })
            .collect();
        ggufs.sort();
        for f in ggufs {
            out.push(json!({
                "key": format!("domain:{f}"),
                "label": format!("{f} (fine-tune)"),
                "kind": "domain",
            }));
        }
    }
    out
}

/// Resolve a serveable-model key to a `ModelSpec`. Domain GGUFs reuse the
/// registry default's serving knobs (n_ctx, q8_0 KV, flash-attn, prompt cache)
/// with the local path swapped in. `None` for an unknown key.
fn model_spec_for_key(key: &str, project_root: &Path) -> Option<ModelSpec> {
    if let Some(id) = key.strip_prefix("base:") {
        return models::get(id).map(|m| m.to_model_spec());
    }
    if let Some(fname) = key.strip_prefix("domain:") {
        let path = project_root.join("models").join(fname);
        if !path.is_file() {
            return None;
        }
        let mut spec = models::default_model().to_model_spec();
        spec.repo_id = "local".into();
        spec.filename = path.to_string_lossy().into_owned();
        return Some(spec);
    }
    None
}

/// What Local Chat opens with when no model is chosen: the project's domain
/// GGUF (the user's fine-tune) if one exists, else the registry default base.
fn default_model_key(project_root: &Path) -> String {
    serveable_models(project_root)
        .iter()
        .find(|m| m["kind"].as_str() == Some("domain"))
        .and_then(|m| m["key"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("base:{}", models::default_model().id))
}

/// Key of the currently-running server, so the UI can pre-select it.
fn current_model_key(status: &InferenceStatus) -> Option<String> {
    if !status.running {
        return None;
    }
    let filename = status.filename.as_deref()?;
    if status.repo_id.as_deref() == Some("local") {
        let base = Path::new(filename).file_name()?.to_str()?;
        return Some(format!("domain:{base}"));
    }
    models::all()
        .iter()
        .find(|m| m.gguf_file == filename)
        .map(|m| format!("base:{}", m.id))
}

/// List the serveable models for the selected project + which one is live.
#[tauri::command]
pub async fn chat_preview_models(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let project = state
        .selected_project
        .lock()
        .await
        .clone()
        .ok_or_else(|| "select a project first".to_string())?;
    let status = state.inference.status().await;
    Ok(json!({
        "models": serveable_models(&project.root),
        "current": current_model_key(&status),
    }))
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
    model_key: Option<String>,
) -> Result<serde_json::Value, String> {
    let project = state
        .selected_project
        .lock()
        .await
        .clone()
        .ok_or_else(|| "select a project first".to_string())?;

    // KARAR 1: training owns the VRAM. Local Chat NEVER interrupts a run —
    // it reports the situation honestly instead. ("Local Chat is sacred"
    // means always reachable in one click, not "kills training on click".)
    let training = state.training.status().await;
    if training.active {
        return Ok(json!({
            "training_active": true,
            "run_id": training.run_id,
            "step": training.step,
            "total_steps": training.total_steps,
            "project": training.project,
            "running": false,
        }));
    }

    // Which model to serve:
    //  - explicit key from the picker -> switch to it (ensure_running restarts
    //    if the server is on a different model);
    //  - no key + a server already up -> reuse it (no surprise switch);
    //  - no key + nothing up -> the smart default (the project's domain GGUF if
    //    one exists, else the base) so one click lands on the user's fine-tune.
    let spec = match model_key {
        Some(key) => model_spec_for_key(&key, &project.root)
            .ok_or_else(|| format!("unknown model `{key}`"))?,
        None => {
            let status = state.inference.status().await;
            if status.running {
                return Ok(bootstrap_context(&project.name, &status));
            }
            let key = default_model_key(&project.root);
            model_spec_for_key(&key, &project.root)
                .ok_or_else(|| format!("default model `{key}` unresolved"))?
        }
    };

    state
        .inference
        .ensure_running(state.python_runner.as_ref(), &spec)
        .await
        .map_err(|e| format!("start inference server: {e}"))?;
    state.inference.mark_used().await;

    let status = state.inference.status().await;
    Ok(bootstrap_context(&project.name, &status))
}

/// The context payload the chat window consumes (project + live endpoint +
/// which model is serving, as a selectable key).
fn bootstrap_context(project: &str, status: &InferenceStatus) -> serde_json::Value {
    json!({
        "project": project,
        "endpoint": status.endpoint,
        "model": status.repo_id,
        "filename": status.filename,
        "running": status.running,
        "model_key": current_model_key(status),
    })
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
        "model_key": current_model_key(&status),
    }))
}

// Suppress unused-imports warning when Arc isn't otherwise referenced.
#[allow(dead_code)]
fn _arc_marker(_: Arc<u8>) {}
