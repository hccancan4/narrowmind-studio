//! Integration tests for `call_worker_streaming` — the one-shot-with-streaming
//! variant Phase 4 training runs on.
//!
//! Contracts pinned here (against the real Python subprocess, same skip
//! pattern as worker_pool.rs):
//! - notifications arrive in order via the callback, the final result still
//!   parses (id-matching loop);
//! - the idle deadline is ACTIVITY-based: a storm whose total duration
//!   exceeds the idle window but whose inter-notification gap stays under it
//!   must complete (each frame refreshes the timer);
//! - true silence past the idle window kills the child and surfaces Timeout;
//! - flipping the cancel channel kills the child and surfaces Cancelled;
//! - a plain no-notification call still round-trips.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use narrowmind_orchestrator::{call_worker_streaming, PythonRunner, WorkerCommand, WorkerError};
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

fn debug_runner() -> PythonRunner {
    PythonRunner::uv_workspace(workspace_root()).with_env("NM_DEBUG_RPC", "1")
}

fn cmd(method: &str, params: serde_json::Value) -> WorkerCommand {
    WorkerCommand {
        module: "narrowmind_workers".into(),
        method: method.into(),
        params,
        timeout: None, // streaming path uses idle_timeout, not this
    }
}

/// Cold spawn allowance (uv + interpreter + imports) — the idle window must
/// at least cover the gap between spawn and the first frame.
const COLD_IDLE: Duration = Duration::from_secs(60);

#[tokio::test]
async fn streams_notifications_in_order_then_result() {
    if !uv_available() {
        eprintln!("skipping: uv not on PATH");
        return;
    }
    let runner = debug_runner();
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let seen: Arc<Mutex<Vec<(String, u64)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_cb = seen.clone();

    let result = call_worker_streaming(
        &runner,
        &cmd("debug.notify_storm", json!({ "count": 5, "interval_ms": 0 })),
        COLD_IDLE,
        cancel_rx,
        move |method, params| {
            let seq = params.get("seq").and_then(serde_json::Value::as_u64).unwrap_or(999);
            seen_cb.lock().unwrap().push((method.to_string(), seq));
        },
    )
    .await
    .expect("storm call succeeds");

    assert_eq!(result.get("notified").and_then(serde_json::Value::as_u64), Some(5));
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 5, "all notifications surfaced via callback");
    for (i, (method, seq)) in seen.iter().enumerate() {
        assert_eq!(method, "debug.tick");
        assert_eq!(*seq as usize, i, "notifications arrive in order");
    }
}

#[tokio::test]
async fn idle_deadline_is_refreshed_by_notifications() {
    if !uv_available() {
        eprintln!("skipping: uv not on PATH");
        return;
    }
    // 8 notifications × 700 ms ≈ 5.6 s total — far past a 3 s idle window,
    // but each 700 ms gap stays well inside it. Must complete.
    // (First frame allowance: the worker emits tick 0 after ~700 ms once
    // imports finish; cold spawn is covered because the idle timer starts
    // at call time — so give the FIRST window a longer fuse by warming up
    // with a fast hello first? No: idle window must cover cold spawn. Use
    // 30 s idle and 35 s total instead? That makes the test slow. Compromise:
    // idle = 15 s covers cold spawn comfortably; storm 8 × 700 ms total
    // 5.6 s < 15 s idle — this would pass even without refresh. To actually
    // prove refresh, make total > idle: 25 ticks × 700 ms = 17.5 s > 15 s.)
    let runner = debug_runner();
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let count = Arc::new(Mutex::new(0_u64));
    let count_cb = count.clone();

    let result = call_worker_streaming(
        &runner,
        &cmd("debug.notify_storm", json!({ "count": 25, "interval_ms": 700 })),
        Duration::from_secs(15),
        cancel_rx,
        move |_m, _p| {
            *count_cb.lock().unwrap() += 1;
        },
    )
    .await
    .expect("a 17.5s storm must survive a 15s idle window when frames keep arriving");

    assert_eq!(result.get("notified").and_then(serde_json::Value::as_u64), Some(25));
    assert_eq!(*count.lock().unwrap(), 25);
}

#[tokio::test]
async fn true_silence_kills_and_times_out() {
    if !uv_available() {
        eprintln!("skipping: uv not on PATH");
        return;
    }
    let runner = debug_runner();
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

    // debug.sleep emits nothing until it returns at 60 s — but our idle
    // window is 8 s (long enough for the cold spawn, short enough to trip
    // during the silent sleep).
    let err = call_worker_streaming(
        &runner,
        &cmd("debug.sleep", json!({ "seconds": 60.0 })),
        Duration::from_secs(8),
        cancel_rx,
        |_m, _p| {},
    )
    .await
    .expect_err("silent worker must be reaped");

    assert!(
        matches!(err, WorkerError::Timeout { .. }),
        "expected Timeout, got: {err}"
    );
}

#[tokio::test]
async fn cancel_kills_inflight_call() {
    if !uv_available() {
        eprintln!("skipping: uv not on PATH");
        return;
    }
    let runner = debug_runner();
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

    // Long storm; cancel from a side task after the first few ticks.
    let storm = cmd("debug.notify_storm", json!({ "count": 100, "interval_ms": 500 }));
    let call = call_worker_streaming(&runner, &storm, COLD_IDLE, cancel_rx, |_m, _p| {});
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(10)).await; // let it spawn + tick a bit
        let _ = cancel_tx.send(true);
    });

    let err = call.await.expect_err("cancelled call must error");
    canceller.await.unwrap();
    assert!(
        matches!(err, WorkerError::Cancelled { .. }),
        "expected Cancelled, got: {err}"
    );
}

#[tokio::test]
async fn plain_call_without_notifications_round_trips() {
    if !uv_available() {
        eprintln!("skipping: uv not on PATH");
        return;
    }
    let runner = debug_runner();
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let mut got_notification = false;

    let result = call_worker_streaming(
        &runner,
        &cmd("hello", json!({ "name": "stream" })),
        COLD_IDLE,
        cancel_rx,
        |_m, _p| {
            got_notification = true;
        },
    )
    .await
    .expect("hello round-trips on the streaming path");

    assert_eq!(
        result.get("message").and_then(serde_json::Value::as_str),
        Some("hello, stream")
    );
    assert!(!got_notification, "hello emits no notifications");
}
