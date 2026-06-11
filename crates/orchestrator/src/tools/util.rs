//! Small helpers shared by long-running tools (synth_gen, run_command,
//! Phase 4 training): durable per-project audit logging and live progress
//! events. Extracted from `synth_gen` once a second consumer appeared.

use std::fs;
use std::io::Write as _;
use std::path::Path;

use super::context::{ToolEvent, ToolEventSink};

/// Append one timestamped line to `<project_root>/agent.log`.
///
/// This is the project's durable audit trail — distinct from `tracing` (dev
/// console) and `ToolEvent` (live UI): it survives app restarts and answers
/// "what did the agent actually do in this project" after the fact. Errors
/// are swallowed on purpose; logging must never fail the tool call itself.
pub(crate) fn log_to_agent_log(project_root: &Path, msg: &str) {
    let path = project_root.join("agent.log");
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let line = format!(
            "[{}] {msg}\n",
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
        );
        let _ = f.write_all(line.as_bytes());
    }
}

/// Fire-and-forget a live progress line to the UI event stream.
pub(crate) fn emit_progress(events: &ToolEventSink, message: &str) {
    let _ = events.send(ToolEvent::Progress {
        message: message.to_string(),
    });
}
