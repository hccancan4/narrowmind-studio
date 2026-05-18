//! Project lifecycle tools: `project_status`, `create_project`, `list_projects`.
//!
//! These wrap [`ProjectStore`] in a stable, model-facing surface. They never touch the
//! filesystem directly — the store is the single point of truth.

use std::fmt::Write as _;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::info;

use super::context::{ProjectScope, ToolContext};
use super::registry::{Tool, ToolDef, ToolError, ToolResult};
use crate::project::{ProjectTier, ProviderConfig};

// ---------------------------------------------------------------------------
// project_status
// ---------------------------------------------------------------------------

pub struct ProjectStatus;

#[async_trait]
impl Tool for ProjectStatus {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "project_status".into(),
            description: "Report the currently-selected project: name, tier, status, base model, \
                          provider, and on-disk path. Returns an empty object when no project \
                          is selected."
                .into(),
            input_schema: json!({ "type": "object" }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, _args: Value) -> Result<ToolResult, ToolError> {
        match ctx.current_project().await {
            None => Ok(ToolResult::text("no project selected")
                .with_structured(json!({ "selected": false }))),
            Some(scope) => {
                let cfg = ctx.project_store.get(&scope.name)?;
                let text = format!(
                    "project `{}`\n  status     {:?}\n  tier       {:?}\n  base_model {}\n  provider   {} ({})\n  root       {}\n  created    {}",
                    cfg.name,
                    cfg.status,
                    cfg.tier,
                    if cfg.base_model.is_empty() { "<unset>".into() } else { cfg.base_model.clone() },
                    cfg.provider.name,
                    cfg.provider.model,
                    scope.root.display(),
                    cfg.created_at,
                );
                Ok(ToolResult::text(text).with_structured(serde_json::to_value(&cfg).unwrap_or(Value::Null)))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// create_project
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateProjectArgs {
    name: String,
    /// `rag`, `lora`, or `hybrid`. Defaults to `rag` (Tier 0).
    #[serde(default)]
    tier: Option<String>,
    /// LLM provider id for the agent loop. Defaults to the currently-selected provider
    /// from settings (passed in via the orchestrator's default provider in `ToolContext`).
    #[serde(default)]
    provider: Option<String>,
    /// Default model id for the agent loop in this project.
    #[serde(default)]
    model: Option<String>,
}

pub struct CreateProject;

#[async_trait]
impl Tool for CreateProject {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "create_project".into(),
            description: "Create a new project directory under the user's projects root with a \
                          fresh project.toml (schema_version=1). Project name must match \
                          [a-z0-9][a-z0-9-]{1,63}."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "tier": { "type": "string", "enum": ["rag", "lora", "hybrid"], "default": "rag" },
                    "provider": { "type": "string", "description": "Provider id, e.g. 'anthropic'. Defaults to the app-wide provider." },
                    "model": { "type": "string", "description": "Default agent model. Defaults to the app-wide model." }
                },
                "required": ["name"]
            }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, args: Value) -> Result<ToolResult, ToolError> {
        let args: CreateProjectArgs = parse_args("create_project", args)?;
        let tier = parse_tier(args.tier.as_deref())?;
        let provider = ProviderConfig {
            name: args.provider.unwrap_or_else(|| "anthropic".into()),
            model: args.model.unwrap_or_else(|| "claude-sonnet-4-6".into()),
            // Per-project override is empty by default; users opt in via Settings dialog.
            synth_model: String::new(),
        };
        let cfg = ctx.project_store.create(&args.name, tier, provider)?;
        let root = ctx.project_store.project_dir(&cfg.name);
        info!(project = %cfg.name, tier = ?cfg.tier, root = %root.display(), "create_project");

        // Auto-select the newly created project so any subsequent file / run tool calls in
        // the same agent turn land inside it. This is what makes "create project X and
        // write a hello file in it" work as a single user prompt.
        ctx.set_project(Some(ProjectScope {
            name: cfg.name.clone(),
            root: root.clone(),
        }))
        .await;

        Ok(ToolResult::text(format!(
            "created project `{}` at {} (now the active project)",
            cfg.name,
            root.display()
        ))
        .with_structured(json!({
            "name": cfg.name,
            "tier": cfg.tier,
            "root": root,
            "selected": true,
        })))
    }
}

fn parse_tier(s: Option<&str>) -> Result<ProjectTier, ToolError> {
    Ok(match s {
        None | Some("" | "rag") => ProjectTier::Rag,
        Some("lora") => ProjectTier::Lora,
        Some("hybrid") => ProjectTier::Hybrid,
        Some(other) => {
            return Err(ToolError::BadInput {
                tool: "create_project".into(),
                reason: format!("unknown tier `{other}` (expected rag | lora | hybrid)"),
            });
        }
    })
}

// ---------------------------------------------------------------------------
// list_projects
// ---------------------------------------------------------------------------

pub struct ListProjects;

#[async_trait]
impl Tool for ListProjects {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "list_projects".into(),
            description: "List all projects in the user's projects root, sorted alphabetically. \
                          Returns a name + status + tier summary for each one."
                .into(),
            input_schema: json!({ "type": "object" }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, _args: Value) -> Result<ToolResult, ToolError> {
        let names = ctx.project_store.list()?;
        let mut rows = Vec::new();
        for name in &names {
            match ctx.project_store.get(name) {
                Ok(cfg) => rows.push(json!({
                    "name": cfg.name,
                    "status": cfg.status,
                    "tier": cfg.tier,
                    "base_model": cfg.base_model,
                })),
                Err(e) => rows.push(json!({
                    "name": name,
                    "error": e.to_string(),
                })),
            }
        }
        let mut text = String::new();
        if rows.is_empty() {
            text.push_str("(no projects yet)\n");
        } else {
            text.push_str("name                        status     tier\n");
            for row in &rows {
                let _ = writeln!(
                    text,
                    "{:<28}{:<11}{}",
                    row.get("name").and_then(Value::as_str).unwrap_or(""),
                    row.get("status").and_then(Value::as_str).unwrap_or("?"),
                    row.get("tier").and_then(Value::as_str).unwrap_or("?"),
                );
            }
        }
        info!(count = names.len(), "list_projects");
        Ok(ToolResult::text(text).with_structured(json!({ "projects": rows })))
    }
}

// ---------------------------------------------------------------------------
// Shared
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
    use crate::tools::context::new_selected_project;

