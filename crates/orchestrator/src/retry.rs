//! Shared exponential-backoff retry and the orchestrator's timeout registry.
//!
//! Before this module, retry logic existed exactly once (hand-rolled inside
//! `synth_gen`) while every other network-touching path — ingestion scrapes,
//! worker spawns, inference health checks — died on the first transient
//! failure. And per-call timeouts were magic numbers scattered across five
//! files, with no record of why tool X chose 600 s.
//!
//! Two exports:
//! - [`with_backoff`] + [`Retry`]: a generic retry loop where the operation
//!   itself classifies each failure as transient (worth retrying with
//!   exponential backoff) or permanent (surface immediately). The classifier
//!   lives at the call site because only the caller knows its semantics —
//!   e.g. HTTP 429 is transient, an unparseable LLM response is permanent.
//! - [`timeouts`]: every cross-boundary deadline in one place, each with the
//!   reasoning that produced the number.

use std::future::Future;
use std::time::Duration;

use tracing::warn;

/// Failure classification returned by a retried operation.
#[derive(Debug)]
pub enum Retry<E> {
    /// Worth another attempt after backoff (rate limits, 5xx, broken pipes,
    /// flaky network). The error is kept so the last one can be surfaced if
    /// every attempt fails.
    Transient(E),
    /// Retrying cannot help (bad input, parse failures, auth errors).
    /// Surfaced immediately without consuming further attempts.
    Permanent(E),
}

/// Exponential backoff schedule: `base * 2^(attempt-1)`, capped at `max_delay`.
#[derive(Debug, Clone)]
pub struct BackoffPolicy {
    /// Total attempts, including the first (so `5` = 1 try + up to 4 retries).
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for BackoffPolicy {
    /// The schedule `synth_gen` shipped with in Phase 2 (decision F):
    /// 1 s → 2 s → 4 s → 8 s → 16 s, five attempts total.
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
        }
    }
}

impl BackoffPolicy {
    /// A tighter schedule for expensive idempotent operations (whole-job
    /// retries like ingestion): one retry after a short pause. Re-running a
    /// ten-minute scrape five times helps nobody.
    #[must_use]
    pub fn single_retry() -> Self {
        Self {
            max_attempts: 2,
            base_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(2),
        }
    }

    /// Delay before `attempt` (1-based; attempt 0 is the initial try and has
    /// no delay).
    #[must_use]
    pub fn delay_for(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::ZERO;
        }
        let exp = self
            .base_delay
            .saturating_mul(1_u32 << (attempt - 1).min(16));
        exp.min(self.max_delay)
    }
}

/// Run `op` until it succeeds, fails permanently, or exhausts the policy.
///
/// `op` receives the 0-based attempt number (useful for logging / varying a
/// request id) and classifies its own failures via [`Retry`]. On exhaustion
/// the *last* transient error is returned.
pub async fn with_backoff<T, E, F, Fut>(
    policy: &BackoffPolicy,
    what: &str,
    mut op: F,
) -> Result<T, E>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<T, Retry<E>>>,
{
    let mut last_err: Option<E> = None;
    for attempt in 0..policy.max_attempts {
        let delay = policy.delay_for(attempt);
        if !delay.is_zero() {
            warn!(what, attempt, delay_secs = delay.as_secs(), "transient failure; backing off then retrying");
            tokio::time::sleep(delay).await;
        }
        match op(attempt).await {
            Ok(v) => return Ok(v),
            Err(Retry::Permanent(e)) => return Err(e),
            Err(Retry::Transient(e)) => last_err = Some(e),
        }
    }
    Err(last_err.expect("with_backoff ran at least one attempt"))
}

/// Every cross-boundary deadline in the orchestrator, with its rationale.
/// Tools take these instead of declaring local magic numbers so a future
/// "why is this 600?" has exactly one place to look — and one place to tune.
pub mod timeouts {
    use std::time::Duration;

