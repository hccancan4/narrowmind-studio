//! Per-call execution context handed to every tool.
//!
//! The context bundles three things:
//! - the *currently-selected* project, read at invoke time so a tool can act on a project
//!   that was created earlier in the same agent turn
//! - the project *store* — used by `create_project` / `list_projects`
//! - an *event sink* — used by long-running tools (`run_command`, ingestion, training)
//!   to stream stdout/stderr/progress notifications back to the UI without buffering

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{mpsc::UnboundedSender, Mutex};

use crate::project::ProjectStore;
use crate::worker::PythonRunner;

/// Identifies which project a tool call belongs to. Cheap to clone.
#[derive(Debug, Clone)]
pub struct ProjectScope {
    pub name: String,
    pub root: PathBuf,
}

/// Shared, mutable selection. The desktop layer holds the *only* `Arc` to one of these and
/// passes a clone into every `ToolContext`. When the agent calls `create_project` (or the
/// user clicks a project in the rail), the same lock is updated and the very next tool sees
/// the change.
pub type SelectedProject = Arc<Mutex<Option<ProjectScope>>>;

#[must_use]
pub fn new_selected_project(initial: Option<ProjectScope>) -> SelectedProject {
    Arc::new(Mutex::new(initial))
}

/// Stream event emitted by a tool while executing. The orchestrator forwards each event
/// straight to the UI as a Tauri event so users see live output (think `tail -f`) rather
/// than waiting for the whole tool call to finish.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", content = "data")]
pub enum ToolEvent {
    Stdout(String),
    Stderr(String),
    Progress { message: String },
}

/// Sink the agent loop reads from; tool implementations clone this and push events.
pub type ToolEventSink = UnboundedSender<ToolEvent>;

/// Hand-off bundle for one tool invocation. Clone is cheap (Arc + sender).
#[derive(Clone)]
pub struct ToolContext {
    /// Shared selection. Tools read at invoke time via [`current_project`].
    pub selected: SelectedProject,
    /// Shared project store — the only handle to projects on disk that tools should use.
    pub project_store: Arc<ProjectStore>,
    /// Channel for streaming progress events.
    pub events: ToolEventSink,
    /// Pre-built runner for spawning Python sidecar workers. Set at app startup
    /// (`workspace_root` + HF cache env baked in). `None` only in unit tests for tools that
    /// don't need a worker — fs and project tools never touch this field.
    pub python_runner: Option<Arc<PythonRunner>>,
}

impl ToolContext {
    #[must_use]
    pub fn new(
        selected: SelectedProject,
        project_store: Arc<ProjectStore>,
        events: ToolEventSink,
    ) -> Self {
        Self {
            selected,
            project_store,
            events,
            python_runner: None,
        }
    }

    /// Attach a `PythonRunner` so worker-spawning tools (`ingest_source`, ...) can use it.
    #[must_use]
    pub fn with_python_runner(mut self, runner: Arc<PythonRunner>) -> Self {
        self.python_runner = Some(runner);
        self
    }

    /// Snapshot the currently-selected project at this exact moment.
    /// Returns `None` if no project has been selected yet — file/run tools should refuse.
    pub async fn current_project(&self) -> Option<ProjectScope> {
        self.selected.lock().await.clone()
    }

    /// Replace the selection. Called by `create_project` (auto-select on create) and by
    /// the desktop layer when the user clicks a different project in the rail.
    pub async fn set_project(&self, scope: Option<ProjectScope>) {
        *self.selected.lock().await = scope;
    }
}
