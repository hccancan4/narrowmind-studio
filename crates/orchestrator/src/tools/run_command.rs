//! `run_command` — spawn a process inside the current project's directory and stream its
//! output back to the UI live (one event per line of stdout / stderr).
//!
//! Sandbox rules (per the Phase 1 design decision):
//! - **cwd is pinned to the project root**. No `cwd` argument is accepted.
//! - **No command allowlist.** The agent will need to run training scripts, model
//!   downloads, ingestion code, custom Python tools, etc. — listing every binary up front
//!   is hopeless. Confinement is *spatial* (the project dir), not *executable-name based*.
//! - **600 s default timeout**, overridable per call. Anything still running after the
//!   timeout is killed; the call resolves to `ToolError::Timeout`.
//! - **stdout / stderr stream as RPC notifications** to the UI through
//!   [`super::context::ToolEventSink`]. They are *not* buffered into a single dump.
//! - **Exit code is a structured field** on the returned [`ToolResult`] so the model can
//!   reason about success / failure without parsing prose.

use std::fmt::Write as _;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{info, warn};

use super::context::{ToolContext, ToolEvent, ToolEventSink};
use super::registry::{Tool, ToolDef, ToolError, ToolResult};
use crate::retry::timeouts;

/// How many stdout / stderr lines we keep verbatim in the `tool_result` content. Streaming
/// is unbounded — this cap is just to keep the final summary the model sees from growing
/// without bound when a command spams thousands of lines.
const MAX_LINES_IN_SUMMARY: usize = 200;

#[derive(Debug, Deserialize)]
struct RunCommandArgs {
    /// Executable name or absolute path. Resolved by the OS; **no shell** is invoked.
    command: String,
    /// Argument vector. Each element is passed as a single argv entry — there is no
    /// quote/escape interpretation.
    #[serde(default)]
    args: Vec<String>,
    /// Override the 600 s default deadline (seconds). Killed on expiry.
    #[serde(default)]
    timeout_secs: Option<u64>,
}

pub struct RunCommand;

#[async_trait]
impl Tool for RunCommand {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "run_command".into(),
            description: "Run a process inside the current project's directory. The working \
                          directory is pinned to the project root — there is no `cwd` argument — \
                          but the executable itself is NOT restricted: absolute paths and \
                          anything on PATH are allowed (the agent legitimately runs python, uv, \
                          git, training scripts). UNC (\\\\server\\share) commands are rejected. \
                          Every invocation is recorded in the project's agent.log audit trail. \
                          stdout and stderr stream to the UI live (one event per line). The \
                          final result includes a summary of output plus a structured exit_code \
                          field. Default timeout is 600s; override with `timeout_secs`. No shell \
                          is invoked: `command` is executed directly with `args` as argv."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command":      { "type": "string", "description": "Executable name or absolute path." },
                    "args":         { "type": "array", "items": { "type": "string" }, "default": [] },
                    "timeout_secs": { "type": "integer", "minimum": 1, "default": 600 }
                },
                "required": ["command"]
            }),
        }
    }

    async fn invoke(&self, ctx: &ToolContext, args: Value) -> Result<ToolResult, ToolError> {
        let args: RunCommandArgs =
            serde_json::from_value(args).map_err(|e| ToolError::BadInput {
                tool: "run_command".into(),
                reason: e.to_string(),
            })?;
        let project = ctx.current_project().await.ok_or(ToolError::NoProject)?;
        let deadline_secs = args
            .timeout_secs
            .unwrap_or(timeouts::RUN_COMMAND_DEFAULT.as_secs());

        // UNC rejection: a \\server\share executable is the quietest possible
        // data-exfil / remote-binary route and has no legitimate use in any
        // current phase (training scripts are local python/uv invocations).
        // Everything else is intentionally allowed — see the trust-model note
        // in docs/ARCHITECTURE.md: this tool's sandbox is spatial-by-convention,
        // not a security boundary.
        if args.command.starts_with("\\\\") || args.command.starts_with("//") {
            return Err(ToolError::BadInput {
                tool: "run_command".into(),
                reason: format!(
                    "UNC / network-path executables are not allowed: `{}`",
                    args.command
                ),
            });
        }

        info!(
            command = %args.command,
            args = ?args.args,
            cwd = %project.root.display(),
            timeout_secs = deadline_secs,
            "run_command spawn",
        );
        // Durable audit trail: agent.log survives app restarts and answers
        // "what did the agent actually execute in this project".
        super::util::log_to_agent_log(
            &project.root,
            &format!("run_command: {} {}", args.command, args.args.join(" ")),
        );

        let mut cmd = Command::new(&args.command);
        cmd.args(&args.args)
            .current_dir(&project.root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| ToolError::Exec {
            tool: "run_command".into(),
            message: format!("spawn failed: {e}"),
        })?;

        let stdout = child.stdout.take().ok_or_else(|| ToolError::Exec {
            tool: "run_command".into(),
            message: "stdout pipe missing".into(),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| ToolError::Exec {
            tool: "run_command".into(),
            message: "stderr pipe missing".into(),
        })?;

        let stdout_task = tokio::spawn(forward_lines(stdout, ctx.events.clone(), false));
        let stderr_task = tokio::spawn(forward_lines(stderr, ctx.events.clone(), true));

        let wait_result = timeout(Duration::from_secs(deadline_secs), child.wait()).await;

        let status = match wait_result {
            Err(_) => {
                warn!(timeout_secs = deadline_secs, "run_command timed out, killing child");
                let _ = child.start_kill();
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                return Err(ToolError::Timeout {
                    tool: "run_command".into(),
                    seconds: deadline_secs,
                });
            }
            Ok(Ok(status)) => status,
            Ok(Err(e)) => {
                return Err(ToolError::Exec {
                    tool: "run_command".into(),
                    message: format!("wait failed: {e}"),
                });
            }
        };

        let stdout_lines = stdout_task.await.unwrap_or_default();
        let stderr_lines = stderr_task.await.unwrap_or_default();

        // -1 sentinel for "killed by signal on unix" / "no exit code available". Real exit
        // codes from terminated children on Windows are always Some(_); on Unix a signal-
        // killed child returns None — we surface that as -1 plus the prose summary below.
        let exit_code = status.code().unwrap_or(-1);

        let content = format_summary(&args.command, &args.args, &stdout_lines, &stderr_lines, exit_code);
        Ok(ToolResult::text(content)
            .with_structured(json!({
                "command":      args.command,
                "args":         args.args,
                "exit_code":    exit_code,
                "stdout_lines": stdout_lines.len(),
                "stderr_lines": stderr_lines.len(),
                "cwd":          project.root,
            }))
            .with_exit_code(exit_code))
    }
}

