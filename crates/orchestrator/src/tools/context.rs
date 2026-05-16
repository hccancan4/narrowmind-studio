//! Per-call execution context handed to every tool.
//!
//! The context bundles three things:
//! - the *current* project (if any) — used by file tools to scope reads/writes
//! - the project *store* — used by `create_project` / `list_projects`
//! - an *event sink* — used by long-running tools (`run_command`, ingestion, training)
//!   to stream stdout/stderr/progress notifications back to the UI without buffering

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::mpsc::UnboundedSender;

use crate::project::ProjectStore;

/// Identifies which project a tool call belongs to. The agent loop sets this when the user
/// has selected a project; it stays `None` for "global" tool calls (list/create/select).
#[derive(Debug, Clone)]
pub struct ProjectScope {
    pub name: String,
    pub root: PathBuf,
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

/// Hand-off bundle for one tool invocation.
#[derive(Clone)]
pub struct ToolContext {
    /// Currently-selected project, or `None` if the agent is talking to the user without
    /// having picked one yet (in which case file/run tools should refuse).
    pub project: Option<Arc<ProjectScope>>,
    /// Shared project store — the only handle to projects on disk that tools should use.
    pub project_store: Arc<ProjectStore>,
    /// Channel for streaming progress events. Cloning is cheap (it's an mpsc sender).
    pub events: ToolEventSink,
}

impl ToolContext {
    /// Construct a context. The store is wrapped in `Arc` so every tool clone is cheap.
    #[must_use]
    pub fn new(
        project: Option<ProjectScope>,
        project_store: Arc<ProjectStore>,
        events: ToolEventSink,
    ) -> Self {
        Self {
            project: project.map(Arc::new),
            project_store,
            events,
        }
    }
}
