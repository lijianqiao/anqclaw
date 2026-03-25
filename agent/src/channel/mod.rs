//! Channel abstraction and implementations.
//!
//! TODO(future): When splitting into workspace crates, extract the `Channel` trait
//! into `crates/channel/` and move each implementation (feishu, etc.) into its own crate.

pub mod cli;
pub mod feishu;

use anyhow::Result;
use std::future::Future;
use std::pin::Pin;
use tokio::sync::mpsc;

use crate::types::{InboundMessage, OutboundMessage};

// ─── Channel Trait ───────────────────────────────────────────────────────────

/// Unified interface for messaging platforms.
///
/// Object-safe: uses `Pin<Box<dyn Future>>`.
pub trait Channel: Send + Sync + 'static {
    /// Start listening for incoming messages. Sends received messages via `tx`.
    /// This is a long-running function (blocks until connection drops / shutdown).
    fn start(
        &self,
        tx: mpsc::Sender<InboundMessage>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;

    /// Send a message through this channel.
    fn send_message(
        &self,
        msg: OutboundMessage,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;

    /// Human-readable channel name (e.g. "feishu").
    fn name(&self) -> &str;
}
