//! Shared Tauri state: project store, tool registry, selection, conversation history,
//! pre-built Python runner, long-lived inference server manager.
//!
//! `AgentSession` is rebuilt per `agent.send_message` call from the saved conversation
//! history + the shared selected-project lock + the freshly-read provider key. The
//! `python_runner` is built once at startup with `workspace_root` and HF cache env baked in,
//! and cloned into every `ToolContext`. The `inference` manager is also a singleton: at
//! most one llama.cpp server runs per app instance per Phase 3 decision F.

use std::path::PathBuf;
use std::sync::Arc;

use narrowmind_agent::Message;
use narrowmind_orchestrator::{
    default_registry, hf_cache_env, new_selected_project, InferenceManager, ProjectStore,
    PythonRunner, SelectedProject, ToolRegistry, INFERENCE_IDLE_TTL,
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
    pub inference: Arc<InferenceManager>,
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
        let inference = Arc::new(InferenceManager::new());
        // Background watchdog: stops the server after IDLE_TTL of no rag_chat traffic.
        // The handle is intentionally dropped — the task lives for the app lifetime.
        let _watchdog = inference.as_ref().clone().spawn_ttl_watcher(INFERENCE_IDLE_TTL);
        Self {
            project_store,
            tool_registry: registry,
            selected_project: new_selected_project(None),
            conversation: Mutex::new(Vec::new()),
            python_runner: Arc::new(runner),
            inference,
        }
    }
}
