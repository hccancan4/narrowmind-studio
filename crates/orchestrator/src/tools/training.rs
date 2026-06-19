//! Training lifecycle agent tools (Phase 4): `start_training`, `stop_training`,
//! `list_runs`, `test_adapter` (M6).
//!
//! Thin facades over [`crate::training::TrainingManager`] — the same pattern
//! the inference tools use over `InferenceManager`. Heavy lifting (VRAM
//! mutex, run bookkeeping, the streaming worker call) lives in the manager;
//! tools validate inputs against the registry + dataset and translate
//! errors into something the model can act on.

use std::fmt::Write as _;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::info;

use super::context::ToolContext;
use super::registry::{Tool, ToolDef, ToolError, ToolResult};
use super::util::{emit_progress, log_to_agent_log};
use crate::models;
use crate::training::{preset, worker_config, wsl};

// ---------------------------------------------------------------------------
// start_training
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct StartTrainingArgs {
    /// Registry model id. Defaults to the registry default (Qwen2.5-7B).
    #[serde(default)]
    base_model_id: Option<String>,
    /// Resume from a previous run's latest checkpoint (KARAR 6: manual only).
    #[serde(default)]
    resume_from: Option<String>,
    // Per-call overrides — win over project.toml [training], which wins over
    // the preset. Only the OOM-relevant + experiment-relevant knobs.
    #[serde(default)]
    lora_r: Option<u32>,
    #[serde(default)]
    max_seq_length: Option<u32>,
    #[serde(default)]
    learning_rate: Option<f64>,
    #[serde(default)]
    num_train_epochs: Option<u32>,
    #[serde(default)]
    seed: Option<u64>,
}

pub struct StartTraining;

