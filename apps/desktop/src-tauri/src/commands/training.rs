//! Training Monitor Tauri commands (Phase 4 M4).
//!
//! Read paths go straight to the run directory files — filesystem is the
//! source of truth, so the Monitor works identically for the live run and
//! for history after an app restart. Live updates ride the "training:event"
//! Tauri event (forwarded from TrainingManager's broadcast in lib.rs);
//! these commands cover initial load + history browsing.

#![allow(clippy::needless_pass_by_value)]

use serde_json::{json, Value};
use tauri::State;

use crate::state::AppState;

/// Live manager snapshot (active run id, step counters).
#[tauri::command]
pub async fn training_status(state: State<'_, AppState>) -> Result<Value, String> {
    let s = state.training.status().await;
    serde_json::to_value(&s).map_err(|e| e.to_string())
}

/// All runs of the selected project (runs/*/status.json, newest first).
#[tauri::command]
pub async fn training_runs(state: State<'_, AppState>) -> Result<Value, String> {
    let project = state
        .selected_project
        .lock()
        .await
        .clone()
        .ok_or_else(|| "no project selected".to_string())?;
    let runs_dir = project.root.join("runs");
    let mut runs: Vec<Value> = std::fs::read_dir(&runs_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            std::fs::read_to_string(e.path().join("status.json"))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
        })
        .collect();
    runs.sort_by(|a, b| {
        let ka = a.get("started_at").and_then(Value::as_str).unwrap_or("");
        let kb = b.get("started_at").and_then(Value::as_str).unwrap_or("");
        kb.cmp(ka)
    });
    Ok(json!({ "runs": runs }))
}

/// Full metric history for one run (metrics.jsonl, parsed). The Monitor
/// loads this once on mount / run switch, then appends live events — which
/// is what makes history visible after an app restart (acceptance c).
#[tauri::command]
pub async fn training_metrics(
    state: State<'_, AppState>,
    run_id: String,
) -> Result<Value, String> {
    let project = state
        .selected_project
        .lock()
        .await
        .clone()
        .ok_or_else(|| "no project selected".to_string())?;
    let path = project.root.join("runs").join(&run_id).join("metrics.jsonl");
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let metrics: Vec<Value> = content
        .lines()
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .collect();
    Ok(json!({ "run_id": run_id, "metrics": metrics }))
}

/// Last N lines of a run's train.log for the log-tail pane.
#[tauri::command]
pub async fn training_log_tail(
    state: State<'_, AppState>,
    run_id: String,
    lines: Option<usize>,
) -> Result<Value, String> {
    let project = state
        .selected_project
        .lock()
        .await
        .clone()
        .ok_or_else(|| "no project selected".to_string())?;
    let path = project.root.join("runs").join(&run_id).join("train.log");
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let n = lines.unwrap_or(50);
    let all: Vec<&str> = content.lines().collect();
    let tail: Vec<&str> = all.iter().rev().take(n).rev().copied().collect();
    Ok(json!({ "run_id": run_id, "lines": tail }))
}

/// Cancel the active run from the UI (same path as the stop_training tool).
#[tauri::command]
pub async fn training_stop(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(state.training.stop().await)
}
