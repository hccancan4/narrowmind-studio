//! `build_dataset` — assemble the per-tier training/RAG datasets a project needs.
//!
//! Tier semantics per Phase 3 decision E:
//! - **`rag`**: emit `datasets/rag.jsonl` (filtered chunks copy) and build the `LanceDB`
//!   vector index at `vector_store/chunks.lance`.
//! - **`lora`**: verify `datasets/sft.jsonl` + `datasets/eval.jsonl` exist (we don't
//!   re-run `generate_sft` here — that's an explicit user action).
//! - **`hybrid`**: do both above.
//!
//! When the tool's `tier` arg is omitted, the project's `project.toml` tier is used.

use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write as _IoWrite};
use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{info, warn};

use super::context::ToolContext;
use super::registry::{Tool, ToolDef, ToolError, ToolResult};
use crate::project::ProjectTier;
use crate::retry::timeouts;
use crate::worker::{call_worker, WorkerCommand};

#[derive(Debug, Deserialize)]
struct BuildDatasetArgs {
    /// Override the project.toml tier. One of "rag" | "lora" | "hybrid".
    #[serde(default)]
    tier: Option<String>,
    /// Only chunks with include=true are exported / embedded. Default true.
    #[serde(default = "default_true")]
    include_only: bool,
    /// Optional single-source filter. Omit to use every source in the project.
    #[serde(default)]
    source_id: Option<String>,
}

fn default_true() -> bool {
    true
}

pub struct BuildDataset;

#[async_trait]
impl Tool for BuildDataset {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "build_dataset".into(),
            description: "Assemble the training / RAG datasets for the current project.\n\n\
                          tier='rag' | 'hybrid': writes datasets/rag.jsonl (filtered chunks copy) \
                          and builds the LanceDB vector index at vector_store/chunks.lance via \
                          the rag worker (BGE-small embeddings).\n\
                          tier='lora' | 'hybrid': verifies datasets/sft.jsonl + datasets/eval.jsonl \
                          exist; does NOT re-run generate_sft (that's an explicit user action).\n\n\
                          When tier is omitted the project.toml tier is used. include_only filters \
                          to chunks with include=true (typical). source_id restricts to one source."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "tier":         { "type": "string", "enum": ["rag", "lora", "hybrid"] },
                    "include_only": { "type": "boolean", "default": true },
                    "source_id":    { "type": "string", "description": "Restrict to one source." }
                }
            }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, args: Value) -> Result<ToolResult, ToolError> {
        let args: BuildDatasetArgs =
            serde_json::from_value(args).map_err(|e| ToolError::BadInput {
                tool: "build_dataset".into(),
                reason: e.to_string(),
            })?;
        let project = ctx.current_project().await.ok_or(ToolError::NoProject)?;
        let cfg = ctx.project_store.get(&project.name).map_err(ToolError::Project)?;

        let tier = resolve_tier(args.tier.as_deref(), cfg.tier)?;
        let needs_rag = matches!(tier, ProjectTier::Rag | ProjectTier::Hybrid);
        let needs_sft = matches!(tier, ProjectTier::Lora | ProjectTier::Hybrid);

        let mut text = String::new();
        let mut structured = serde_json::Map::new();
        let _ = writeln!(text, "build_dataset for `{}` (tier={:?})", project.name, tier);

        // --- RAG path: rag.jsonl + LanceDB ---
        if needs_rag {
            let datasets_dir = project.root.join("datasets");
            fs::create_dir_all(&datasets_dir)?;
            let rag_path = datasets_dir.join("rag.jsonl");
            let copied = copy_chunks_to_rag_jsonl(
                &project.root,
                &rag_path,
                args.source_id.as_deref(),
                args.include_only,
            )?;
            let _ = writeln!(text, "  rag.jsonl: {copied} chunks → {}", rag_path.display());
            structured.insert("rag_jsonl_chunks".into(), json!(copied));
            structured.insert("rag_jsonl_path".into(), json!(rag_path));

            // Trigger embed + LanceDB upsert via rag worker.
            let runner = ctx
                .python_runner
                .as_ref()
                .ok_or_else(|| ToolError::Exec {
                    tool: "build_dataset".into(),
                    message: "PythonRunner not configured on ToolContext".into(),
                })?
                .clone();
            let params = json!({
                "project_root": project.root,
                "source_id": args.source_id,
                "include_only": args.include_only,
                // Always build the BM25 sidecar at the same time so hybrid
                // retrieval works without a second tool call. Phase 3 vector
                // stores migrate via the standalone rag.build_fts_index call
                // (see build_fts_index tool) if a re-embed isn't desired.
                "with_fts": true,
            });
            let cmd = WorkerCommand {
                module: "narrowmind_workers.rag".into(),
                method: "rag.embed_chunks".into(),
                params,
                timeout: Some(timeouts::EMBED_BUILD),
            };
            let res = call_worker(&runner, &cmd).await.map_err(|e| ToolError::Exec {
                tool: "build_dataset".into(),
                message: format!("rag worker: {e}"),
            })?;
            let embedded = res.get("embedded").and_then(Value::as_u64).unwrap_or(0);
            let elapsed = res.get("elapsed_seconds").and_then(Value::as_f64).unwrap_or(0.0);
            let fts_built = res.get("fts_built").and_then(Value::as_bool).unwrap_or(false);
            let _ = writeln!(
                text,
                "  vector_store: {embedded} embedded in {elapsed:.1}s (BGE-small{})",
                if fts_built { " + BM25 FTS index" } else { ", FTS index skipped" }
            );
            structured.insert("embedded".into(), json!(embedded));
            structured.insert("embed_elapsed_seconds".into(), json!(elapsed));
            structured.insert("fts_built".into(), json!(fts_built));
        }

        // --- SFT path: verify pre-existing files, never auto-generate ---
        if needs_sft {
            let sft = project.root.join("datasets").join("sft.jsonl");
            let eval = project.root.join("datasets").join("eval.jsonl");
            let sft_lines = file_line_count(&sft).unwrap_or(0);
            let eval_lines = file_line_count(&eval).unwrap_or(0);
            if sft_lines == 0 {
                warn!(path = %sft.display(), "sft.jsonl missing / empty for build_dataset tier");
                let _ = writeln!(
                    text,
                    "  WARN: datasets/sft.jsonl missing or empty — run generate_sft first"
                );
            } else {
                let _ = writeln!(text, "  sft.jsonl: {sft_lines} pairs (already built)");
            }
            if eval_lines == 0 {
                let _ = writeln!(
                    text,
                    "  WARN: datasets/eval.jsonl missing or empty — generate_sft normally writes it"
                );
            } else {
                let _ = writeln!(text, "  eval.jsonl: {eval_lines} pairs (already built)");
            }
            structured.insert("sft_lines".into(), json!(sft_lines));
            structured.insert("eval_lines".into(), json!(eval_lines));
        }

        info!(project = %project.name, tier = ?tier, "build_dataset");
        Ok(ToolResult::text(text).with_structured(Value::Object(structured)))
    }
}