#[async_trait]
impl Tool for StartTraining {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "start_training".into(),
            description: "Start a QLoRA fine-tune of the project's SFT dataset \
                          (datasets/sft.jsonl) on a registry base model. Runs in the WSL2 \
                          training environment via Unsloth with the rtx-3070-8gb preset; \
                          project.toml [training] and the args here override preset knobs. \
                          VRAM hard mutex: a running inference server is STOPPED first and \
                          is NOT auto-restarted afterwards. Returns the run_id immediately — \
                          progress streams to the Training Monitor and runs/<id>/metrics.jsonl. \
                          If training OOMs, lower max_seq_length stepwise 2048→1536→1024."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "base_model_id":    { "type": "string", "description": "Registry id (default: registry default model). Must be training_supported." },
                    "resume_from":      { "type": "string", "description": "Previous run_id to resume from its latest checkpoint." },
                    "lora_r":           { "type": "integer", "minimum": 1, "maximum": 256 },
                    "max_seq_length":   { "type": "integer", "minimum": 64, "maximum": 32768 },
                    "learning_rate":    { "type": "number" },
                    "num_train_epochs": { "type": "integer", "minimum": 1, "maximum": 100 },
                    "seed":             { "type": "integer" }
                }
            }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, args: Value) -> Result<ToolResult, ToolError> {
        let args: StartTrainingArgs =
            serde_json::from_value(args).map_err(|e| ToolError::BadInput {
                tool: "start_training".into(),
                reason: e.to_string(),
            })?;
        let project = ctx.current_project().await.ok_or(ToolError::NoProject)?;
        let manager = ctx.training.as_ref().ok_or_else(|| ToolError::Exec {
            tool: "start_training".into(),
            message: "TrainingManager not configured on ToolContext".into(),
        })?;
        let runner = ctx.training_runner.as_ref().ok_or_else(|| ToolError::Exec {
            tool: "start_training".into(),
            message: "training runner not configured on ToolContext".into(),
        })?;
        let inference = ctx.inference.as_ref().ok_or_else(|| ToolError::Exec {
            tool: "start_training".into(),
            message: "InferenceManager not configured on ToolContext".into(),
        })?;

        // --- Registry validation -------------------------------------------
        let base = match args.base_model_id.as_deref() {
            None => models::default_model(),
            Some(id) => models::get(id).ok_or_else(|| ToolError::BadInput {
                tool: "start_training".into(),
                reason: format!(
                    "unknown base_model_id `{id}`; available: {}",
                    models::all().iter().map(|m| m.id).collect::<Vec<_>>().join(", ")
                ),
            })?,
        };
        if !base.training_supported {
            return Err(ToolError::BadInput {
                tool: "start_training".into(),
                reason: format!("model `{}` is not training_supported", base.id),
            });
        }

        // --- Dataset validation (>= 10 usable pairs) ------------------------
        let sft_path = project.root.join("datasets").join("sft.jsonl");
        let pair_count = count_jsonl_pairs(&sft_path);
        if pair_count < 10 {
            return Err(ToolError::Exec {
                tool: "start_training".into(),
                message: format!(
                    "datasets/sft.jsonl has {pair_count} usable pairs (need >= 10) — run generate_sft first"
                ),
            });
        }

        // --- Resume validation ----------------------------------------------
        if let Some(prev) = args.resume_from.as_deref() {
            let prev_dir = project.root.join("runs").join(prev).join("checkpoints");
            if !prev_dir.is_dir() {
                return Err(ToolError::BadInput {
                    tool: "start_training".into(),
                    reason: format!("resume_from run `{prev}` has no checkpoints directory"),
                });
            }
        }

        // --- Config assembly: preset < project.toml < tool args --------------
        let cfg_proj = ctx
            .project_store
            .get(&project.name)
            .map_err(ToolError::Project)?;
        let mut tcfg = cfg_proj.training.clone();
        if args.lora_r.is_some() {
            tcfg.lora_r = args.lora_r;
        }
        if args.max_seq_length.is_some() {
            tcfg.max_seq_length = args.max_seq_length;
        }
        if args.learning_rate.is_some() {
            tcfg.learning_rate = args.learning_rate;
        }
        if args.num_train_epochs.is_some() {
            tcfg.num_train_epochs = args.num_train_epochs;
        }
        let preset = preset::get(&tcfg.preset).ok_or_else(|| ToolError::BadInput {
            tool: "start_training".into(),
            reason: format!("unknown training preset `{}` in project.toml", tcfg.preset),
        })?;
        // Seed: explicit arg > project [training] seed > derived from the
        // synth split seed (same project trains reproducibly by default).
        // Masked to u32: HF Trainer / numpy reject anything past 2**32-1.
        let seed = args
            .seed
            .or(tcfg.seed)
            .or_else(|| cfg_proj.synth.as_ref().map(|s| s.split_seed ^ 0x5EED))
            .unwrap_or(42)
            & 0xFFFF_FFFF;

        let wcfg = worker_config(&tcfg, preset, base, seed);
        let idle = std::time::Duration::from_secs(tcfg.idle_timeout_secs);

        let msg = format!(
            "starting QLoRA: {} on {pair_count} pairs (preset {}, seq {}, r {}, {} epochs, seed {seed})",
            base.id,
            preset.id,
            tcfg.max_seq_length.unwrap_or(preset.max_seq_length),
            tcfg.lora_r.unwrap_or(preset.lora_r),
            tcfg.num_train_epochs.unwrap_or(preset.num_train_epochs),
        );
        emit_progress(&ctx.events, &msg);
        log_to_agent_log(&project.root, &msg);

        let run_id = manager
            .start(
                runner.clone(),
                project.name.clone(),
                project.root.clone(),
                wcfg,
                idle,
                args.resume_from.clone(),
                inference.clone(),
            )
            .await
            .map_err(|e| ToolError::Exec {
                tool: "start_training".into(),
                message: e,
            })?;

        info!(run_id = %run_id, model = base.id, pairs = pair_count, "start_training");
        Ok(ToolResult::text(format!(
            "training run `{run_id}` started ({}, {pair_count} pairs). Progress streams to the \
             Training tab; metrics persist to runs/{run_id}/metrics.jsonl. The inference server \
             (if it was running) was stopped to free VRAM and will NOT auto-restart — when \
             training finishes, restart it with start_inference_server or the Local chat button.",
            base.display_name
        ))
        .with_structured(json!({
            "run_id": run_id,
            "base_model_id": base.id,
            "pairs": pair_count,
            "resumed_from": args.resume_from,
        })))
    }
}

