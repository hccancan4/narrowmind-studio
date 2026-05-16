//! Shared Tauri state: project store, tool registry, selection, conversation history,
//! pre-built Python runner.
//!
//! `AgentSession` is rebuilt per `agent.send_message` call from the saved conversation
//! history + the shared selected-project lock + the freshly-read provider key. The
//! `python_runner` is built once at startup with `workspace_root` and HF cache env baked in,
//! and cloned into every `ToolContext`.

use std::path::PathBuf;
use std::sync::Arc;

use narrowmind_agent::Message;
use narrowmind_orchestrator::{
    default_registry, hf_cache_env, new_selected_project, ProjectStore, PythonRunner,
    SelectedProject, ToolRegistry,
};
use tokio::sync::Mutex;

/// System prompt the agent sees on every turn.
pub const SYSTEM_PROMPT: &str = include_str!("system_prompt.md");

pub struct AppState {
    pub project_store: Arc<ProjectStore>,
    pub tool_registry: Arc<ToolRegistry>,
    pub selected_project: SelectedProject,
    pub conversation: Mutex<Vec<Message>>,
    pub python_runner: Arc<PythonRunner>,
}

impl AppState {
    pub fn new(project_store: Arc<ProjectStore>, workspace_root: PathBuf) -> Self {
        let registry = Arc::new(default_registry());
        let mut runner = PythonRunner::uv_workspace(workspace_root);
        if let Ok(env) = hf_cache_env() {
            for (k, v) in env {
                runner = runner.with_env(k, v);
            }
        }
        Self {
            project_store,
            tool_registry: registry,
            selected_project: new_selected_project(None),
            conversation: Mutex::new(Vec::new()),
            python_runner: Arc::new(runner),
        }
    }
}
