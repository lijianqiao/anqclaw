//! Lightweight in-memory metrics — no external dependencies.
//!
//! Maintains atomic counters for key operational metrics.
//! Exposed via `GET /metrics` on the HTTP channel as JSON.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

/// Global application metrics (singleton, shared via `Arc`).
pub struct Metrics {
    pub total_requests: AtomicU64,
    pub active_sessions: AtomicU64,
    pub llm_calls: AtomicU64,
    pub tool_calls: AtomicU64,
    /// Cumulative response time in milliseconds (divide by llm_calls for avg).
    pub llm_total_ms: AtomicU64,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            total_requests: AtomicU64::new(0),
            active_sessions: AtomicU64::new(0),
            llm_calls: AtomicU64::new(0),
            tool_calls: AtomicU64::new(0),
            llm_total_ms: AtomicU64::new(0),
        }
    }

    /// Returns a JSON-serializable snapshot of current metrics.
    pub fn snapshot(&self) -> MetricsSnapshot {
        let llm_calls = self.llm_calls.load(Ordering::Relaxed);
        let llm_total_ms = self.llm_total_ms.load(Ordering::Relaxed);
        let avg_response_ms = if llm_calls > 0 {
            llm_total_ms / llm_calls
        } else {
            0
        };

        MetricsSnapshot {
            total_requests: self.total_requests.load(Ordering::Relaxed),
            active_sessions: self.active_sessions.load(Ordering::Relaxed),
            llm_calls,
            tool_calls: self.tool_calls.load(Ordering::Relaxed),
            avg_response_ms,
        }
    }
}

#[derive(Serialize)]
pub struct MetricsSnapshot {
    pub total_requests: u64,
    pub active_sessions: u64,
    pub llm_calls: u64,
    pub tool_calls: u64,
    pub avg_response_ms: u64,
}