// ---------------------------------------------------------------------------
// stop_training
// ---------------------------------------------------------------------------

pub struct StopTraining;

#[async_trait]
impl Tool for StopTraining {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "stop_training".into(),
            description: "Cancel the active training run. Partial checkpoints remain in the \
                          run directory and can be resumed later with start_training's \
                          resume_from argument. Idempotent — reports when nothing is running."
                .into(),
            input_schema: json!({ "type": "object" }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, _args: Value) -> Result<ToolResult, ToolError> {
        let manager = ctx.training.as_ref().ok_or_else(|| ToolError::Exec {
            tool: "stop_training".into(),
            message: "TrainingManager not configured on ToolContext".into(),
        })?;
        let status = manager.status().await;
        if manager.stop().await {
            info!(run_id = ?status.run_id, "stop_training");
            Ok(ToolResult::text(format!(
                "cancellation requested for run {} (was at step {}/{}); checkpoints up to the \
                 last completed epoch remain resumable",
                status.run_id.unwrap_or_default(),
                status.step,
                status.total_steps
            )))
        } else {
            Ok(ToolResult::text("no training run is active"))
        }
    }
}

// ---------------------------------------------------------------------------
// list_runs
// ---------------------------------------------------------------------------

pub struct ListRuns;

#[async_trait]
impl Tool for ListRuns {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "list_runs".into(),
            description: "List this project's training runs (from runs/*/status.json): id, \
                          status, progress, base model, best eval_loss when finished."
                .into(),
            input_schema: json!({ "type": "object" }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, _args: Value) -> Result<ToolResult, ToolError> {
        let project = ctx.current_project().await.ok_or(ToolError::NoProject)?;
        let runs = read_runs(&project.root);

        let mut text = String::new();
        if runs.is_empty() {
            text.push_str("(no training runs yet)\n");
        } else {
            text.push_str("run                               status      step        model\n");
            for r in &runs {
                let _ = writeln!(
                    text,
                    "{:<34}{:<12}{:<12}{}",
                    r.get("run_id").and_then(Value::as_str).unwrap_or("?"),
                    r.get("status").and_then(Value::as_str).unwrap_or("?"),
                    format!(
                        "{}/{}",
                        r.get("step").and_then(Value::as_u64).unwrap_or(0),
                        r.get("total_steps").and_then(Value::as_u64).unwrap_or(0)
                    ),
                    r.get("base_model").and_then(Value::as_str).unwrap_or("?"),
                );
            }
        }
        info!(count = runs.len(), "list_runs");
        Ok(ToolResult::text(text).with_structured(json!({ "runs": runs })))
    }
}

// ---------------------------------------------------------------------------
// test_adapter (M6 — KARAR 8)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct TestAdapterArgs {
    /// Run id whose adapter to test. Defaults to project.toml [training] last_run_id.
    #[serde(default)]
    run_id: Option<String>,
    /// How many eval questions to sample (default 3, max 5 — this is a smoke
    /// test, not an eval; the Phase 5 Eval Console owns real comparisons).
    #[serde(default)]
    questions: Option<u32>,
}

pub struct TestAdapter;

