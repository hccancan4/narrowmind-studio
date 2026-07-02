//! `export_corpus` / `import_corpus` — build a prepared corpus once, reuse it across projects.
//!
//! Phase 4.9: a prepared corpus is the expensive artifact of the whole produce path —
//! embedding 14K+ chunks takes CPU-hours and grounded `generate_sft` costs real synth
//! dollars. Before this tool, starting a second project over the same domain meant paying
//! both again. Now a project's prepared datasets (`rag.jsonl`, `sft.jsonl`, `eval.jsonl`)
//! and its LanceDB `vector_store/` can be exported to a shared library and imported into
//! any number of new projects.
//!
//! Layout: `<store_root>/_corpora/<name>/{corpus.toml, datasets/, vector_store/}`.
//! The `_corpora` directory lives *inside* the project-store root on purpose: no second
//! env var, WSL sees it through the same mount the training worker already uses, and
//! `ProjectStore::list` ignores it automatically (no `project.toml` inside).
//!
//! Deliberately NOT copied: `sources/` (raw documents — the corpus is the *prepared*
//! output, provenance stays with the origin project), `runs/`, `models/`, `evals/`.
//! Deliberately NO overwrite flags in v1 — export refuses an existing corpus name and
//! import refuses a project that already has prepared data (protecting hours of work
//! beats saving one manual delete).

use std::fs;
use std::path::Path;

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::context::ToolContext;
use super::registry::{Tool, ToolDef, ToolError, ToolResult};
use crate::project::validate_name;

/// Reserved directory (inside the project-store root) that holds every exported corpus.
/// Underscore prefix keeps it visually apart from project dirs; the store's listing skips
/// it because it never contains a `project.toml`.
pub const CORPORA_DIR: &str = "_corpora";

/// Files under `datasets/` that participate in export/import. `rag.jsonl` is the one
/// hard requirement (a corpus with no retrieval corpus is not a corpus); the SFT pair
/// files ride along when present.
const DATASET_FILES: &[&str] = &["rag.jsonl", "sft.jsonl", "eval.jsonl"];

/// Manifest written at export time. TOML for the same reason `project.toml` is TOML:
/// humans read these files when debugging, and the filesystem is the source of truth.
#[derive(Debug, Serialize, Deserialize)]
struct CorpusManifest {
    schema_version: u32,
    name: String,
    /// Project the corpus was exported from (provenance pointer, not a live link).
    source_project: String,
    created_at: String,
    /// Line counts at export time, so `import_corpus` can sanity-report without
    /// re-scanning multi-hundred-MB jsonl files.
    chunk_count: u64,
    sft_count: u64,
    eval_count: u64,
    /// Free-text licensing / provenance notes. The Phase 4.9 corpus policy is
    /// Apache-compatible/PD sources only — record per-source licenses here.
    notes: String,
}

const CORPUS_SCHEMA_VERSION: u32 = 1;

/// `Exec` error constructor — every failure in this module is tool-scoped.
fn fail(tool: &str, message: impl Into<String>) -> ToolError {
    ToolError::Exec {
        tool: tool.into(),
        message: message.into(),
    }
}

fn bad(tool: &str, reason: impl Into<String>) -> ToolError {
    ToolError::BadInput {
        tool: tool.into(),
        reason: reason.into(),
    }
}

// ---------------------------------------------------------------------------
// export_corpus
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ExportCorpusArgs {
    /// Corpus name (kebab-case, same rules as project names).
    name: String,
    /// Licensing / provenance notes stored in the manifest.
    #[serde(default)]
    notes: String,
}

pub struct ExportCorpus;

