//! `list_chunks` + `filter_chunks` — pure-Rust tools that read and update the
//! `chunks.jsonl` files written by the Python ingestion pipeline.
//!
//! Reading is direct (line-buffered JSONL); filtering is rewrite-in-place via the same
//! `.tmp` swap convention the Python side uses, so a crash mid-write leaves the previous
//! `chunks.jsonl` intact.

use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write as _IoWrite};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{info, warn};

use super::context::ToolContext;
use super::registry::{Tool, ToolDef, ToolError, ToolResult};

/// Per-call cap on how many chunks come back as full text. The structured payload
/// always contains every matched chunk's metadata; the `content` summary only renders
/// this many bodies so we don't blow up the model's context.
const MAX_PREVIEW: usize = 30;

#[derive(Debug, Deserialize)]
struct ListChunksArgs {
    /// When set, restrict to one source. When absent, list across every source in the
    /// current project that has a `chunks.jsonl`.
    #[serde(default)]
    source_id: Option<String>,
    /// Substring search (case-insensitive) over the chunk text. Optional.
    #[serde(default)]
    search: Option<String>,
    /// `"included"` | `"excluded"` | `"all"`. Default `"all"`.
    #[serde(default = "default_filter")]
    show: String,
    /// Pagination.
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_filter() -> String {
    "all".into()
}
fn default_limit() -> usize {
    50
}

#[derive(Debug, Deserialize)]
struct FilterChunksArgs {
    /// Source id whose `chunks.jsonl` will be rewritten.
    source_id: String,
    /// Chunk ids to update. Unknown ids are ignored (and counted in the response).
    chunk_ids: Vec<String>,
    /// Flip the `include` flag to this value for every matched chunk.
    include: bool,
}

/// On-disk schema for one line in `chunks.jsonl`. Mirrors `narrowmind_workers.chunking.Chunk`.
/// Shared with `synth_gen`, which samples the same files (crate-visible for that reason).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChunkRecord {
    pub(crate) chunk_id: String,
    pub(crate) doc_id: String,
    pub(crate) source_id: String,
    pub(crate) text: String,
    pub(crate) token_count: u32,
    pub(crate) sentence_range: (u32, u32),
    #[serde(default = "default_true")]
    pub(crate) include: bool,
    #[serde(default)]
    pub(crate) metadata: Value,
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// list_chunks
// ---------------------------------------------------------------------------

pub struct ListChunks;

#[async_trait]
impl Tool for ListChunks {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "list_chunks".into(),
            description: "List chunks in the current project. Filterable by source, by an \
                          include/excluded view, and by a case-insensitive search over chunk \
                          text. Returns count summary plus the first 30 chunk bodies; the full \
                          metadata list is on the structured payload."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "source_id": { "type": "string", "description": "Restrict to one source." },
                    "search":    { "type": "string", "description": "Case-insensitive substring filter over chunk text." },
                    "show":      { "type": "string", "enum": ["all", "included", "excluded"], "default": "all" },
                    "offset":    { "type": "integer", "minimum": 0, "default": 0 },
                    "limit":     { "type": "integer", "minimum": 1, "maximum": 1000, "default": 50 }
                }
            }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, args: Value) -> Result<ToolResult, ToolError> {
        let args: ListChunksArgs =
            serde_json::from_value(args).map_err(|e| ToolError::BadInput {
                tool: "list_chunks".into(),
                reason: e.to_string(),
            })?;
        let project = ctx.current_project().await.ok_or(ToolError::NoProject)?;

        let source_ids = match &args.source_id {
            Some(id) => vec![id.clone()],
            None => list_sources_with_chunks(&project.root)?,
        };

        let needle = args.search.as_ref().map(|s| s.to_lowercase());
        let mut total = 0_usize;
        let mut matched: Vec<ChunkRecord> = Vec::new();

        for src in &source_ids {
            let path = project.root.join("sources").join(src).join("chunks.jsonl");
            if !path.is_file() {
                continue;
            }
            let file = fs::File::open(&path)?;
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let line = line?;
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                total += 1;
                let rec: ChunkRecord = match serde_json::from_str(line) {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(source = src, error = %e, "skipping malformed chunk line");
                        continue;
                    }
                };
                if !show_matches(&args.show, rec.include) {
                    continue;
                }
                if let Some(needle) = &needle {
                    if !rec.text.to_lowercase().contains(needle) {
                        continue;
                    }
                }
                matched.push(rec);
            }
        }

        let total_matched = matched.len();
        let page: Vec<ChunkRecord> = matched
            .into_iter()
            .skip(args.offset)
            .take(args.limit)
            .collect();

        let mut text = String::new();
        let _ = writeln!(
            text,
            "{} chunks scanned / {} matched ({} returned, offset {}, limit {})",
            total,
            total_matched,
            page.len(),
            args.offset,
            args.limit
        );
        for ch in page.iter().take(MAX_PREVIEW) {
            let snippet: String = ch.text.chars().take(160).collect();
            let _ = writeln!(
                text,
                "- [{}] src={} doc={} tokens={} include={} :: {}…",
                ch.chunk_id, ch.source_id, ch.doc_id, ch.token_count, ch.include, snippet
            );
        }
        if page.len() > MAX_PREVIEW {
            let _ = writeln!(text, "… {} more rows in structured payload …", page.len() - MAX_PREVIEW);
        }

        info!(scanned = total, matched = total_matched, returned = page.len(), "list_chunks");
        Ok(ToolResult::text(text).with_structured(json!({
            "scanned": total,
            "matched": total_matched,
            "returned": page.len(),
            "offset": args.offset,
            "limit": args.limit,
            "chunks": page,
        })))
    }
}

