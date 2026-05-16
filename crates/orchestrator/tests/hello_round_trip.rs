//! End-to-end: spawn the real Python worker via `uv run` and round-trip a `hello` call.
//!
//! Requires `uv` on PATH and the workspace synced (`uv sync` at the repo root). CI runs
//! `uv sync` before `cargo test`, and local devs get the same via `.nm-env.ps1` + `uv sync`.
//! If `uv` is missing the test is skipped so contributors without the Python toolchain can
//! still iterate on Rust code.

use std::path::PathBuf;

use narrowmind_orchestrator::{hello_round_trip, PythonRunner};

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at crates/orchestrator; the workspace root is two levels up.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn uv_available() -> bool {
    std::process::Command::new("uv")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[tokio::test]
async fn hello_named_round_trip() {
    if !uv_available() {
        eprintln!("skipping: uv not on PATH");
        return;
    }

    let runner = PythonRunner::uv_workspace(workspace_root());
    let result = hello_round_trip(&runner, Some("rust"))
        .await
        .expect("worker round-trip succeeded");

    assert_eq!(result.message, "hello, rust");
    assert!(result.worker_pid > 0);
    assert!(!result.worker_version.is_empty());
    assert!(result.python_version.starts_with("3."));
}

#[tokio::test]
async fn hello_default_name() {
    if !uv_available() {
        eprintln!("skipping: uv not on PATH");
        return;
    }

    let runner = PythonRunner::uv_workspace(workspace_root());
    let result = hello_round_trip(&runner, None)
        .await
        .expect("worker round-trip succeeded");

    assert_eq!(result.message, "hello, world");
}