#[async_trait]
impl Tool for ExportCorpus {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "export_corpus".into(),
            description: "Export the current project's prepared corpus (datasets/rag.jsonl + \
                          sft.jsonl/eval.jsonl when present + the vector_store/) into the \
                          shared corpora library so other projects can import it without \
                          re-ingesting, re-embedding, or re-synthesizing. Refuses to \
                          overwrite an existing corpus name. Record source licenses in \
                          `notes`."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name":  { "type": "string", "description": "Corpus name (kebab-case)." },
                    "notes": { "type": "string", "description": "License / provenance notes." }
                },
                "required": ["name"]
            }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, args: Value) -> Result<ToolResult, ToolError> {
        const TOOL: &str = "export_corpus";
        let args: ExportCorpusArgs =
            serde_json::from_value(args).map_err(|e| bad(TOOL, e.to_string()))?;
        validate_name(&args.name)
            .map_err(|reason| bad(TOOL, format!("corpus name: {reason}")))?;
        let Some(scope) = ctx.current_project().await else {
            return Err(ToolError::NoProject);
        };

        let rag = scope.root.join("datasets").join("rag.jsonl");
        if !rag.is_file() {
            return Err(fail(
                TOOL,
                format!(
                    "project `{}` has no datasets/rag.jsonl — run build_dataset first; there \
                     is no prepared corpus to export",
                    scope.name
                ),
            ));
        }

        let corpus_dir = ctx
            .project_store
            .root()
            .join(CORPORA_DIR)
            .join(&args.name);
        if corpus_dir.exists() {
            return Err(fail(
                TOOL,
                format!(
                    "corpus `{}` already exists at {} — pick a new name or delete it manually \
                     (no overwrite in v1)",
                    args.name,
                    corpus_dir.display()
                ),
            ));
        }

        // Copy datasets (rag required, sft/eval opportunistic) + vector store.
        let dst_datasets = corpus_dir.join("datasets");
        fs::create_dir_all(&dst_datasets)?;
        let mut copied: Vec<String> = Vec::new();
        for file in DATASET_FILES {
            let src = scope.root.join("datasets").join(file);
            if src.is_file() {
                fs::copy(&src, dst_datasets.join(file))?;
                copied.push((*file).to_string());
            }
        }
        let src_store = scope.root.join("vector_store");
        let store_copied = if dir_has_entries(&src_store) {
            copy_dir_recursive(&src_store, &corpus_dir.join("vector_store"))
                .map_err(|e| fail(TOOL, e))?;
            true
        } else {
            false
        };

        let manifest = CorpusManifest {
            schema_version: CORPUS_SCHEMA_VERSION,
            name: args.name.clone(),
            source_project: scope.name.clone(),
            created_at: Utc::now().to_rfc3339(),
            chunk_count: count_lines(&rag),
            sft_count: count_lines(&scope.root.join("datasets").join("sft.jsonl")),
            eval_count: count_lines(&scope.root.join("datasets").join("eval.jsonl")),
            notes: args.notes,
        };
        let manifest_path = corpus_dir.join("corpus.toml");
        let toml_text = toml::to_string_pretty(&manifest)
            .map_err(|e| fail(TOOL, format!("manifest serialise: {e}")))?;
        fs::write(&manifest_path, toml_text)?;

        let text = format!(
            "exported corpus `{}` from project `{}`\n  chunks {} / sft {} / eval {}\n  \
             datasets: {}\n  vector_store: {}\n  at {}",
            manifest.name,
            manifest.source_project,
            manifest.chunk_count,
            manifest.sft_count,
            manifest.eval_count,
            copied.join(", "),
            if store_copied { "copied" } else { "absent (re-embed after import)" },
            corpus_dir.display()
        );
        Ok(ToolResult::text(text).with_structured(json!({
            "name": manifest.name,
            "source_project": manifest.source_project,
            "chunk_count": manifest.chunk_count,
            "sft_count": manifest.sft_count,
            "eval_count": manifest.eval_count,
            "vector_store": store_copied,
            "path": corpus_dir.to_string_lossy(),
        })))
    }
}

// ---------------------------------------------------------------------------
// import_corpus
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ImportCorpusArgs {
    /// Corpus to import (must exist under `_corpora/`).
    name: String,
    /// Also copy sft.jsonl / eval.jsonl (default true). Set false to reuse only the
    /// retrieval side and generate fresh SFT data in the target project.
    #[serde(default = "default_true")]
    include_sft: bool,
}