fn show_matches(filter: &str, include: bool) -> bool {
    match filter {
        "included" => include,
        "excluded" => !include,
        _ => true,
    }
}

fn list_sources_with_chunks(project_root: &std::path::Path) -> Result<Vec<String>, ToolError> {
    let sources_dir = project_root.join("sources");
    if !sources_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out: Vec<String> = Vec::new();
    for entry in fs::read_dir(&sources_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let chunks_path = entry.path().join("chunks.jsonl");
        if chunks_path.is_file() {
            if let Some(name) = entry.file_name().to_str() {
                out.push(name.to_string());
            }
        }
    }
    out.sort();
    Ok(out)
}

// ---------------------------------------------------------------------------
// filter_chunks
// ---------------------------------------------------------------------------

pub struct FilterChunks;

#[async_trait]
impl Tool for FilterChunks {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "filter_chunks".into(),
            description: "Toggle the include flag on a list of chunks within one source. \
                          Rewrites chunks.jsonl atomically (.tmp swap). Returns the number of \
                          chunks updated and any chunk_ids that weren't found."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "source_id": { "type": "string" },
                    "chunk_ids": { "type": "array", "items": { "type": "string" } },
                    "include":   { "type": "boolean", "description": "Set include flag to this value." }
                },
                "required": ["source_id", "chunk_ids", "include"]
            }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, args: Value) -> Result<ToolResult, ToolError> {
        let args: FilterChunksArgs =
            serde_json::from_value(args).map_err(|e| ToolError::BadInput {
                tool: "filter_chunks".into(),
                reason: e.to_string(),
            })?;
        let project = ctx.current_project().await.ok_or(ToolError::NoProject)?;

        let path = project
            .root
            .join("sources")
            .join(&args.source_id)
            .join("chunks.jsonl");
        if !path.is_file() {
            return Err(ToolError::Exec {
                tool: "filter_chunks".into(),
                message: format!("chunks.jsonl missing for source {}", args.source_id),
            });
        }

        let wanted: std::collections::HashSet<&str> =
            args.chunk_ids.iter().map(String::as_str).collect();

        let in_file = fs::File::open(&path)?;
        let reader = BufReader::new(in_file);
        let tmp_path = path.with_extension("jsonl.tmp");
        let out_file = fs::File::create(&tmp_path)?;
        let mut writer = BufWriter::new(out_file);

        let mut updated = 0_usize;
        let mut found: std::collections::HashSet<String> = std::collections::HashSet::new();

        for line in reader.lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let mut rec: ChunkRecord = serde_json::from_str(trimmed).map_err(|e| ToolError::Exec {
                tool: "filter_chunks".into(),
                message: format!("malformed chunk line: {e}"),
            })?;
            if wanted.contains(rec.chunk_id.as_str()) {
                rec.include = args.include;
                found.insert(rec.chunk_id.clone());
                updated += 1;
            }
            serde_json::to_writer(&mut writer, &rec).map_err(|e| ToolError::Exec {
                tool: "filter_chunks".into(),
                message: e.to_string(),
            })?;
            writer.write_all(b"\n")?;
        }
        writer.flush()?;
        drop(writer);
        fs::rename(&tmp_path, &path)?;

        let missing: Vec<String> = args
            .chunk_ids
            .iter()
            .filter(|id| !found.contains(*id))
            .cloned()
            .collect();

        info!(
            source = args.source_id,
            requested = args.chunk_ids.len(),
            updated,
            missing = missing.len(),
            "filter_chunks",
        );

        let mut text = String::new();
        let _ = writeln!(
            text,
            "{updated} chunks in `{}` set include={}",
            args.source_id, args.include
        );
        if !missing.is_empty() {
            let _ = writeln!(text, "missing chunk_ids: {missing:?}");
        }
        Ok(ToolResult::text(text).with_structured(json!({
            "source_id": args.source_id,
            "updated": updated,
            "missing": missing,
            "include": args.include,
        })))
    }
}