fn resolve_tier(arg: Option<&str>, project_tier: ProjectTier) -> Result<ProjectTier, ToolError> {
    match arg {
        None => Ok(project_tier),
        Some("rag") => Ok(ProjectTier::Rag),
        Some("lora") => Ok(ProjectTier::Lora),
        Some("hybrid") => Ok(ProjectTier::Hybrid),
        Some(other) => Err(ToolError::BadInput {
            tool: "build_dataset".into(),
            reason: format!("unknown tier `{other}` (expected rag | lora | hybrid)"),
        }),
    }
}

/// Copy filtered chunks from all `sources/<id>/chunks.jsonl` files into a single
/// `datasets/rag.jsonl`. We do not include the BGE embeddings — those live in `LanceDB`
/// (Phase 3) and the SDK in Phase 6 will read them from a Lance snapshot.
fn copy_chunks_to_rag_jsonl(
    project_root: &Path,
    output: &Path,
    source_filter: Option<&str>,
    include_only: bool,
) -> Result<usize, ToolError> {
    let sources_dir = project_root.join("sources");
    let tmp = output.with_extension("jsonl.tmp");
    let out_file = fs::File::create(&tmp)?;
    let mut writer = BufWriter::new(out_file);
    let mut written = 0_usize;

    if sources_dir.is_dir() {
        for entry in fs::read_dir(&sources_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if source_filter.is_some_and(|f| f != name) {
                continue;
            }
            let chunks_path = entry.path().join("chunks.jsonl");
            if !chunks_path.is_file() {
                continue;
            }
            let file = fs::File::open(&chunks_path)?;
            for line in BufReader::new(file).lines() {
                let line = line?;
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                // Only filter at the include flag — do not re-parse fields the model added.
                if include_only {
                    match serde_json::from_str::<Value>(trimmed) {
                        Ok(v) => {
                            let included = v.get("include").and_then(Value::as_bool).unwrap_or(true);
                            if !included {
                                continue;
                            }
                        }
                        Err(_) => continue,
                    }
                }
                writer.write_all(trimmed.as_bytes())?;
                writer.write_all(b"\n")?;
                written += 1;
            }
        }
    }
    writer.flush()?;
    drop(writer);
    if written == 0 {
        fs::remove_file(&tmp).ok();
        return Ok(0);
    }
    fs::rename(&tmp, output)?;
    Ok(written)
}

fn file_line_count(path: &Path) -> std::io::Result<usize> {
    if !path.is_file() {
        return Ok(0);
    }
    let file = fs::File::open(path)?;
    let mut n = 0;
    for line in BufReader::new(file).lines() {
        if !line?.trim().is_empty() {
            n += 1;
        }
    }
    Ok(n)
}
