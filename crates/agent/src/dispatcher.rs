//! `ToolDispatcher` — the abstraction the agent loop uses to invoke tools.
//!
//! The loop never sees orchestrator types directly: it asks the dispatcher for the current
//! tool defs (handed to the provider so the LLM knows what's available) and asks the
//! dispatcher to run a specific tool when the provider emits a `ToolCall`. The orchestrator
//! supplies a concrete impl (`OrchestratorDispatcher`) that wires this into its
//! filesystem-backed `ToolRegistry` and per-call `ToolContext`.

use async_trait::async_trait;
use serde_json::Value;

use crate::types::ToolDef;

/// Outcome of a single dispatched tool call. The orchestrator's tool framework has its own
/// `ToolError` / `ToolResult` types; the dispatcher flattens both into this shape because
/// the agent loop only needs to know "what string goes into the next `tool_result` block"
/// and "should it carry `is_error: true`".
#[derive(Debug, Clone)]
pub struct ToolDispatchOutcome {
    pub content: String,
    pub is_error: bool,
}

impl ToolDispatchOutcome {
    #[must_use]
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    #[must_use]
    pub fn err(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

#[async_trait]
pub trait ToolDispatcher: Send + Sync {
    /// Current tool schema list, in the same shape the provider expects. Called once per
    /// agent turn so the model always sees an up-to-date catalogue.
    fn defs(&self) -> Vec<ToolDef>;

    /// Execute one tool call. `args` is the raw JSON `input` from the provider's `ToolCall`
    /// event. Implementations must never panic — convert every failure into
    /// `ToolDispatchOutcome::err(...)`.
    async fn dispatch(&self, name: &str, args: Value) -> ToolDispatchOutcome;
}