async fn forward_lines<R>(reader: R, sink: ToolEventSink, is_stderr: bool) -> Vec<String>
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(reader).lines();
    let mut collected: Vec<String> = Vec::new();
    loop {
        match reader.next_line().await {
            Ok(Some(line)) => {
                let ev = if is_stderr {
                    ToolEvent::Stderr(line.clone())
                } else {
                    ToolEvent::Stdout(line.clone())
                };
                // Drop on send failure: caller has gone away, no point retrying.
                let _ = sink.send(ev);
                collected.push(line);
            }
            Ok(None) => break,
            Err(e) => {
                warn!(error = %e, "stream read error in run_command");
                break;
            }
        }
    }
    collected
}

fn format_summary(
    command: &str,
    args: &[String],
    stdout: &[String],
    stderr: &[String],
    exit_code: i32,
) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "$ {command} {}", args.join(" "));
    let _ = writeln!(text, "exit_code: {exit_code}");
    append_capped(&mut text, "--- stdout ---", stdout);
    append_capped(&mut text, "--- stderr ---", stderr);
    text
}

fn append_capped(text: &mut String, header: &str, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    let _ = writeln!(text, "{header}");
    if lines.len() <= MAX_LINES_IN_SUMMARY {
        for l in lines {
            let _ = writeln!(text, "{l}");
        }
    } else {
        // Keep the first half and last half of MAX_LINES_IN_SUMMARY so context shows both
        // the boot-up and the failure tail of a noisy command.
        let half = MAX_LINES_IN_SUMMARY / 2;
        let total = lines.len();
        for l in &lines[..half] {
            let _ = writeln!(text, "{l}");
        }
        let _ = writeln!(text, "… {} more lines omitted …", total - MAX_LINES_IN_SUMMARY);
        for l in &lines[total - half..] {
            let _ = writeln!(text, "{l}");
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use tempfile::TempDir;
    use tokio::sync::mpsc;

    use super::*;
    use crate::project::ProjectStore;
    use crate::tools::context::{new_selected_project, ProjectScope};
    use crate::tools::ToolEvent;

    fn python_available() -> bool {
        std::process::Command::new("python")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    fn ctx(root: PathBuf) -> (ToolContext, mpsc::UnboundedReceiver<ToolEvent>) {
        let store = Arc::new(ProjectStore::new(root.parent().unwrap()));
        let (tx, rx) = mpsc::unbounded_channel();
        let scope = ProjectScope {
            name: "demo".into(),
            root,
        };
        let selected = new_selected_project(Some(scope));
        (ToolContext::new(selected, store, tx), rx)
    }

    #[tokio::test]
    async fn unc_command_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("demo");
        std::fs::create_dir_all(&root).unwrap();
        let (ctx, _rx) = ctx(root.clone());

        for unc in ["\\\\server\\share\\evil.exe", "//server/share/evil"] {
            let err = RunCommand
                .invoke(&ctx, json!({ "command": unc }))
                .await
                .expect_err("UNC command must be rejected");
            assert!(
                matches!(err, ToolError::BadInput { .. }),
                "expected BadInput for `{unc}`, got: {err}"
            );
        }
    }

    #[tokio::test]
    async fn invocation_is_audit_logged() {
        if !python_available() {
            eprintln!("skipping: python not on PATH");
            return;
        }
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("demo");
        std::fs::create_dir_all(&root).unwrap();
        let (ctx, _rx) = ctx(root.clone());

        RunCommand
            .invoke(&ctx, json!({ "command": "python", "args": ["-c", "pass"] }))
            .await
            .unwrap();

        let log = std::fs::read_to_string(root.join("agent.log")).expect("agent.log written");
        assert!(
            log.contains("run_command: python -c pass"),
            "audit line missing from agent.log: {log}"
        );
    }

    #[tokio::test]
    async fn captures_exit_code_and_streams_stdout() {
        if !python_available() {
            eprintln!("skipping: python not on PATH");
            return;
        }
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("demo");
        std::fs::create_dir_all(&root).unwrap();
        let (ctx, mut rx) = ctx(root);

        let res = RunCommand
            .invoke(
                &ctx,
                json!({
                    "command": "python",
                    "args": ["-c", "print('hi'); print('there')"]
                }),
            )
            .await
            .unwrap();
        assert_eq!(res.exit_code, Some(0));
        let stdout_events: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|e| match e {
                ToolEvent::Stdout(s) => Some(s),
                _ => None,
            })
            .collect();
        assert_eq!(stdout_events, vec!["hi".to_string(), "there".to_string()]);
        let structured = res.structured.expect("structured payload");
        assert_eq!(structured["exit_code"], 0);
        assert_eq!(structured["stdout_lines"], 2);
    }

    #[tokio::test]
    async fn surfaces_non_zero_exit_code() {
        if !python_available() {
            eprintln!("skipping: python not on PATH");
            return;
        }
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("demo");
        std::fs::create_dir_all(&root).unwrap();
        let (ctx, _rx) = ctx(root);

        let res = RunCommand
            .invoke(
                &ctx,
                json!({
                    "command": "python",
                    "args": ["-c", "import sys; sys.exit(42)"]
                }),
            )
            .await
            .unwrap();
        assert_eq!(res.exit_code, Some(42));
    }

    #[tokio::test]
    async fn cwd_is_pinned_to_project_root() {
        if !python_available() {
            eprintln!("skipping: python not on PATH");
            return;
        }
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("demo");
        std::fs::create_dir_all(&root).unwrap();
        let canon = root.canonicalize().unwrap();
        let (ctx, mut rx) = ctx(root);

        let _ = RunCommand
            .invoke(
                &ctx,
                json!({
                    "command": "python",
                    "args": ["-c", "import os, sys; sys.stdout.write(os.getcwd())"]
                }),
            )
            .await
            .unwrap();
        // python's print without newline still ends the line on flush at process exit
        let mut stdout = String::new();
        while let Ok(ev) = rx.try_recv() {
            if let ToolEvent::Stdout(s) = ev {
                stdout.push_str(&s);
            }
        }
        let reported_cwd = PathBuf::from(stdout.trim());
        let reported_canon = reported_cwd.canonicalize().unwrap_or(reported_cwd);
        assert_eq!(reported_canon, canon);
    }

    #[tokio::test]
    async fn timeout_kills_child_and_returns_timeout_error() {
        if !python_available() {
            eprintln!("skipping: python not on PATH");
            return;
        }
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("demo");
        std::fs::create_dir_all(&root).unwrap();
        let (ctx, _rx) = ctx(root);

        let err = RunCommand
            .invoke(
                &ctx,
                json!({
                    "command": "python",
                    "args": ["-c", "import time; time.sleep(30)"],
                    "timeout_secs": 1
                }),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Timeout { seconds: 1, .. }));
    }

    #[tokio::test]
    async fn no_project_refuses() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(ProjectStore::new(tmp.path()));
        let (tx, _rx) = mpsc::unbounded_channel();
        let selected = new_selected_project(None);
        let ctx = ToolContext::new(selected, store, tx);

        let err = RunCommand
            .invoke(&ctx, json!({ "command": "echo", "args": ["hi"] }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::NoProject));
    }

    #[tokio::test]
    async fn missing_executable_yields_exec_error() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("demo");
        std::fs::create_dir_all(&root).unwrap();
        let (ctx, _rx) = ctx(root);

        let err = RunCommand
            .invoke(
                &ctx,
                json!({ "command": "this-binary-definitely-does-not-exist-1234", "args": [] }),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Exec { .. }));
    }
}
