//! Shared Tauri state: project store, tool registry, conversation history.
//!
//! `AgentSession` itself is *not* stored: we rebuild it per `agent.send_message` call from
//! the saved conversation history, the currently-selected project (which determines tool
//! sandbox scope), and the freshly-read provider key. This means a user switching projects
//! or rotating their API key takes effect on the next message without explicit reset logic.

use std::path::PathBuf;
use std::sync::Arc;

use narrowmind_agent::Message;
use narrowmind_orchestrator::{default_registry, ProjectStore, ToolRegistry};
use tokio::sync::Mutex;

/// System prompt the agent sees on every turn. Kept short, with explicit guidance about the
/// available tool surface and the studio's sandboxing model.
pub const SYSTEM_PROMPT: &str = include_str!("system_prompt.md");

/// Long-lived application state, shared across Tauri commands via `tauri::State`.
pub struct AppState {
    pub project_store: Arc<ProjectStore>,
    pub tool_registry: Arc<ToolRegistry>,
    /// Currently-selected project (name only — the store resolves it to a path on demand).
    pub selected_project: Mutex<Option<String>>,
    /// Conversation history for the active agent session.
    pub conversation: Mutex<Vec<Message>>,
}

impl AppState {
    pub fn new(project_store: Arc<ProjectStore>) -> Self {
        let registry = Arc::new(default_registry());
        Self {
            project_store,
            tool_registry: registry,
            selected_project: Mutex::new(None),
            conversation: Mutex::new(Vec::new()),
        }
    }

    /// Resolve the currently selected project name (if any) to an absolute path on disk.
    pub async fn current_project_root(&self) -> Option<(String, PathBuf)> {
        let guard = self.selected_project.lock().await;
        guard
            .as_ref()
            .map(|name| (name.clone(), self.project_store.project_dir(name)))
    }
}
