//! `ingest_source` — pull text into the current project from a source description.
//!
//! Phase 2 M1 ships the `local` source type, which routes to the Python ingestion worker's
//! `ingestion.local_path` RPC method. M2 will add `wikipedia`, `url`, and `hf_dataset`
//! source types behind the same tool name — schema gains those discriminator values then.

use std::fmt::Write as _;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::info;

use super::context::ToolContext;
use super::registry::{Tool, ToolDef, ToolError, ToolResult};
use crate::error::WorkerError;
use crate::retry::{timeouts, with_backoff, BackoffPolicy, Retry};
use crate::worker::{call_worker, WorkerCommand};

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
    /// Walk a Wikipedia category up to `max_depth` sub-categories, capped at `max_pages`.
    Wikipedia {
        /// Category name without the `Category:` prefix.
        category: String,
        #[serde(default = "default_language")]
        language: String,
        #[serde(default = "default_wikipedia_max_depth")]
        max_depth: u32,
        #[serde(default = "default_wikipedia_max_pages")]
        max_pages: u32,
        #[serde(default)]
        source_id: Option<String>,
    },
    /// Fetch one URL or BFS-crawl from a seed URL.
    Url {
        url: String,
        #[serde(default)]
        max_depth: u32,
        #[serde(default = "default_url_max_pages")]
        max_pages: u32,
        #[serde(default = "default_true")]
        same_domain_only: bool,
        #[serde(default)]
        source_id: Option<String>,
    },
}

fn default_language() -> String {
    "en".into()
}
fn default_wikipedia_max_depth() -> u32 {
    1
}
fn default_wikipedia_max_pages() -> u32 {
    50
}
fn default_url_max_pages() -> u32 {
    1
}
fn default_true() -> bool {
    true
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
            // Anthropic tool input_schema does not support oneOf/allOf/anyOf at the top
            // level (returns 400 invalid_request_error). Flatten into one object with every
            // possible field; runtime serde deserialisation (the tagged IngestArgs enum
            // below) still enforces which fields are required for which `type`. Per-type
            // requirements live in the description so the model knows.
            input_schema: json!({
                "type": "object",
                "properties": {
                    "type": {
                        "type": "string",
                        "enum": ["local", "wikipedia", "url"],
                        "description": "Source kind. Required."
                    },
                    "path": {
                        "type": "string",
                        "description": "type=local only. Absolute path to a file or directory; dirs walked recursively."
                    },
                    "category": {
                        "type": "string",
                        "description": "type=wikipedia only. Category name without the 'Category:' prefix."
                    },
                    "language": {
                        "type": "string",
                        "default": "en",
                        "description": "type=wikipedia only. ISO 639-1 code."
                    },
                    "max_depth": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 5,
                        "description": "wikipedia: sub-category depth (default 1, max 5). url: BFS crawl depth (default 0, max 3)."
                    },
                    "max_pages": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "wikipedia: max articles (default 50). url: max pages crawled (default 1)."
                    },
                    "url": {
                        "type": "string",
                        "description": "type=url only. Absolute http(s) URL."
                    },
                    "same_domain_only": {
                        "type": "boolean",
                        "default": true,
                        "description": "type=url only. Restrict crawl to seed URL's domain."
                    },
                    "source_id": {
                        "type": "string",
                        "description": "Optional fixed source id. Auto-generated when omitted."
                    }
                },
                "required": ["type"]
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

        let (method, params) = match args {
            IngestArgs::Local { path, source_id } => (
                "ingestion.local_path",
                json!({
                    "project_root": project.root,
                    "target": path,
                    "source_id": source_id,
                }),
            ),
            IngestArgs::Wikipedia {
                category,
                language,
                max_depth,
                max_pages,
                source_id,
            } => (
                "ingestion.wikipedia",
                json!({
                    "project_root": project.root,
                    "category": category,
                    "language": language,
                    "max_depth": max_depth,
                    "max_pages": max_pages,
                    "source_id": source_id,
                }),
            ),
            IngestArgs::Url {
                url,
                max_depth,
                max_pages,
                same_domain_only,
                source_id,
            } => (
                "ingestion.url",
                json!({
                    "project_root": project.root,
                    "url": url,
                    "max_depth": max_depth,
                    "max_pages": max_pages,
                    "same_domain_only": same_domain_only,
                    "source_id": source_id,
                }),
            ),
        };

        let cmd = WorkerCommand {
            module: "narrowmind_workers.ingestion".into(),
            method: method.into(),
            params,
            timeout: Some(timeouts::INGEST),
        };
        // Ingestion is idempotent (sources/<id>/ files are written atomically via
        // .tmp swaps), so re-running the whole job after a process-level failure
        // is safe. One retry only — re-running a multi-minute scrape five times
        // helps nobody. RPC errors (bad path, invalid params) and timeouts are
        // permanent: the former won't change, the latter already cost the full
        // deadline once.
        let value = with_backoff(&BackoffPolicy::single_retry(), "ingest_source", |_| {
            let runner = runner.clone();
            let cmd = cmd.clone();
            async move {
                call_worker(&runner, &cmd).await.map_err(|e| match e {
                    e @ (WorkerError::Spawn { .. }
                    | WorkerError::Io(_)
                    | WorkerError::EarlyExit { .. }) => Retry::Transient(e),
                    e => Retry::Permanent(e),
                })
            }
        })
        .await
        .map_err(|e| ToolError::Exec {
            tool: "ingest_source".into(),
            message: e.to_string(),
        })?;
        Ok(format_outcome(&value))
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
