//! Agent loop driver: `agent_send_message` and `agent_reset`.

use std::sync::Arc;

use narrowmind_agent::{AgentEvent, AgentSession, AnthropicProvider, Provider};
use narrowmind_orchestrator::{OrchestratorDispatcher, SecretStore, ToolContext};
use tauri::{AppHandle, Emitter, State};

use crate::state::{AppState, SYSTEM_PROMPT};

pub const AGENT_EVENT: &str = "agent:event";
pub const TOOL_EVENT: &str = "agent:tool";

#[tauri::command]
pub async fn agent_send_message(
    app: AppHandle,
    state: State<'_, AppState>,
    message: String,
) -> Result<(), String> {
    if message.trim().is_empty() {
        return Err("empty message".into());
    }

    // Provider key from keychain.
    let api_key = SecretStore::get_provider_key("anthropic")
        .map_err(|e| format!("could not read Anthropic key: {e}"))?
        .ok_or("Anthropic API key not set — open Settings and paste your key first")?;

    let provider: Arc<dyn Provider> = Arc::new(
        AnthropicProvider::builder()
            .api_key(api_key)
            .build()
            .map_err(|e| e.to_string())?,
    );

    // Tool event sink for live stdout/stderr streaming.
    let (tool_tx, mut tool_rx) = tokio::sync::mpsc::unbounded_channel();

    // ToolContext shares the SAME selected_project lock as AppState — so create_project
    // (auto-select) and rail clicks both update one source of truth, and every tool sees
    // the current project at invoke time rather than at session-start time.
    let tool_ctx = ToolContext::new(
        state.selected_project.clone(),
        state.project_store.clone(),
        tool_tx,
    );
    let dispatcher = Arc::new(OrchestratorDispatcher::new(
        state.tool_registry.clone(),
        tool_ctx,
    ));

    let app_for_tools = app.clone();
    let tool_pump = tokio::spawn(async move {
        while let Some(ev) = tool_rx.recv().await {
            let _ = app_for_tools.emit(TOOL_EVENT, ev);
        }
    });

    let initial = state.conversation.lock().await.clone();
    let (agent_tx, mut agent_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let mut session = AgentSession::with_messages(
        provider,
        dispatcher,
        Some(SYSTEM_PROMPT.to_string()),
        agent_tx,
        initial,
    );

    let app_for_agent = app.clone();
    let agent_pump = tokio::spawn(async move {
        while let Some(ev) = agent_rx.recv().await {
            let _ = app_for_agent.emit(AGENT_EVENT, ev);
        }
    });

    let result = session.send_user_message(message).await;

    // Persist post-turn conversation even on error so a partial transcript survives.
    {
        let mut conv = state.conversation.lock().await;
        *conv = session.messages().to_vec();
    }

    drop(session);
    let _ = tool_pump.await;
    let _ = agent_pump.await;

    result.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn agent_reset(state: State<'_, AppState>) -> Result<(), String> {
    state.conversation.lock().await.clear();
    Ok(())
}

#[tauri::command]
pub async fn agent_turn_count(state: State<'_, AppState>) -> Result<usize, String> {
    Ok(state.conversation.lock().await.len())
}
