//! Project lifecycle commands invoked by the left rail.

use serde::Serialize;
use tauri::State;

use narrowmind_orchestrator::{ProjectStatus, ProjectTier, ProviderConfig};

use crate::state::AppState;

/// Lightweight summary used by the left rail. Avoid serialising the full `ProjectConfig` so
/// the rail can render without hitting per-project file I/O for every render.
#[derive(Debug, Serialize)]
pub struct ProjectSummary {
    pub name: String,
    pub status: String,
    pub tier: String,
}

#[tauri::command]
pub async fn list_projects(state: State<'_, AppState>) -> Result<Vec<ProjectSummary>, String> {
    let names = state.project_store.list().map_err(|e| e.to_string())?;
    let mut summaries = Vec::with_capacity(names.len());
    for name in names {
        match state.project_store.get(&name) {
            Ok(cfg) => summaries.push(ProjectSummary {
                name: cfg.name,
                status: format!("{:?}", cfg.status).to_lowercase(),
                tier: format!("{:?}", cfg.tier).to_lowercase(),
            }),
            Err(_) => summaries.push(ProjectSummary {
                name,
                status: "unknown".into(),
                tier: "unknown".into(),
            }),
        }
    }
    Ok(summaries)
}

#[tauri::command]
pub async fn create_project(
    state: State<'_, AppState>,
    name: String,
    tier: Option<String>,
    provider: Option<String>,
    model: Option<String>,
) -> Result<ProjectSummary, String> {
    let tier = match tier.as_deref() {
        None | Some("" | "rag") => ProjectTier::Rag,
        Some("lora") => ProjectTier::Lora,
        Some("hybrid") => ProjectTier::Hybrid,
        Some(other) => return Err(format!("unknown tier `{other}` (expected rag | lora | hybrid)")),
    };
    let provider = ProviderConfig {
        name: provider.unwrap_or_else(|| "anthropic".into()),
        model: model.unwrap_or_else(|| "claude-opus-4-7".into()),
    };
    let cfg = state
        .project_store
        .create(&name, tier, provider)
        .map_err(|e| e.to_string())?;
    Ok(ProjectSummary {
        name: cfg.name,
        status: format!("{:?}", cfg.status).to_lowercase(),
        tier: format!("{:?}", cfg.tier).to_lowercase(),
    })
}

#[tauri::command]
pub async fn delete_project(state: State<'_, AppState>, name: String) -> Result<(), String> {
    state
        .project_store
        .delete(&name)
        .map_err(|e| e.to_string())?;
    // If the deleted project was selected, clear the selection so the next agent message
    // doesn't try to sandbox into a missing directory.
    let mut sel = state.selected_project.lock().await;
    if sel.as_deref() == Some(&name) {
        *sel = None;
        // Drop the conversation history too — it refers to a project that no longer exists.
        state.conversation.lock().await.clear();
    }
    Ok(())
}

#[tauri::command]
pub async fn select_project(state: State<'_, AppState>, name: String) -> Result<(), String> {
    // Validate the project exists before flipping the selection — a typo in the UI should
    // surface immediately, not on the next agent turn.
    let _cfg = state.project_store.get(&name).map_err(|e| e.to_string())?;
    let mut sel = state.selected_project.lock().await;
    let switching_away = sel.as_deref() != Some(&name);
    *sel = Some(name);
    if switching_away {
        // Conversation history is per-project: switching projects starts a new conversation
        // so the model doesn't see tool_use ids from a previous sandbox.
        state.conversation.lock().await.clear();
    }
    Ok(())
}

#[tauri::command]
pub async fn current_project(state: State<'_, AppState>) -> Result<Option<String>, String> {
    Ok(state.selected_project.lock().await.clone())
}

#[tauri::command]
pub async fn project_status(
    state: State<'_, AppState>,
    name: String,
) -> Result<ProjectSummary, String> {
    let cfg = state.project_store.get(&name).map_err(|e| e.to_string())?;
    Ok(ProjectSummary {
        name: cfg.name,
        status: format!("{:?}", cfg.status).to_lowercase(),
        tier: format!("{:?}", cfg.tier).to_lowercase(),
    })
}

#[allow(dead_code)] // re-exported for completeness; the UI doesn't yet expose archive
pub const _STATUS_ARCHIVED: ProjectStatus = ProjectStatus::Archived;