// ---------------------------------------------------------------------------
// Persisted source path of a chunks.jsonl file — used by tests below.
// ---------------------------------------------------------------------------

#[cfg(test)]
fn write_chunks_fixture(dir: &std::path::Path, records: &[ChunkRecord]) -> std::path::PathBuf {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join("chunks.jsonl");
    let f = fs::File::create(&path).unwrap();
    let mut w = BufWriter::new(f);
    for r in records {
        serde_json::to_writer(&mut w, r).unwrap();
        w.write_all(b"\n").unwrap();
    }
    w.flush().unwrap();
    path
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::TempDir;
    use tokio::sync::mpsc;

    use super::*;
    use crate::project::ProjectStore;
    use crate::tools::context::{new_selected_project, ProjectScope};

    fn ctx_with(
        root: std::path::PathBuf,
    ) -> (ToolContext, mpsc::UnboundedReceiver<crate::tools::ToolEvent>) {
        let store = Arc::new(ProjectStore::new(root.parent().unwrap()));
        let (tx, rx) = mpsc::unbounded_channel();
        let selected = new_selected_project(Some(ProjectScope {
            name: "demo".into(),
            root,
        }));
        (ToolContext::new(selected, store, tx), rx)
    }

    fn fixture(text: &str, id: &str, src: &str) -> ChunkRecord {
        ChunkRecord {
            chunk_id: id.into(),
            doc_id: format!("doc-{src}"),
            source_id: src.into(),
            text: text.into(),
            token_count: u32::try_from(text.split_whitespace().count()).unwrap_or(u32::MAX),
            sentence_range: (0, 0),
            include: true,
            metadata: Value::Null,
        }
    }

    #[tokio::test]
    async fn list_chunks_returns_all_by_default() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("demo");
        write_chunks_fixture(
            &root.join("sources").join("src1"),
            &[
                fixture("first chunk talking about consciousness", "ck1", "src1"),
                fixture("second chunk about functionalism", "ck2", "src1"),
            ],
        );
        let (ctx, _rx) = ctx_with(root);
        let res = ListChunks.invoke(&ctx, json!({})).await.unwrap();
        let s = res.structured.unwrap();
        assert_eq!(s["scanned"], 2);
        assert_eq!(s["matched"], 2);
    }

    #[tokio::test]
    async fn list_chunks_search_filters() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("demo");
        write_chunks_fixture(
            &root.join("sources").join("src1"),
            &[
                fixture("first chunk talking about consciousness", "ck1", "src1"),
                fixture("second chunk about functionalism", "ck2", "src1"),
            ],
        );
        let (ctx, _rx) = ctx_with(root);
        let res = ListChunks
            .invoke(&ctx, json!({ "search": "function" }))
            .await
            .unwrap();
        assert_eq!(res.structured.unwrap()["matched"], 1);
    }

    #[tokio::test]
    async fn filter_chunks_toggles_include_and_rewrites() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("demo");
        write_chunks_fixture(
            &root.join("sources").join("src1"),
            &[
                fixture("a", "ck1", "src1"),
                fixture("b", "ck2", "src1"),
                fixture("c", "ck3", "src1"),
            ],
        );
        let (ctx, _rx) = ctx_with(root.clone());
        let res = FilterChunks
            .invoke(
                &ctx,
                json!({
                    "source_id": "src1",
                    "chunk_ids": ["ck1", "ck3", "doesnotexist"],
                    "include": false
                }),
            )
            .await
            .unwrap();
        let s = res.structured.unwrap();
        assert_eq!(s["updated"], 2);
        assert_eq!(s["missing"], json!(["doesnotexist"]));

        // List again with "excluded" filter — should see ck1 + ck3
        let res = ListChunks
            .invoke(&ctx, json!({ "show": "excluded" }))
            .await
            .unwrap();
        assert_eq!(res.structured.unwrap()["matched"], 2);
    }
}
