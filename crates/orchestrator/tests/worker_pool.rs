//! Integration tests for the long-lived `WorkerPool` against the real Python
//! worker (`uv run python -m narrowmind_workers`).
//!
//! These are contract probes in the spirit of `docs/testing.md`'s lancedb
//! lesson: every property the pool promises (process reuse, serialized
//! concurrency, timeout-kill-respawn, queued-timeout-spares-worker, crash
//! retry, stderr-flood survival, shutdown reaping) gets a test that talks to
//! the genuine subprocess, not a mock — the failure modes live in the OS
//! pipe/process layer, which mocks can't reproduce.
//!
//! The failure-path tests use `debug.*` RPC methods that the Python worker
//! registers only when `NM_DEBUG_RPC=1` (see `narrowmind_workers/debug.py`),
//! so production spawns never expose them.
//!
//! Requires `uv` on PATH + synced workspace; skipped otherwise (same pattern
//! as `hello_round_trip.rs`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use narrowmind_orchestrator::{PythonRunner, WorkerCommand, WorkerError, WorkerPool};
use serde_json::json;

fn workspace_root() -> PathBuf {
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

/// Pool with the debug RPC surface enabled.
fn debug_pool() -> WorkerPool {
    let runner =
        PythonRunner::uv_workspace(workspace_root()).with_env("NM_DEBUG_RPC", "1");
    WorkerPool::new(Arc::new(runner))
}

fn hello_cmd(timeout: Duration) -> WorkerCommand {
    WorkerCommand {
        module: "narrowmind_workers".into(),
        method: "hello".into(),
        params: json!({ "name": "pool" }),
        timeout: Some(timeout),
    }
}

fn debug_cmd(method: &str, params: serde_json::Value, timeout: Duration) -> WorkerCommand {
    WorkerCommand {
        module: "narrowmind_workers".into(),
        method: method.into(),
        params,
        timeout: Some(timeout),
    }
}

/// Extract the worker's pid from a `hello` response.
fn pid_of(value: &serde_json::Value) -> u64 {
    value
        .get("worker_pid")
        .and_then(serde_json::Value::as_u64)
        .expect("hello response carries worker_pid")
}

// The first call in each test pays the cold spawn (uv + interpreter + imports),
// so first-call timeouts are generous; warm-call timeouts can be tight.
const COLD: Duration = Duration::from_secs(60);
const WARM: Duration = Duration::from_secs(20);

#[tokio::test]
async fn pool_reuses_process_across_calls() {
    if !uv_available() {
        eprintln!("skipping: uv not on PATH");
        return;
    }
    let pool = debug_pool();
    let a = pool.call(&hello_cmd(COLD)).await.expect("first call");
    let b = pool.call(&hello_cmd(WARM)).await.expect("second call");
    assert_eq!(
        pid_of(&a),
        pid_of(&b),
        "both calls must be served by the same persistent child"
    );
    pool.shutdown().await;
}

#[tokio::test]
async fn pool_is_lazy_until_first_call() {
    if !uv_available() {
        eprintln!("skipping: uv not on PATH");
        return;
    }
    let pool = debug_pool();
    assert!(
        pool.status().await.is_empty(),
        "no worker entry may exist before the first call"
    );
    pool.call(&hello_cmd(COLD)).await.expect("first call");
    let status = pool.status().await;
    assert_eq!(status.len(), 1);
    assert!(status[0].pid.is_some(), "live worker reports a pid");
    pool.shutdown().await;
}

#[tokio::test]
async fn pool_serializes_concurrent_callers() {
    if !uv_available() {
        eprintln!("skipping: uv not on PATH");
        return;
    }
    let pool = Arc::new(debug_pool());
    // Warm the worker first so the 8 concurrent calls all hit a live child.
    let warm = pool.call(&hello_cmd(COLD)).await.expect("warmup");
    let warm_pid = pid_of(&warm);

    let mut handles = Vec::new();
    for _ in 0..8 {
        let pool = pool.clone();
        handles.push(tokio::spawn(async move {
            pool.call(&hello_cmd(WARM)).await.expect("concurrent call")
        }));
    }
    for h in handles {
        let value = h.await.expect("task join");
        assert_eq!(
            pid_of(&value),
            warm_pid,
            "all concurrent callers share one child; no frame interleaving"
        );
    }
    pool.shutdown().await;
}

#[tokio::test]
async fn pool_rpc_error_keeps_worker_alive() {
    if !uv_available() {
        eprintln!("skipping: uv not on PATH");
        return;
    }
    let pool = debug_pool();
    let first = pool.call(&hello_cmd(COLD)).await.expect("warmup");
    let err = pool
        .call(&debug_cmd("no.such.method", json!({}), WARM))
        .await
        .expect_err("unknown method must error");
    assert!(
        matches!(err, WorkerError::Rpc { .. }),
        "application-level error expected, got: {err}"
    );
    let after = pool.call(&hello_cmd(WARM)).await.expect("worker survives");
    assert_eq!(
        pid_of(&first),
        pid_of(&after),
        "an RPC error must not kill the child"
    );
    pool.shutdown().await;
}

#[tokio::test]
async fn pool_timeout_kills_and_respawns() {
    if !uv_available() {
        eprintln!("skipping: uv not on PATH");
        return;
    }
    let pool = debug_pool();
    let first = pool.call(&hello_cmd(COLD)).await.expect("warmup");

    let err = pool
        .call(&debug_cmd(
            "debug.sleep",
            json!({ "seconds": 30.0 }),
            Duration::from_secs(1),
        ))
        .await
        .expect_err("sleep must out-wait the 1s deadline");
    assert!(
        matches!(err, WorkerError::Timeout { .. }),
        "expected Timeout, got: {err}"
    );

    // The wedged child was killed; the next call respawns transparently.
    let after = pool.call(&hello_cmd(COLD)).await.expect("respawned call");
    assert_ne!(
        pid_of(&first),
        pid_of(&after),
        "in-flight timeout must have killed the old child"
    );
    pool.shutdown().await;
}

#[tokio::test]
async fn pool_queued_timeout_spares_worker() {
    if !uv_available() {
        eprintln!("skipping: uv not on PATH");
        return;
    }
    let pool = Arc::new(debug_pool());
    let first = pool.call(&hello_cmd(COLD)).await.expect("warmup");
    let first_pid = pid_of(&first);

    // Occupy the worker for ~3s with a generous deadline.
    let occupier = {
        let pool = pool.clone();
        tokio::spawn(async move {
            pool.call(&debug_cmd(
                "debug.sleep",
                json!({ "seconds": 3.0 }),
                Duration::from_secs(15),
            ))
            .await
        })
    };
    // Give the occupier a moment to acquire the worker mutex.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // This caller times out while QUEUED — the worker must stay alive.
    let err = pool
        .call(&hello_cmd(Duration::from_millis(500)))
        .await
        .expect_err("queued caller must time out");
    assert!(matches!(err, WorkerError::Timeout { .. }));

    occupier
        .await
        .expect("join")
        .expect("occupier finishes fine");
    let after = pool.call(&hello_cmd(WARM)).await.expect("same worker");
    assert_eq!(
        first_pid,
        pid_of(&after),
        "a queued timeout must NOT kill the busy-but-healthy worker"
    );
    pool.shutdown().await;
}

#[tokio::test]
async fn pool_crash_mid_call_retries_then_surfaces() {
    if !uv_available() {
        eprintln!("skipping: uv not on PATH");
        return;
    }
    let pool = debug_pool();
    pool.call(&hello_cmd(COLD)).await.expect("warmup");

    // debug.exit dies before writing a response — and dies again on the
    // automatic retry. The error must surface, bounded at one retry.
    let err = pool
        .call(&debug_cmd("debug.exit", json!({ "code": 3 }), COLD))
        .await
        .expect_err("a handler that always crashes must surface an error");
    assert!(
        matches!(err, WorkerError::EarlyExit { .. } | WorkerError::Io(_)),
        "expected EarlyExit/Io, got: {err}"
    );

    // The pool itself is not poisoned: next call spawns a fresh child.
    pool.call(&hello_cmd(COLD)).await.expect("pool recovers");
    pool.shutdown().await;
}

#[tokio::test]
async fn pool_survives_stderr_flood() {
    if !uv_available() {
        eprintln!("skipping: uv not on PATH");
        return;
    }
    let pool = debug_pool();
    pool.call(&hello_cmd(COLD)).await.expect("warmup");

    // 1 MB of stderr noise: without a drain task this deadlocks (pipe buffer
    // fills, Python blocks in print, our read never completes → false timeout).
    let value = pool
        .call(&debug_cmd(
            "debug.spam_stderr",
            json!({ "bytes": 1_000_000 }),
            WARM,
        ))
        .await
        .expect("call must complete despite the stderr flood");
    assert!(
        value
            .get("wrote_stderr_bytes")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
            >= 1_000_000
    );
    pool.shutdown().await;
}

#[tokio::test]
async fn pool_shutdown_reaps_children() {
    if !uv_available() {
        eprintln!("skipping: uv not on PATH");
        return;
    }
    let pool = debug_pool();
    pool.call(&hello_cmd(COLD)).await.expect("warmup");
    assert_eq!(pool.status().await.len(), 1);

    pool.shutdown().await;
    let status = pool.status().await;
    // The worker entry may remain but must hold no live child.
    assert!(
        status.iter().all(|s| s.pid.is_none()),
        "no live pid may survive shutdown: {status:?}"
    );

    // Pool is reusable after shutdown: a new call lazily respawns.
    pool.call(&hello_cmd(COLD)).await.expect("respawn after shutdown");
    pool.shutdown().await;
}