fn default_true() -> bool {
    true
}

pub struct ImportCorpus;

#[async_trait]
impl Tool for ImportCorpus {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "import_corpus".into(),
            description: "Import a prepared corpus from the shared corpora library into the \
                          current project: datasets (rag.jsonl + optionally sft/eval) and the \
                          vector_store, skipping ingest + embedding + synthesis entirely. \
                          Refuses if the project already has prepared data (rag.jsonl or a \
                          non-empty vector_store). Set include_sft=false to take only the \
                          retrieval side."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name":        { "type": "string" },
                    "include_sft": { "type": "boolean", "default": true }
                },
                "required": ["name"]
            }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, args: Value) -> Result<ToolResult, ToolError> {
        const TOOL: &str = "import_corpus";
        let args: ImportCorpusArgs =
            serde_json::from_value(args).map_err(|e| bad(TOOL, e.to_string()))?;
        validate_name(&args.name)
            .map_err(|reason| bad(TOOL, format!("corpus name: {reason}")))?;
        let Some(scope) = ctx.current_project().await else {
            return Err(ToolError::NoProject);
        };

        let corpus_dir = ctx
            .project_store
            .root()
            .join(CORPORA_DIR)
            .join(&args.name);
        let manifest_path = corpus_dir.join("corpus.toml");
        if !manifest_path.is_file() {
            // List what IS available so the caller can self-correct without another probe.
            let available = list_corpora(ctx.project_store.root());
            return Err(fail(
                TOOL,
                format!(
                    "corpus `{}` not found. Available: {}",
                    args.name,
                    if available.is_empty() { "<none>".into() } else { available.join(", ") }
                ),
            ));
        }
        let manifest: CorpusManifest =
            toml::from_str(&fs::read_to_string(&manifest_path)?)
                .map_err(|e| fail(TOOL, format!("corpus.toml parse: {e}")))?;
        if manifest.schema_version != CORPUS_SCHEMA_VERSION {
            return Err(fail(
                TOOL,
                format!(
                    "corpus `{}` has schema v{}, this build expects v{CORPUS_SCHEMA_VERSION}",
                    args.name, manifest.schema_version
                ),
            ));
        }

        // Refuse to clobber prepared work in the target project.
        let dst_rag = scope.root.join("datasets").join("rag.jsonl");
        let dst_store = scope.root.join("vector_store");
        if dst_rag.is_file() || dir_has_entries(&dst_store) {
            return Err(fail(
                TOOL,
                format!(
                    "project `{}` already has prepared data (datasets/rag.jsonl or a non-empty \
                     vector_store) — import into a fresh project (no overwrite in v1)",
                    scope.name
                ),
            ));
        }

        let dst_datasets = scope.root.join("datasets");
        fs::create_dir_all(&dst_datasets)?;
        let mut copied: Vec<String> = Vec::new();
        for file in DATASET_FILES {
            if !args.include_sft && *file != "rag.jsonl" {
                continue;
            }
            let src = corpus_dir.join("datasets").join(file);
            if src.is_file() {
                fs::copy(&src, dst_datasets.join(file))?;
                copied.push((*file).to_string());
            }
        }
        let src_store = corpus_dir.join("vector_store");
        let store_copied = if dir_has_entries(&src_store) {
            copy_dir_recursive(&src_store, &dst_store).map_err(|e| fail(TOOL, e))?;
            true
        } else {
            false
        };

        let text = format!(
            "imported corpus `{}` (from project `{}`, {}) into `{}`\n  chunks {} / sft {} / \
             eval {}\n  datasets: {}\n  vector_store: {}{}",
            manifest.name,
            manifest.source_project,
            manifest.created_at,
            scope.name,
            manifest.chunk_count,
            manifest.sft_count,
            manifest.eval_count,
            copied.join(", "),
            if store_copied {
                "copied — retrieval is ready, no re-embedding needed"
            } else {
                "absent — run build_dataset to embed"
            },
            if manifest.notes.is_empty() {
                String::new()
            } else {
                format!("\n  notes: {}", manifest.notes)
            },
        );
        Ok(ToolResult::text(text).with_structured(json!({
            "name": manifest.name,
            "source_project": manifest.source_project,
            "chunk_count": manifest.chunk_count,
            "copied": copied,
            "vector_store": store_copied,
        })))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Names of every corpus under `<root>/_corpora/` that has a manifest, sorted.
fn list_corpora(store_root: &Path) -> Vec<String> {
    let dir = store_root.join(CORPORA_DIR);
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .filter_map(std::result::Result::ok)
        .filter(|e| e.path().join("corpus.toml").is_file())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    names.sort();
    names
}

/// `true` when `dir` exists and contains at least one entry — the "has prepared data"
/// test for vector stores (an empty dir is created by `ProjectStore::create`, so bare
/// existence is not a signal).
fn dir_has_entries(dir: &Path) -> bool {
    fs::read_dir(dir).map(|mut e| e.next().is_some()).unwrap_or(false)
}

/// Minimal recursive copy. std-only on purpose: the copied trees are LanceDB data dirs
/// (regular files + subdirs, no symlinks on our platforms), and pulling a crate for this
/// would be premature complexity.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    let entries = fs::read_dir(src).map_err(|e| format!("{}: {e}", src.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", src.display()))?;
        let ty = entry
            .file_type()
            .map_err(|e| format!("{}: {e}", entry.path().display()))?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), &to)
                .map_err(|e| format!("{}: {e}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// Line count for manifests; 0 for missing files (sft/eval are optional).
fn count_lines(path: &Path) -> u64 {
    use std::io::{BufRead, BufReader};
    let Ok(f) = fs::File::open(path) else { return 0 };
    BufReader::new(f).lines().map_while(std::result::Result::ok).count() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ProjectStore;
    use crate::tools::context::{new_selected_project, ProjectScope};
    use std::sync::Arc;
    use tokio::sync::mpsc::unbounded_channel;

    /// Store + two projects + a ToolContext selected on the first.
    fn setup() -> (tempfile::TempDir, Arc<ProjectStore>, ToolContext) {
        use crate::project::{ProjectTier, ProviderConfig};
        let provider = || ProviderConfig {
            name: "anthropic".into(),
            model: "claude-test".into(),
            synth_model: String::new(),
        };
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Arc::new(ProjectStore::new(tmp.path().to_path_buf()));
        store.create("proj-a", ProjectTier::Rag, provider()).unwrap();
        store.create("proj-b", ProjectTier::Rag, provider()).unwrap();
        let scope = ProjectScope {
            name: "proj-a".into(),
            root: store.root().join("proj-a"),
        };
        let (tx, _rx) = unbounded_channel();
        let ctx = ToolContext::new(new_selected_project(Some(scope)), store.clone(), tx);
        (tmp, store, ctx)
    }

    fn prepare_project(root: &Path, chunks: usize) {
        let ds = root.join("datasets");
        fs::create_dir_all(&ds).unwrap();
        let rag: String = (0..chunks).map(|i| format!("{{\"chunk_id\":\"ck_{i}\"}}\n")).collect();
        fs::write(ds.join("rag.jsonl"), rag).unwrap();
        fs::write(ds.join("sft.jsonl"), "{\"q\":1}\n{\"q\":2}\n").unwrap();
        fs::write(ds.join("eval.jsonl"), "{\"q\":3}\n").unwrap();
        let vs = root.join("vector_store").join("table.lance");
        fs::create_dir_all(&vs).unwrap();
        fs::write(vs.join("data.bin"), b"lance-bytes").unwrap();
    }

    async fn select(ctx: &ToolContext, store: &ProjectStore, name: &str) {
        ctx.set_project(Some(ProjectScope {
            name: name.into(),
            root: store.root().join(name),
        }))
        .await;
    }

    #[tokio::test]
    async fn export_then_import_roundtrips_datasets_and_store() {
        let (_tmp, store, ctx) = setup();
        prepare_project(&store.root().join("proj-a"), 5);

        let res = ExportCorpus
            .invoke(&ctx, json!({ "name": "felsefe", "notes": "PD sources" }))
            .await
            .unwrap();
        assert!(res.content.contains("chunks 5 / sft 2 / eval 1"));
        assert!(store.root().join(CORPORA_DIR).join("felsefe").join("corpus.toml").is_file());

        select(&ctx, &store, "proj-b").await;
        let res = ImportCorpus.invoke(&ctx, json!({ "name": "felsefe" })).await.unwrap();
        assert!(res.content.contains("retrieval is ready"));
        let b = store.root().join("proj-b");
        assert!(b.join("datasets").join("rag.jsonl").is_file());
        assert!(b.join("datasets").join("sft.jsonl").is_file());
        assert_eq!(
            fs::read(b.join("vector_store").join("table.lance").join("data.bin")).unwrap(),
            b"lance-bytes"
        );
    }

    #[tokio::test]
    async fn import_without_sft_copies_only_rag() {
        let (_tmp, store, ctx) = setup();
        prepare_project(&store.root().join("proj-a"), 3);
        ExportCorpus.invoke(&ctx, json!({ "name": "ragonly" })).await.unwrap();

        select(&ctx, &store, "proj-b").await;
        ImportCorpus
            .invoke(&ctx, json!({ "name": "ragonly", "include_sft": false }))
            .await
            .unwrap();
        let ds = store.root().join("proj-b").join("datasets");
        assert!(ds.join("rag.jsonl").is_file());
        assert!(!ds.join("sft.jsonl").exists());
        assert!(!ds.join("eval.jsonl").exists());
    }

    #[tokio::test]
    async fn export_refuses_unprepared_project_and_existing_name() {
        let (_tmp, store, ctx) = setup();
        // No rag.jsonl yet → refuse.
        let err = ExportCorpus.invoke(&ctx, json!({ "name": "empty" })).await.unwrap_err();
        assert!(err.to_string().contains("no datasets/rag.jsonl"));

        prepare_project(&store.root().join("proj-a"), 1);
        ExportCorpus.invoke(&ctx, json!({ "name": "dup" })).await.unwrap();
        let err = ExportCorpus.invoke(&ctx, json!({ "name": "dup" })).await.unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[tokio::test]
    async fn import_refuses_missing_corpus_and_prepared_target() {
        let (_tmp, store, ctx) = setup();
        prepare_project(&store.root().join("proj-a"), 1);

        // Missing corpus → error names what IS available (nothing yet).
        let err = ImportCorpus.invoke(&ctx, json!({ "name": "ghost" })).await.unwrap_err();
        assert!(err.to_string().contains("not found"));

        ExportCorpus.invoke(&ctx, json!({ "name": "real" })).await.unwrap();
        // proj-a itself is "prepared" → import must refuse to clobber it.
        let err = ImportCorpus.invoke(&ctx, json!({ "name": "real" })).await.unwrap_err();
        assert!(err.to_string().contains("already has prepared data"));
    }

    #[tokio::test]
    async fn corpora_dir_is_invisible_to_project_listing() {
        let (_tmp, store, ctx) = setup();
        prepare_project(&store.root().join("proj-a"), 1);
        ExportCorpus.invoke(&ctx, json!({ "name": "hidden" })).await.unwrap();
        let names = store.list().unwrap();
        assert_eq!(names, vec!["proj-a".to_string(), "proj-b".to_string()]);
    }

    #[test]
    fn invalid_corpus_name_is_rejected() {
        // validate_name is shared with project names — spot-check the wiring, not the rules.
        assert!(validate_name("has spaces").is_err());
        assert!(validate_name("ok-name-1").is_ok());
    }
}
