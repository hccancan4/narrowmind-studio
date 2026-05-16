//! Filesystem tools: `read_file`, `write_file`, `list_dir`.
//!
//! All three are project-scoped through [`super::sandbox::resolve_within`] — there is no
//! way for the model to escape into the user's wider filesystem from these tools.

use std::fmt::Write as _;
use std::fs;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::info;

use super::context::ToolContext;
use super::registry::{Tool, ToolDef, ToolError, ToolResult};
use super::sandbox::resolve_within;

/// Maximum number of bytes `read_file` will return. Keeps wildly-oversized files from
/// blowing up the model's context — the model can ask the user instead of being silently
/// truncated, and large blobs belong in the dataset pipeline, not the agent loop.
const MAX_READ_BYTES: usize = 256 * 1024;

// ---------------------------------------------------------------------------
// read_file
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ReadFileArgs {
    path: String,
}

pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "read_file".into(),
            description: "Read a UTF-8 file inside the current project. Returns the file contents \
                          (truncated to 256 KiB) plus byte / line counts."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path relative to the project root. May not start with / or contain ..." }
                },
                "required": ["path"]
            }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, args: Value) -> Result<ToolResult, ToolError> {
        let args: ReadFileArgs = parse_args("read_file", args)?;
        let project = ctx.project.as_ref().ok_or(ToolError::NoProject)?;
        let abs = resolve_within(&project.root, &args.path)?;
        if !abs.is_file() {
            return Err(ToolError::Exec {
                tool: "read_file".into(),
                message: format!("`{}` is not a file", args.path),
            });
        }
        let bytes = fs::read(&abs)?;
        let total_bytes = bytes.len();
        let truncated = total_bytes > MAX_READ_BYTES;
        let slice = if truncated { &bytes[..MAX_READ_BYTES] } else { &bytes[..] };
        let text = String::from_utf8_lossy(slice).into_owned();
        let line_count = text.matches('\n').count() + usize::from(!text.is_empty());
        info!(path = %args.path, bytes = total_bytes, truncated, "read_file");
        let summary = if truncated {
            format!("{total_bytes} bytes, {line_count} lines (truncated to 256 KiB)\n---\n{text}")
        } else {
            format!("{total_bytes} bytes, {line_count} lines\n---\n{text}")
        };
        Ok(ToolResult::text(summary).with_structured(json!({
            "path": args.path,
            "total_bytes": total_bytes,
            "returned_bytes": slice.len(),
            "line_count": line_count,
            "truncated": truncated,
        })))
    }
}

// ---------------------------------------------------------------------------
// write_file
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct WriteFileArgs {
    path: String,
    content: String,
    /// If true, create parent directories as needed. Defaults to `true` — the alternative
    /// is a constant stream of `mkdir`-then-`write_file` calls from the model.
    #[serde(default = "default_true")]
    create_parents: bool,
}

fn default_true() -> bool {
    true
}

pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "write_file".into(),
            description: "Write a UTF-8 string to a file inside the current project. Overwrites \
                          if the file exists. Creates parent directories by default."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path relative to the project root." },
                    "content": { "type": "string", "description": "UTF-8 contents to write." },
                    "create_parents": { "type": "boolean", "default": true }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, args: Value) -> Result<ToolResult, ToolError> {
        let args: WriteFileArgs = parse_args("write_file", args)?;
        let project = ctx.project.as_ref().ok_or(ToolError::NoProject)?;
        let abs = resolve_within(&project.root, &args.path)?;
        if args.create_parents {
            if let Some(parent) = abs.parent() {
                fs::create_dir_all(parent)?;
            }
        }
        let bytes = args.content.as_bytes();
        fs::write(&abs, bytes)?;
        info!(path = %args.path, bytes = bytes.len(), "write_file");
        Ok(ToolResult::text(format!(
            "wrote {} bytes to `{}`",
            bytes.len(),
            args.path
        ))
        .with_structured(json!({
            "path": args.path,
            "bytes_written": bytes.len(),
        })))
    }
}

// ---------------------------------------------------------------------------
// list_dir
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ListDirArgs {
    /// Defaults to project root.
    #[serde(default)]
    path: Option<String>,
}

pub struct ListDir;

