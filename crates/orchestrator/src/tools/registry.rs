//! Tool trait, error type, in-memory registry, and the dispatch entry point.
//!
//! Tools are async, project-scoped, and emit progress events through their
//! [`ToolContext`]. They never panic; every failure mode is a `ToolError` variant.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tracing::{debug, warn};

use super::context::ToolContext;
use crate::project::ProjectError;

// Tool schema lives in the agent crate so providers, dispatchers, and orchestrator tools
// all agree on a single source of truth. Re-export here so callers can keep importing
// from `narrowmind_orchestrator::tools::ToolDef`.
pub use narrowmind_agent::ToolDef;

/// Successful tool output. `content` is the human-readable string the model will see in the
/// `tool_result` block; `structured` is JSON the UI / orchestrator may use for richer rendering,
/// and `exit_code` is populated by process-running tools so the model can see the raw status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

impl ToolResult {
    #[must_use]
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            structured: None,
            exit_code: None,
        }
    }

    #[must_use]
    pub fn with_structured(mut self, structured: Value) -> Self {
        self.structured = Some(structured);
        self
    }

    #[must_use]
    pub fn with_exit_code(mut self, code: i32) -> Self {
        self.exit_code = Some(code);
        self
    }
}

/// Anything that can go wrong calling a tool. The agent loop converts each variant into a
/// `tool_result` block with `is_error = true` so the model sees the failure verbatim.
#[derive(Debug, Error)]
pub enum ToolError {
    #[error("unknown tool `{0}`")]
    Unknown(String),

    #[error("invalid arguments for `{tool}`: {reason}")]
    BadInput { tool: String, reason: String },

    #[error("this tool requires a selected project; none is set")]
    NoProject,

    #[error("path `{0}` escapes the project sandbox")]
    PathEscape(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Project(#[from] ProjectError),

    #[error("tool `{tool}` failed: {message}")]
    Exec { tool: String, message: String },

    #[error("tool `{tool}` timed out after {seconds}s")]
    Timeout { tool: String, seconds: u64 },
}

impl ToolError {
    /// Render this error as a `tool_result` payload that's safe to send back to the model.
    /// Sensitive paths are kept (they're inside the project sandbox by construction) but the
    /// raw `Debug` is not — that's logged separately.
    #[must_use]
    pub fn to_tool_result(&self) -> ToolResult {
        ToolResult::text(self.to_string())
    }
}

/// One callable tool. Implementations are stateless: the registry holds an `Arc<dyn Tool>`
/// and every invocation gets a fresh [`ToolContext`].
#[async_trait]
pub trait Tool: Send + Sync {
    /// Static schema describing the tool to the LLM. Called once per agent turn.
    fn def(&self) -> ToolDef;

    /// Execute the tool. `args` is the raw JSON object the model produced — the impl is
    /// responsible for deserialising and validating it.
    async fn invoke(&self, ctx: &ToolContext, args: Value) -> Result<ToolResult, ToolError>;
}

/// In-memory map of tool name to implementation. Cheap to clone (each entry is an `Arc`).
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tools: BTreeMap::new(),
        }
    }

    /// Add a tool. Subsequent registrations under the same name overwrite — the agent loop
    /// is expected to build a fresh registry per session, so this is a deliberate choice.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.def().name;
        debug!(tool = %name, "registering tool");
        self.tools.insert(name, tool);
    }

    /// Snapshot of every registered tool's definition, for handing to the provider.
    #[must_use]
    pub fn defs(&self) -> Vec<ToolDef> {
        self.tools.values().map(|t| t.def()).collect()
    }

    /// Sorted list of registered tool names — handy for logging / debugging.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// Dispatch a single tool call. Errors and successful results both flow back to the
    /// caller — the agent loop converts errors into `tool_result` blocks with `is_error`.
    pub async fn dispatch(
        &self,
        name: &str,
        ctx: &ToolContext,
        args: Value,
    ) -> Result<ToolResult, ToolError> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| ToolError::Unknown(name.to_string()))?;
        match tool.invoke(ctx, args).await {
            Ok(r) => Ok(r),
            Err(e) => {
                warn!(tool = %name, error = %e, "tool invocation failed");
                Err(e)
            }
        }
    }
}