#[async_trait]
impl Tool for TestAdapter {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "test_adapter".into(),
            description: "Smoke-test a trained adapter: load base(4-bit)+LoRA in the training \
                          environment and answer the first few questions from datasets/eval.jsonl. \
                          Slow by design (verification aid, not serving). VRAM hard mutex applies — \
                          a running inference server is stopped first and not auto-restarted."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "run_id":    { "type": "string", "description": "Defaults to [training] last_run_id." },
                    "questions": { "type": "integer", "minimum": 1, "maximum": 5, "default": 3 }
                }
            }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, args: Value) -> Result<ToolResult, ToolError> {
        let args: TestAdapterArgs =
            serde_json::from_value(args).map_err(|e| ToolError::BadInput {
                tool: "test_adapter".into(),
                reason: e.to_string(),
            })?;
        let project = ctx.current_project().await.ok_or(ToolError::NoProject)?;
        let runner = ctx.training_runner.as_ref().ok_or_else(|| ToolError::Exec {
            tool: "test_adapter".into(),
            message: "training runner not configured on ToolContext".into(),
        })?;
        let manager = ctx.training.as_ref().ok_or_else(|| ToolError::Exec {
            tool: "test_adapter".into(),
            message: "TrainingManager not configured on ToolContext".into(),
        })?;
        let inference = ctx.inference.as_ref().ok_or_else(|| ToolError::Exec {
            tool: "test_adapter".into(),
            message: "InferenceManager not configured on ToolContext".into(),
        })?;
        if manager.is_active().await {
            return Err(ToolError::Exec {
                tool: "test_adapter".into(),
                message: "a training run is active; wait for it or stop_training first".into(),
            });
        }

        // Resolve run id.
        let run_id = match args.run_id {
            Some(id) => id,
            None => ctx
                .project_store
                .get(&project.name)
                .map_err(ToolError::Project)?
                .training
                .last_run_id
                .ok_or_else(|| ToolError::BadInput {
                    tool: "test_adapter".into(),
                    reason: "no run_id given and project has no [training] last_run_id".into(),
                })?,
        };

        // First N eval questions.
        let n = args.questions.unwrap_or(3).clamp(1, 5) as usize;
        let eval_path = project.root.join("datasets").join("eval.jsonl");
        let questions = read_eval_questions(&eval_path, n);
        if questions.is_empty() {
            return Err(ToolError::Exec {
                tool: "test_adapter".into(),
                message: format!("no usable questions in {}", eval_path.display()),
            });
        }

        // VRAM hard mutex (KARAR 1 applies here too).
        if inference.status().await.running {
            inference.stop().await;
            log_to_agent_log(
                &project.root,
                "inference server stopped to free VRAM for adapter test",
            );
        }

        emit_progress(
            &ctx.events,
            &format!("loading base+adapter for run {run_id} ({} questions)…", questions.len()),
        );

        let (_, cancel_rx) = tokio::sync::watch::channel(false);
        let events = ctx.events.clone();
        let params = json!({
            "project_root": wsl::to_wsl_path(&project.root),
            "run_id": run_id,
            "questions": questions,
            "max_new_tokens": 256,
        });
        let cmd = crate::worker::WorkerCommand {
            module: "narrowmind_workers.training".into(),
            method: "training.test_adapter".into(),
            params,
            timeout: None,
        };
        // Model load can take minutes (5 GB through the WSL cache); generation
        // emits a notification per answered question, refreshing the deadline.
        let value = crate::worker::call_worker_streaming(
            runner,
            &cmd,
            std::time::Duration::from_secs(600),
            cancel_rx,
            move |method, p| {
                if method == "training.adapter_answer" {
                    let i = p.get("index").and_then(Value::as_u64).unwrap_or(0) + 1;
                    let of = p.get("of").and_then(Value::as_u64).unwrap_or(0);
                    let _ = events.send(super::context::ToolEvent::Progress {
                        message: format!("adapter answered question {i}/{of}"),
                    });
                }
            },
        )
        .await
        .map_err(|e| ToolError::Exec {
            tool: "test_adapter".into(),
            message: e.to_string(),
        })?;

        let mut text = String::new();
        let _ = writeln!(text, "adapter smoke test — run {run_id}\n");
        if let Some(answers) = value.get("answers").and_then(Value::as_array) {
            for (i, a) in answers.iter().enumerate() {
                let _ = writeln!(
                    text,
                    "Q{}: {}\nA{}: {}\n",
                    i + 1,
                    a.get("question").and_then(Value::as_str).unwrap_or("?"),
                    i + 1,
                    a.get("answer").and_then(Value::as_str).unwrap_or("?"),
                );
            }
        }
        text.push_str(
            "note: inference server was stopped for VRAM; restart with start_inference_server \
             or the Local chat button.",
        );
        Ok(ToolResult::text(text).with_structured(value))
    }
}

