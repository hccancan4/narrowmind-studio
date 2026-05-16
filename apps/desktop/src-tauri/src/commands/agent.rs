//! Agent loop driver: `agent_send_message` and `agent_reset`.
//!
//! Each `send_message` call rebuilds the `AgentSession` from the stored conversation history,
//! the currently-selected project (which fixes the tool sandbox scope), and the freshly-read
//! provider key. The session's event stream is forwarded to the front-end as Tauri events.

use std::sync::Arc;

use narrowmind_agent::{AgentEvent, AgentSession, AnthropicProvider, Provider};
use narrowmind_orchestrator::{
    OrchestratorDispatcher, ProjectScope, SecretStore, ToolContext,
};
use tauri::{AppHandle, Emitter, State};

use crate::state::{AppState, SYSTEM_PROMPT};

/// Event channel name the front-end subscribes to for streaming assistant output.
pub const AGENT_EVENT: &str = "agent:event";
/// Event channel name the front-end subscribes to for live tool stdout/stderr lines.
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

    // 1. Resolve current project (required — without a project, sandbox tools refuse).
    let (project_name, project_root) = state
        .current_project_root()
        .await
        .ok_or("no project selected — pick one in the left rail (or ask the agent to create one)")?;

    // 2. Load provider key from the OS keychain.
    let api_key = SecretStore::get_provider_key("anthropic")
        .map_err(|e| format!("could not read Anthropic key: {e}"))?
        .ok_or("Anthropic API key not set — open Settings and paste your key first")?;

    // 3. Build provider.
    let provider: Arc<dyn Provider> = Arc::new(
        AnthropicProvider::builder()
            .api_key(api_key)
            .build()
            .map_err(|e| e.to_string())?,
    );

    // 4. Build a ToolContext with the project scope + a fresh tool event channel.
    let (tool_tx, mut tool_rx) = tokio::sync::mpsc::unbounded_channel();
    let scope = ProjectScope {
        name: project_name.clone(),
        root: project_root.clone(),
    };
    let tool_ctx = ToolContext::new(
        Some(scope),
        state.project_store.clone(),
        tool_tx,
    );
    let dispatcher = Arc::new(OrchestratorDispatcher::new(
        state.tool_registry.clone(),
        tool_ctx,
    ));

    // 5. Pump tool events to the front-end while the agent runs.
    let app_for_tools = app.clone();
    let tool_pump = tokio::spawn(async move {
        while let Some(ev) = tool_rx.recv().await {
            let _ = app_for_tools.emit(TOOL_EVENT, ev);
        }
    });

    // 6. Build the agent session from the stored history.
    let initial = state.conversation.lock().await.clone();
    let (agent_tx, mut agent_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let mut session = AgentSession::with_messages(
        provider,
        dispatcher,
        Some(SYSTEM_PROMPT.to_string()),
        agent_tx,
        initial,
    );

    // 7. Pump agent events to the front-end while the agent runs.
    let app_for_agent = app.clone();
    let agent_pump = tokio::spawn(async move {
        while let Some(ev) = agent_rx.recv().await {
            let _ = app_for_agent.emit(AGENT_EVENT, ev);
        }
    });

    // 8. Drive the turn. The agent fully consumes the provider stream and any tool calls
    //    before returning; we use ? rather than spawning so completion order is well-defined.
    let result = session.send_user_message(message).await;

    // 9. Persist the conversation back into shared state — even on error, so the user can
    //    see the partial transcript in the next render.
    {
        let mut conv = state.conversation.lock().await;
        *conv = session.messages().to_vec();
    }

    // 10. Drop the session, which drops the inner mpsc senders. Drain the pumps so they
    //     unblock cleanly before this command returns.
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
