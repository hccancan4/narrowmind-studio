//! `ingest_source` — pull text into the current project from a source description.
//!
//! Phase 2 M1 ships the `local` source type, which routes to the Python ingestion worker's
//! `ingestion.local_path` RPC method. M2 will add `wikipedia`, `url`, and `hf_dataset`
//! source types behind the same tool name — schema gains those discriminator values then.

use std::fmt::Write as _;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::info;

use super::context::ToolContext;
use super::registry::{Tool, ToolDef, ToolError, ToolResult};
use crate::worker::{call_worker, WorkerCommand};

/// Per-call timeout. Local-dir ingestion of a few hundred mixed PDF/DOCX/EPUB files can
/// stretch past a minute on cold caches; 10 minutes is a forgiving ceiling.
const INGEST_TIMEOUT_SECS: u64 = 600;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum IngestArgs {
    /// Pull text out of a file or directory on disk.
    Local {
        /// Absolute path to a file or directory. Recursed when a directory.
        path: String,
        /// Optional fixed id. Auto-generated when omitted.
        #[serde(default)]
        source_id: Option<String>,
    },
}

pub struct IngestSource;

#[async_trait]
impl Tool for IngestSource {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "ingest_source".into(),
            description: "Pull text into the current project from a source. v0.2 supports \
                          local files and directories (PDF / TXT / MD / EPUB / DOCX / HTML); \
                          Wikipedia, URL crawling, and HuggingFace datasets land in the next \
                          milestone. Writes <project>/sources/<id>/documents.jsonl + source.json. \
                          Per-file failures are recorded in source.json without aborting."
                .into(),
            input_schema: json!({
                "type": "object",
                "oneOf": [
                    {
                        "title": "local",
                        "type": "object",
                        "properties": {
                            "type": { "const": "local" },
                            "path": { "type": "string", "description": "Absolute path to a file or directory." },
                            "source_id": { "type": "string", "description": "Optional fixed source id." }
                        },
                        "required": ["type", "path"]
                    }
                ]
            }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, args: Value) -> Result<ToolResult, ToolError> {
        let args: IngestArgs = serde_json::from_value(args).map_err(|e| ToolError::BadInput {
            tool: "ingest_source".into(),
            reason: e.to_string(),
        })?;
        let project = ctx.current_project().await.ok_or(ToolError::NoProject)?;
        let runner = ctx
            .python_runner
            .as_ref()
            .ok_or_else(|| ToolError::Exec {
                tool: "ingest_source".into(),
                message: "PythonRunner not configured on ToolContext".into(),
            })?
            .clone();

        match args {
            IngestArgs::Local { path, source_id } => {
                let params = json!({
                    "project_root": project.root,
                    "target": path,
                    "source_id": source_id,
                });
                let cmd = WorkerCommand {
                    module: "narrowmind_workers.ingestion".into(),
                    method: "ingestion.local_path".into(),
                    params,
                    timeout: Some(Duration::from_secs(INGEST_TIMEOUT_SECS)),
                };
                let value = call_worker(&runner, &cmd).await.map_err(|e| ToolError::Exec {
                    tool: "ingest_source".into(),
                    message: e.to_string(),
                })?;
                Ok(format_outcome(&value))
            }
        }
    }
}

/// Render the manifest JSON the worker returns as something the model can act on, plus
/// a structured copy for the UI.
fn format_outcome(manifest: &Value) -> ToolResult {
    let source_id = manifest.get("source_id").and_then(Value::as_str).unwrap_or("?");
    let doc_count = manifest.get("document_count").and_then(Value::as_u64).unwrap_or(0);
    let fail_count = manifest.get("failure_count").and_then(Value::as_u64).unwrap_or(0);

    let mut text = String::new();
    let _ = writeln!(text, "source `{source_id}`: ingested {doc_count} documents, {fail_count} failures");
    if let Some(failures) = manifest.get("failures").and_then(Value::as_array) {
        for f in failures.iter().take(5) {
            let path = f.get("path").and_then(Value::as_str).unwrap_or("?");
            let err = f.get("error").and_then(Value::as_str).unwrap_or("?");
            let _ = writeln!(text, "  - {path}: {err}");
        }
        if failures.len() > 5 {
            let _ = writeln!(text, "  … {} more failures (see source.json)", failures.len() - 5);
        }
    }
    info!(source_id, doc_count, fail_count, "ingest_source");
    ToolResult::text(text).with_structured(manifest.clone())
}