    /// One-shot + pooled worker calls with no explicit override. Forgiving
    /// enough for a cold interpreter spawn on a warm cache; tight enough to
    /// reap a wedged worker quickly.
    pub const WORKER_DEFAULT: Duration = Duration::from_secs(20);

    /// `rag.query` retrieval. First pooled call pays the BGE-small cold load
    /// (~5-8 s); warm calls are 50-300 ms. 60 s covers the cold path with
    /// slack for an HF hub checksum on first-ever model download.
    pub const RAG_QUERY: Duration = Duration::from_secs(60);

    /// Non-streaming chat completion against the local llama.cpp server.
    /// 7B Q4_K_M at ~70 tok/s produces 1024 tokens in ~15 s; 180 s leaves
    /// room for a long prompt eval on CPU-only fallback machines.
    pub const LOCAL_CHAT_COMPLETION: Duration = Duration::from_secs(180);

    /// Whole-source ingestion (scrape + clean + chunk). Wikipedia category
    /// crawls of ~100 pages run 3-8 minutes on a residential connection.
    pub const INGEST: Duration = Duration::from_secs(600);

    /// Full-corpus embed + FTS index build. 376 chunks ≈ 45 s on the
    /// reference CPU; 900 s covers corpora an order of magnitude larger.
    pub const EMBED_BUILD: Duration = Duration::from_secs(900);

    /// `run_command` default when the agent doesn't specify one.
    pub const RUN_COMMAND_DEFAULT: Duration = Duration::from_secs(600);

    /// llama.cpp server: spawn → /v1/models responds. Model load from a warm
    /// page cache is ~10 s; cold NVMe read of a 4.4 GB GGUF can take ~60 s.
    pub const INFERENCE_STARTUP_HEALTH: Duration = Duration::from_secs(120);

    /// Single HTTP health probe against a running server.
    pub const INFERENCE_HEALTH_PROBE: Duration = Duration::from_secs(5);
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    fn instant_policy(max_attempts: u32) -> BackoffPolicy {
        BackoffPolicy {
            max_attempts,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(2),
        }
    }

    #[tokio::test]
    async fn succeeds_first_try_without_delay() {
        let calls = AtomicU32::new(0);
        let out: Result<u32, &str> = with_backoff(&instant_policy(5), "test", |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Ok(42) }
        })
        .await;
        assert_eq!(out.unwrap(), 42);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retries_transient_until_success() {
        let calls = AtomicU32::new(0);
        let out: Result<u32, &str> = with_backoff(&instant_policy(5), "test", |attempt| {
            calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if attempt < 2 {
                    Err(Retry::Transient("flaky"))
                } else {
                    Ok(7)
                }
            }
        })
        .await;
        assert_eq!(out.unwrap(), 7);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn permanent_failure_short_circuits() {
        let calls = AtomicU32::new(0);
        let out: Result<u32, &str> = with_backoff(&instant_policy(5), "test", |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(Retry::Permanent("bad input")) }
        })
        .await;
        assert_eq!(out.unwrap_err(), "bad input");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no retries on permanent");
    }

    #[tokio::test]
    async fn exhaustion_returns_last_transient_error() {
        let out: Result<u32, String> = with_backoff(&instant_policy(3), "test", |attempt| async move {
            Err(Retry::Transient(format!("fail #{attempt}")))
        })
        .await;
        assert_eq!(out.unwrap_err(), "fail #2", "last error wins");
    }

    #[test]
    fn delay_schedule_matches_phase2_decision() {
        let p = BackoffPolicy::default();
        assert_eq!(p.delay_for(0), Duration::ZERO);
        assert_eq!(p.delay_for(1), Duration::from_secs(1));
        assert_eq!(p.delay_for(2), Duration::from_secs(2));
        assert_eq!(p.delay_for(3), Duration::from_secs(4));
        assert_eq!(p.delay_for(4), Duration::from_secs(8));
        assert_eq!(p.delay_for(5), Duration::from_secs(16));
        assert_eq!(p.delay_for(6), Duration::from_secs(30), "capped at max_delay");
    }
}
