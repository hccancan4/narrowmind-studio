//! `NarrowMind Studio` agent loop.
//!
//! Hosts the `Provider` trait, tool dispatcher, and streaming response handler.
//! Phase 0 is a placeholder — the real implementation lands in Phase 1.

/// Crate version string for build-info reporting.
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