// ---------------------------------------------------------------------------
// export_domain_gguf (Phase 4.6 slice 3) — merge adapter -> quantized GGUF
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ExportGgufArgs {
    /// Run id whose adapter to export. Defaults to project.toml [training] `last_run_id`.
    #[serde(default)]
    run_id: Option<String>,
    /// llama-quantize type (default `Q4_K_M`).
    #[serde(default)]
    quant: Option<String>,
    /// GGUF filename slug; defaults to the project name.
    #[serde(default)]
    domain: Option<String>,
}

pub struct ExportDomainGguf;

#[async_trait]
impl Tool for ExportDomainGguf {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "export_domain_gguf".into(),
            description: "Merge a trained LoRA adapter into its base and produce ONE quantized \
                          domain GGUF (default Q4_K_M) under the project's models/ dir — the file \
                          the llama.cpp server loads to serve your fine-tune. Runs the merge in the \
                          WSL training env, then convert + quantize via the vendored llama.cpp \
                          build; the ~15 GB 16-bit/f16 intermediates stage on ext4 and are removed. \
                          VRAM hard mutex applies. Slow (several minutes for a 7B merge + convert)."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "run_id": { "type": "string", "description": "Defaults to [training] last_run_id." },
                    "quant":  { "type": "string", "default": "Q4_K_M", "description": "llama-quantize type, e.g. Q4_K_M, Q5_K_M, Q8_0." },
                    "domain": { "type": "string", "description": "GGUF filename slug; defaults to the project name." }
                }
            }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, args: Value) -> Result<ToolResult, ToolError> {
        let args: ExportGgufArgs = serde_json::from_value(args).map_err(|e| ToolError::BadInput {
            tool: "export_domain_gguf".into(),
            reason: e.to_string(),
        })?;
        let project = ctx.current_project().await.ok_or(ToolError::NoProject)?;
        let runner = ctx.training_runner.as_ref().ok_or_else(|| ToolError::Exec {
            tool: "export_domain_gguf".into(),
            message: "training runner not configured on ToolContext".into(),
        })?;
        let manager = ctx.training.as_ref().ok_or_else(|| ToolError::Exec {
            tool: "export_domain_gguf".into(),
            message: "TrainingManager not configured on ToolContext".into(),
        })?;
        let inference = ctx.inference.as_ref().ok_or_else(|| ToolError::Exec {
            tool: "export_domain_gguf".into(),
            message: "InferenceManager not configured on ToolContext".into(),
        })?;
        if manager.is_active().await {
            return Err(ToolError::Exec {
                tool: "export_domain_gguf".into(),
                message: "a training run is active; wait for it or stop_training first".into(),
            });
        }

        let run_id = match args.run_id {
            Some(id) => id,
            None => ctx
                .project_store
                .get(&project.name)
                .map_err(ToolError::Project)?
                .training
                .last_run_id
                .ok_or_else(|| ToolError::BadInput {
                    tool: "export_domain_gguf".into(),
                    reason: "no run_id given and project has no [training] last_run_id".into(),
                })?,
        };
        let quant = args.quant.unwrap_or_else(|| "Q4_K_M".into());

        // Fail fast before the WSL spawn if there's no adapter.
        let adapter_dir = project.root.join("runs").join(&run_id).join("adapter");
        if !adapter_dir.is_dir() {
            return Err(ToolError::BadInput {
                tool: "export_domain_gguf".into(),
                reason: format!("run `{run_id}` has no adapter dir — train first"),
            });
        }

        // VRAM hard mutex (KARAR 1).
        if inference.status().await.running {
            inference.stop().await;
            log_to_agent_log(&project.root, "inference server stopped to free VRAM for GGUF export");
        }

        emit_progress(
            &ctx.events,
            &format!("exporting run {run_id} → {quant} GGUF (merge + convert + quantize)…"),
        );

        let (_, cancel_rx) = tokio::sync::watch::channel(false);
        let events = ctx.events.clone();
        let mut params = json!({
            "project_root": wsl::to_wsl_path(&project.root),
            "run_id": run_id,
            "quant": quant.clone(),
        });
        if let Some(domain) = args.domain {
            params["domain"] = json!(domain);
        }
        let cmd = crate::worker::WorkerCommand {
            module: "narrowmind_workers.training".into(),
            method: "training.export_gguf".into(),
            params,
            timeout: None,
        };
        // Stages (merge → convert → quantize) each notify, refreshing the
        // deadline; within a stage a ~15 GB write can stay silent for minutes,
        // so the idle window is generous.
        let value = crate::worker::call_worker_streaming(
            runner,
            &cmd,
            std::time::Duration::from_secs(1800),
            cancel_rx,
            move |method, p| {
                if method == "export.stage" {
                    if let Some(stage) = p.get("stage").and_then(Value::as_str) {
                        let _ = events.send(super::context::ToolEvent::Progress {
                            message: format!("export: {stage}"),
                        });
                    }
                }
            },
        )
        .await
        .map_err(|e| ToolError::Exec {
            tool: "export_domain_gguf".into(),
            message: e.to_string(),
        })?;

        let gguf_path = value.get("gguf_path").and_then(Value::as_str).unwrap_or("?");
        let size_mb = value.get("size_mb").and_then(Value::as_u64).unwrap_or(0);
        info!(gguf = %gguf_path, size_mb, quant = %quant, "export_domain_gguf");
        Ok(ToolResult::text(format!(
            "exported domain GGUF: {gguf_path} ({size_mb} MB, {quant}). Load it with \
             start_inference_server (filename set to this path), or see list_domain_models. \
             Inference server was stopped for VRAM."
        ))
        .with_structured(value))
    }
}

