//! `start_inference_server` + `stop_inference_server` agent tools.
//!
//! The actual lifecycle is owned by [`crate::inference::InferenceManager`] sitting on
//! `ToolContext` via the desktop app's `AppState`. These tools are thin facades so the
//! agent can drive the server from a user prompt.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::info;

use super::context::ToolContext;
use super::registry::{Tool, ToolDef, ToolError, ToolResult};
use crate::inference::{KvCacheType, ModelSpec};

#[derive(Debug, Deserialize)]
struct StartArgs {
    /// Override the default model. Optional.
    #[serde(default)]
    repo_id: Option<String>,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    n_ctx: Option<u32>,
    /// llama.cpp `-ngl`. `-1` = all layers to GPU. Default `-1` per Phase 3 decision.
    #[serde(default)]
    n_gpu_layers: Option<i32>,
    /// KV-cache quantization: `f16` | `q8_0` | `q4_0` (Phase 4.6). Default `q8_0`.
    #[serde(default)]
    kv_cache_type: Option<String>,
    /// Force Flash Attention even with an f16 cache. A quantized cache enables it
    /// regardless.
    #[serde(default)]
    flash_attn: Option<bool>,
    /// Reuse the prompt-prefix KV cache across requests (host RAM). Default on.
    #[serde(default)]
    prompt_cache: Option<bool>,
}

pub struct StartInferenceServer;

#[async_trait]
impl Tool for StartInferenceServer {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "start_inference_server".into(),
            description: "Start the llama.cpp inference server for this app (one running \
                          server at a time). Downloads the GGUF from HuggingFace via hf-hub \
                          if not in cache, spawns the OpenAI-compatible server, waits for \
                          health. Default model: bartowski/Qwen2.5-7B-Instruct-GGUF Q4_K_M. \
                          KV cache is quantized to q8_0 by default (frees VRAM for longer \
                          context on 8 GB; pass kv_cache_type='f16' for the exact-baseline \
                          opt-out). To serve a fine-tuned domain model, pass its absolute \
                          .gguf path (from export_domain_gguf / list_domain_models) as \
                          `filename` — it is loaded directly, no download. Returns the \
                          endpoint URL the rag_chat / chat_preview tools will point at."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "repo_id":      { "type": "string", "description": "HF repo id, e.g. 'bartowski/Qwen2.5-7B-Instruct-GGUF'." },
                    "filename":     { "type": "string", "description": "GGUF filename inside the repo, OR an absolute local path to a domain GGUF (from export_domain_gguf) — a local path is served directly, no download." },
                    "n_ctx":        { "type": "integer", "minimum": 256, "maximum": 32768, "default": 4096 },
                    "n_gpu_layers": { "type": "integer", "default": -1, "description": "-1 = all layers on GPU; 0 = CPU-only." },
                    "kv_cache_type": { "type": "string", "enum": ["f16", "q8_0", "q4_0"], "default": "q8_0", "description": "KV-cache quantization. q8_0 (default) is near-lossless and frees ~half the KV VRAM; f16 is the full-precision opt-out; q4_0 is aggressive (more context, quality risk). Quantized caches enable Flash Attention automatically." },
                    "flash_attn":   { "type": "boolean", "description": "Force Flash Attention even with an f16 cache. A quantized kv_cache_type turns it on regardless." },
                    "prompt_cache": { "type": "boolean", "default": true, "description": "Reuse the prompt-prefix KV cache across requests (host RAM, no VRAM cost) so the shared system prompt + repeated RAG context aren't re-evaluated every call." }
                }
            }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, args: Value) -> Result<ToolResult, ToolError> {
        let args: StartArgs = serde_json::from_value(args).map_err(|e| ToolError::BadInput {
            tool: "start_inference_server".into(),
            reason: e.to_string(),
        })?;

        let manager = ctx
            .inference
            .as_ref()
            .ok_or_else(|| ToolError::Exec {
                tool: "start_inference_server".into(),
                message: "InferenceManager not configured on ToolContext".into(),
            })?
            .clone();
        let runner = ctx
            .python_runner
            .as_ref()
            .ok_or_else(|| ToolError::Exec {
                tool: "start_inference_server".into(),
                message: "PythonRunner not configured on ToolContext".into(),
            })?
            .clone();

        let mut model = ModelSpec::default_qwen2_5_7b_q4km();
        let repo_given = args.repo_id.is_some();
        if let Some(r) = args.repo_id {
            model.repo_id = r;
        }
        if let Some(f) = args.filename {
            model.filename = f;
        }
        // A local domain GGUF path (from export_domain_gguf) is not an HF repo
        // file — label the repo "local" so status is honest and the model-change
        // check keys on the path. (Phase 4.6 slice 3c.)
        if !repo_given && std::path::Path::new(&model.filename).is_absolute() {
            model.repo_id = "local".into();
        }
        if let Some(n) = args.n_ctx {
            model.n_ctx = n;
        }
        if let Some(g) = args.n_gpu_layers {
            model.n_gpu_layers = g;
        }
        if let Some(kv) = args.kv_cache_type {
            model.kv_cache_type = KvCacheType::from_wire(&kv).ok_or_else(|| ToolError::BadInput {
                tool: "start_inference_server".into(),
                reason: format!("unknown kv_cache_type `{kv}` (expected f16, q8_0, or q4_0)"),
            })?;
        }
        if let Some(fa) = args.flash_attn {
            model.flash_attn = fa;
        }
        if let Some(pc) = args.prompt_cache {
            model.prompt_cache = pc;
        }

        let endpoint = manager
            .ensure_running(&runner, &model)
            .await
            .map_err(|e| ToolError::Exec {
                tool: "start_inference_server".into(),
                message: e.to_string(),
            })?;
        let status = manager.status().await;

        let flash_on = model.flash_attn || model.kv_cache_type.requires_flash_attn();
        info!(endpoint = %endpoint, model = %model.repo_id, kv = ?model.kv_cache_type, flash_attn = flash_on, prompt_cache = model.prompt_cache, "start_inference_server");
        Ok(ToolResult::text(format!(
            "inference server up at {endpoint} (model `{}/{}`, n_ctx={}, n_gpu_layers={}, kv_cache={}, flash_attn={}, prompt_cache={})",
            model.repo_id, model.filename, model.n_ctx, model.n_gpu_layers,
            model.kv_cache_type.as_wire(), flash_on, model.prompt_cache
        ))
        .with_structured(serde_json::to_value(&status).unwrap_or(Value::Null)))
    }
}

pub struct StopInferenceServer;

#[async_trait]
impl Tool for StopInferenceServer {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "stop_inference_server".into(),
            description: "Stop the running llama.cpp inference server. Idempotent — returns \
                          ok even if no server was running."
                .into(),
            input_schema: json!({ "type": "object" }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, _args: Value) -> Result<ToolResult, ToolError> {
        let manager = ctx
            .inference
            .as_ref()
            .ok_or_else(|| ToolError::Exec {
                tool: "stop_inference_server".into(),
                message: "InferenceManager not configured on ToolContext".into(),
            })?
            .clone();
        manager.stop().await;
        info!("stop_inference_server");
        Ok(ToolResult::text("inference server stopped (or was not running)"))
    }
}
