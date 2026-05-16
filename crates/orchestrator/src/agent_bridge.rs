//! Glue between the orchestrator's tool framework and the agent loop.
//!
//! `OrchestratorDispatcher` implements [`narrowmind_agent::ToolDispatcher`] by routing every
//! `dispatch` call through a [`ToolRegistry`] with a stored [`ToolContext`]. The agent loop
//! holds an `Arc<dyn ToolDispatcher>`; the desktop crate constructs an
//! `OrchestratorDispatcher` per session and hands it in.

use std::sync::Arc;

use async_trait::async_trait;
use narrowmind_agent::{ToolDef, ToolDispatchOutcome, ToolDispatcher};
use serde_json::Value;
use tracing::warn;

use crate::tools::{ToolContext, ToolRegistry};

/// Concrete `ToolDispatcher` wrapping the orchestrator's `ToolRegistry`. Holding the context
/// inside the dispatcher means every dispatched call sees the same project scope + event
/// sink for the lifetime of one agent session.
pub struct OrchestratorDispatcher {
    registry: Arc<ToolRegistry>,
    context: ToolContext,
}

impl OrchestratorDispatcher {
    #[must_use]
    pub fn new(registry: Arc<ToolRegistry>, context: ToolContext) -> Self {
        Self { registry, context }
    }
}

#[async_trait]
impl ToolDispatcher for OrchestratorDispatcher {
    fn defs(&self) -> Vec<ToolDef> {
        self.registry.defs()
    }

    async fn dispatch(&self, name: &str, args: Value) -> ToolDispatchOutcome {
        match self.registry.dispatch(name, &self.context, args).await {
            Ok(result) => ToolDispatchOutcome::ok(result.content),
            Err(e) => {
                warn!(tool = name, error = %e, "tool dispatch returned error to agent loop");
                ToolDispatchOutcome::err(e.to_string())
            }
        }
    }
}

/// Build a `ToolRegistry` pre-populated with the Phase 1 v0 tool set.
#[must_use]
pub fn default_registry() -> ToolRegistry {
    use crate::tools::fs::{ListDir, ReadFile, WriteFile};
    use crate::tools::projects::{CreateProject, ListProjects, ProjectStatus};
    use crate::tools::run_command::RunCommand;

    let mut r = ToolRegistry::new();
    r.register(Arc::new(ReadFile));
    r.register(Arc::new(WriteFile));
    r.register(Arc::new(ListDir));
    r.register(Arc::new(RunCommand));
    r.register(Arc::new(ProjectStatus));
    r.register(Arc::new(CreateProject));
    r.register(Arc::new(ListProjects));
    r
}