// ---------------------------------------------------------------------------
// list_domain_models (Phase 4.6 slice 3) — the project's exported GGUFs
// ---------------------------------------------------------------------------

pub struct ListDomainModels;

#[async_trait]
impl Tool for ListDomainModels {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "list_domain_models".into(),
            description: "List the domain GGUF files exported for the current project (its models/ \
                          dir). These merged + quantized fine-tunes are what the inference server \
                          loads to serve a domain model."
                .into(),
            input_schema: json!({ "type": "object" }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, _args: Value) -> Result<ToolResult, ToolError> {
        let project = ctx.current_project().await.ok_or(ToolError::NoProject)?;
        let models = read_domain_models(&project.root.join("models"));
        let mut text = String::new();
        if models.is_empty() {
            text.push_str("no domain GGUFs yet — run export_domain_gguf after training.");
        } else {
            let _ = writeln!(text, "domain GGUFs ({}):", models.len());
            for m in &models {
                let _ = writeln!(
                    text,
                    "  {} ({} MB)",
                    m.get("filename").and_then(Value::as_str).unwrap_or("?"),
                    m.get("size_mb").and_then(Value::as_u64).unwrap_or(0),
                );
            }
        }
        info!(count = models.len(), "list_domain_models");
        Ok(ToolResult::text(text).with_structured(json!({ "models": models })))
    }
}

