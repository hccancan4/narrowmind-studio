//! Tauri commands the Dataset Browser calls directly (bypassing the agent loop).
//!
//! Both commands delegate to the same `ListChunks` / `FilterChunks` `Tool` impls the
//! agent uses, so the UI and the model share one code path — only the wrapper differs.

#![allow(clippy::needless_pass_by_value)]

use narrowmind_orchestrator::tools::chunks::{FilterChunks, ListChunks};
use narrowmind_orchestrator::{Tool, ToolContext};
use serde_json::{json, Value};
use tauri::State;

use crate::state::AppState;

#[tauri::command]
pub async fn list_chunks_cmd(
    state: State<'_, AppState>,
    source_id: Option<String>,
    search: Option<String>,
    show: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<Value, String> {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let ctx = ToolContext::new(
        state.selected_project.clone(),
        state.project_store.clone(),
        tx,
    );
    let args = json!({
        "source_id": source_id,
        "search": search,
        "show": show.unwrap_or_else(|| "all".into()),
        "offset": offset.unwrap_or(0),
        "limit": limit.unwrap_or(200),
    });
    let result = ListChunks.invoke(&ctx, args).await.map_err(|e| e.to_string())?;
    Ok(result.structured.unwrap_or(Value::Null))
}

#[tauri::command]
pub async fn filter_chunks_cmd(
    state: State<'_, AppState>,
    source_id: String,
    chunk_ids: Vec<String>,
    include: bool,
) -> Result<Value, String> {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let ctx = ToolContext::new(
        state.selected_project.clone(),
        state.project_store.clone(),
        tx,
    );
    let args = json!({ "source_id": source_id, "chunk_ids": chunk_ids, "include": include });
    let result = FilterChunks
        .invoke(&ctx, args)
        .await
        .map_err(|e| e.to_string())?;
    Ok(result.structured.unwrap_or(Value::Null))
}
