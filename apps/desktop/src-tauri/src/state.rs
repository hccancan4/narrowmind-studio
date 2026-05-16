//! Shared Tauri state: project store, tool registry, selection, conversation history.
//!
//! `AgentSession` itself is *not* stored: we rebuild it per `agent.send_message` call from
//! the saved conversation history, the shared selected-project lock, and the freshly-read
//! provider key. The `selected_project` lock is wired into every tool's `ToolContext` so
//! `create_project` can auto-select and the next tool in the same turn sees the change.

use std::sync::Arc;

use narrowmind_agent::Message;
use narrowmind_orchestrator::{
    default_registry, new_selected_project, ProjectStore, SelectedProject, ToolRegistry,
};
use tokio::sync::Mutex;

/// System prompt the agent sees on every turn.
pub const SYSTEM_PROMPT: &str = include_str!("system_prompt.md");

pub struct AppState {
    pub project_store: Arc<ProjectStore>,
    pub tool_registry: Arc<ToolRegistry>,
    /// The single source of truth for which project is "current". Cloned into every
    /// `ToolContext`; rail clicks and `create_project` both write here.
    pub selected_project: SelectedProject,
    /// Conversation history for the active agent session.
    pub conversation: Mutex<Vec<Message>>,
}

impl AppState {
    pub fn new(project_store: Arc<ProjectStore>) -> Self {
        let registry = Arc::new(default_registry());
        Self {
            project_store,
            tool_registry: registry,
            selected_project: new_selected_project(None),
            conversation: Mutex::new(Vec::new()),
        }
    }
}