    fn root_ctx() -> (TempDir, ToolContext) {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(ProjectStore::new(tmp.path()));
        let (tx, _rx) = mpsc::unbounded_channel();
        let selected = new_selected_project(None);
        let ctx = ToolContext::new(selected, store, tx);
        (tmp, ctx)
    }

    #[tokio::test]
    async fn create_then_list_then_status() {
        let (_tmp, ctx) = root_ctx();
        let create = CreateProject
            .invoke(&ctx, json!({ "name": "alpha", "tier": "hybrid" }))
            .await
            .unwrap();
        assert!(create.content.contains("alpha"));

        let list = ListProjects.invoke(&ctx, json!({})).await.unwrap();
        assert!(list.content.contains("alpha"));

        // create_project auto-selects, so status now reflects the new project.
        let status = ProjectStatus.invoke(&ctx, json!({})).await.unwrap();
        let structured = status.structured.unwrap();
        assert_eq!(structured["name"], "alpha");
        assert_eq!(structured["tier"], "hybrid");
    }

    #[tokio::test]
    async fn create_rejects_bad_tier() {
        let (_tmp, ctx) = root_ctx();
        let err = CreateProject
            .invoke(&ctx, json!({ "name": "demo", "tier": "tier4" }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::BadInput { .. }));
    }

    #[tokio::test]
    async fn create_auto_selects_new_project() {
        let (_tmp, ctx) = root_ctx();
        assert!(ctx.current_project().await.is_none());
        CreateProject
            .invoke(&ctx, json!({ "name": "fresh", "tier": "rag" }))
            .await
            .unwrap();
        let selected = ctx.current_project().await.expect("auto-selected");
        assert_eq!(selected.name, "fresh");
        assert!(selected.root.ends_with("fresh"));
    }
}
