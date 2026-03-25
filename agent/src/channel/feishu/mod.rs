//! Feishu (飞书) channel implementation.
//!
//! Uses:
//! - WebSocket long-connection with Protobuf binary frames (pbbp2 protocol)
//! - REST API for sending Interactive Card (Markdown) messages
//! - Token caching with proactive refresh
//! - Auto-reconnect with exponential backoff
//!
//! TODO(future): When splitting into workspace crates, move to `crates/channel-feishu/`.

pub mod api;
pub mod types;
pub mod ws;

use anyhow::Result;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::config::FeishuSection;
use crate::types::{InboundMessage, OutboundMessage};

use super::Channel;

// ─── FeishuChannel ───────────────────────────────────────────────────────────

pub struct FeishuChannel {
    api: Arc<api::FeishuApi>,
    allow_from: Vec<String>,
}

impl FeishuChannel {
    pub fn new(config: &FeishuSection) -> Self {
        Self {
            api: Arc::new(api::FeishuApi::new(config)),
            allow_from: config.allow_from.clone(),
        }
    }
}

impl Channel for FeishuChannel {
    fn start(
        &self,
        tx: mpsc::Sender<InboundMessage>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            ws::run_with_reconnect(&self.api, tx, &self.allow_from).await
        })
    }

    fn send_message(
        &self,
        msg: OutboundMessage,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            if let Some(ref reply_to) = msg.reply_to {
                self.api
                    .reply_card(&msg.chat_id, reply_to, &msg.content)
                    .await
            } else {
                self.api.send_card(&msg.chat_id, &msg.content).await
            }
        })
    }

    fn name(&self) -> &str {
        "feishu"
    }
}