#[async_trait]
impl Tool for ListDir {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "list_dir".into(),
            description: "List entries (files and subdirectories) inside a directory in the \
                          current project. Returns name, kind, and size for each entry."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory path relative to the project root. Defaults to the root." }
                }
            }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, args: Value) -> Result<ToolResult, ToolError> {
        let args: ListDirArgs = parse_args("list_dir", args)?;
        let project = ctx.project.as_ref().ok_or(ToolError::NoProject)?;
        let abs = resolve_within(&project.root, args.path.as_deref().unwrap_or("."))?;
        if !abs.is_dir() {
            return Err(ToolError::Exec {
                tool: "list_dir".into(),
                message: format!("`{}` is not a directory", args.path.unwrap_or_default()),
            });
        }
        let mut entries: Vec<EntryInfo> = Vec::new();
        for entry in fs::read_dir(&abs)? {
            let entry = entry?;
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let ft = entry.file_type()?;
            let kind = if ft.is_dir() {
                "dir"
            } else if ft.is_file() {
                "file"
            } else if ft.is_symlink() {
                "symlink"
            } else {
                "other"
            };
            let size = if ft.is_file() {
                entry.metadata().ok().map_or(0, |m| m.len())
            } else {
                0
            };
            entries.push(EntryInfo { name: file_name, kind: kind.into(), size });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));

        let mut text = String::new();
        for e in &entries {
            let _ = writeln!(text, "{:<8} {:>10}  {}", e.kind, e.size, e.name);
        }
        if text.is_empty() {
            text.push_str("(empty directory)\n");
        }
        info!(
            path = args.path.as_deref().unwrap_or("."),
            entries = entries.len(),
            "list_dir"
        );
        Ok(ToolResult::text(text).with_structured(json!({ "entries": entries })))
    }
}

#[derive(Debug, serde::Serialize)]
struct EntryInfo {
    name: String,
    kind: String,
    size: u64,
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn parse_args<T: serde::de::DeserializeOwned>(tool: &str, args: Value) -> Result<T, ToolError> {
    serde_json::from_value(args).map_err(|e| ToolError::BadInput {
        tool: tool.into(),
        reason: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::TempDir;
    use tokio::sync::mpsc;

    use super::*;
    use crate::project::ProjectStore;
    use crate::tools::context::ProjectScope;

    fn ctx_with(root: std::path::PathBuf) -> (ToolContext, mpsc::UnboundedReceiver<crate::tools::ToolEvent>) {
        let store = Arc::new(ProjectStore::new(root.parent().unwrap()));
        let (tx, rx) = mpsc::unbounded_channel();
        let scope = ProjectScope {
            name: "demo".into(),
            root,
        };
        (ToolContext::new(Some(scope), store, tx), rx)
    }

    #[tokio::test]
    async fn write_then_read_round_trip() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("demo");
        std::fs::create_dir_all(&root).unwrap();
        let (ctx, _rx) = ctx_with(root);

        let wf = WriteFile;
        let res = wf
            .invoke(&ctx, json!({ "path": "hello.txt", "content": "merhaba dünya\n" }))
            .await
            .unwrap();
        assert!(res.content.starts_with("wrote"));

        let rf = ReadFile;
        let res = rf.invoke(&ctx, json!({ "path": "hello.txt" })).await.unwrap();
        assert!(res.content.contains("merhaba dünya"));
    }

    #[tokio::test]
    async fn write_file_refuses_path_escape() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("demo");
        std::fs::create_dir_all(&root).unwrap();
        let (ctx, _rx) = ctx_with(root);

        let wf = WriteFile;
        let err = wf
            .invoke(&ctx, json!({ "path": "../escape.txt", "content": "no" }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PathEscape(_)));
    }

    #[tokio::test]
    async fn list_dir_lists_created_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("demo");
        std::fs::create_dir_all(root.join("datasets")).unwrap();
        std::fs::write(root.join("a.txt"), "x").unwrap();
        let (ctx, _rx) = ctx_with(root);

        let res = ListDir.invoke(&ctx, json!({})).await.unwrap();
        assert!(res.content.contains("a.txt"));
        assert!(res.content.contains("datasets"));
    }

    #[tokio::test]
    async fn tools_refuse_when_no_project_selected() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(ProjectStore::new(tmp.path()));
        let (tx, _rx) = mpsc::unbounded_channel();
        let ctx = ToolContext::new(None, store, tx);
        let err = WriteFile
            .invoke(&ctx, json!({ "path": "x.txt", "content": "y" }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::NoProject));
    }
}