/// Read `models/*.gguf` as `{filename, path, size_mb}`, sorted by filename.
fn read_domain_models(models_dir: &std::path::Path) -> Vec<Value> {
    let mut models = Vec::new();
    let Ok(entries) = std::fs::read_dir(models_dir) else {
        return models;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("gguf") {
            let size_mb = entry.metadata().map_or(0, |m| m.len() / (1024 * 1024));
            let filename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string();
            models.push(json!({
                "filename": filename,
                "path": path.to_string_lossy(),
                "size_mb": size_mb,
            }));
        }
    }
    models.sort_by(|a, b| a["filename"].as_str().cmp(&b["filename"].as_str()));
    models
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Count sft.jsonl lines that parse as {question, answer} with non-empty strings.
fn count_jsonl_pairs(path: &std::path::Path) -> usize {
    let Ok(content) = std::fs::read_to_string(path) else {
        return 0;
    };
    content
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .filter(|v| {
            v.get("question").and_then(Value::as_str).is_some_and(|s| !s.is_empty())
                && v.get("answer").and_then(Value::as_str).is_some_and(|s| !s.is_empty())
        })
        .count()
}

/// First `n` questions from eval.jsonl.
fn read_eval_questions(path: &std::path::Path, n: usize) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .filter_map(|v| v.get("question").and_then(Value::as_str).map(String::from))
        .filter(|q| !q.is_empty())
        .take(n)
        .collect()
}

/// Load every runs/*/status.json, newest first by started_at string ordering
/// (ISO-8601 sorts lexicographically).
fn read_runs(project_root: &std::path::Path) -> Vec<Value> {
    let runs_dir = project_root.join("runs");
    let Ok(entries) = std::fs::read_dir(&runs_dir) else {
        return Vec::new();
    };
    let mut out: Vec<Value> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let status = e.path().join("status.json");
            std::fs::read_to_string(status)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
        })
        .collect();
    out.sort_by(|a, b| {
        let ka = a.get("started_at").and_then(Value::as_str).unwrap_or("");
        let kb = b.get("started_at").and_then(Value::as_str).unwrap_or("");
        kb.cmp(ka)
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_pairs_filters_unusable_lines(){
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("sft.jsonl");
        std::fs::write(
            &p,
            concat!(
                r#"{"question":"q","answer":"a"}"#, "\n",
                "garbage\n",
                r#"{"question":"","answer":"a"}"#, "\n",
                r#"{"question":"q2","answer":"a2"}"#, "\n",
            ),
        )
        .unwrap();
        assert_eq!(count_jsonl_pairs(&p), 2);
        assert_eq!(count_jsonl_pairs(&tmp.path().join("missing.jsonl")), 0);
    }

    #[test]
    fn eval_questions_take_first_n() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("eval.jsonl");
        std::fs::write(
            &p,
            concat!(
                r#"{"question":"q1","answer":"a"}"#, "\n",
                r#"{"question":"q2","answer":"a"}"#, "\n",
                r#"{"question":"q3","answer":"a"}"#, "\n",
            ),
        )
        .unwrap();
        assert_eq!(read_eval_questions(&p, 2), vec!["q1", "q2"]);
    }

    #[test]
    fn read_runs_sorts_newest_first() {
        let tmp = tempfile::TempDir::new().unwrap();
        for (id, ts) in [("old", "2026-06-10T00:00:00Z"), ("new", "2026-06-12T00:00:00Z")] {
            let d = tmp.path().join("runs").join(id);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join("status.json"),
                format!(r#"{{"run_id":"{id}","status":"completed","started_at":"{ts}"}}"#),
            )
            .unwrap();
        }
        let runs = read_runs(tmp.path());
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0]["run_id"], "new");
    }

    #[test]
    fn read_domain_models_filters_gguf_and_sorts() {
        let tmp = tempfile::TempDir::new().unwrap();
        let models = tmp.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join("zeta-q4_k_m.gguf"), b"x").unwrap();
        std::fs::write(models.join("alpha-q8_0.gguf"), b"xx").unwrap();
        std::fs::write(models.join("notes.txt"), b"ignore me").unwrap();
        let out = read_domain_models(&models);
        assert_eq!(out.len(), 2, "only .gguf files");
        // sorted by filename
        assert_eq!(out[0]["filename"], "alpha-q8_0.gguf");
        assert_eq!(out[1]["filename"], "zeta-q4_k_m.gguf");
        // missing dir -> empty, no panic
        assert!(read_domain_models(&tmp.path().join("nope")).is_empty());
    }
}
