//! HTTP channel — lightweight REST API for `POST /v1/chat`.
//!
//! Enables integration with curl, Web UIs, and other tools that speak HTTP.
//! Authentication via optional Bearer token configured in `[channel.http]`.
//!
//! Design: synchronous request/response — each POST blocks until the agent
//! replies (or timeout).  A oneshot channel bridges the async gap between
//! the axum handler and the Gateway send-back path.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::config::HttpChannelSection;
use crate::types::{InboundMessage, MessageContent, OutboundMessage};

// ─── HTTP channel ────────────────────────────────────────────────────────────

pub struct HttpChannel {
    config: HttpChannelSection,
    /// Pending responses: message_id → oneshot sender.
    /// When the Gateway sends a reply via `send_message`, we look up the
    /// corresponding oneshot and deliver the response.
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<OutboundMessage>>>>,
}

impl HttpChannel {
    pub fn new(config: &HttpChannelSection) -> Self {
        Self {
            config: config.clone(),
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl super::Channel for HttpChannel {
    fn start(
        &self,
        tx: mpsc::Sender<InboundMessage>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let state = HttpState {
                tx,
                pending: self.pending.clone(),
                bearer_token: if self.config.bearer_token.is_empty() {
                    None
                } else {
                    Some(self.config.bearer_token.clone())
                },
            };

            let app = Router::new()
                .route("/v1/chat", post(handle_chat))
                .route("/health", get(handle_health))
                .with_state(Arc::new(state));

            let listener = tokio::net::TcpListener::bind(&self.config.bind).await?;
            tracing::info!(bind = %self.config.bind, "http channel listening");

            axum::serve(listener, app).await?;
            Ok(())
        })
    }

    fn send_message(
        &self,
        msg: OutboundMessage,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        let pending = self.pending.clone();
        Box::pin(async move {
            let reply_to = msg.reply_to.clone().unwrap_or_default();
            if reply_to.is_empty() {
                tracing::warn!("http channel: send_message with no reply_to, dropping");
                return Ok(());
            }
            let sender = {
                let mut map = pending.lock().await;
                map.remove(&reply_to)
            };
            if let Some(tx) = sender {
                let _ = tx.send(msg);
            } else {
                tracing::warn!(
                    reply_to = %reply_to,
                    "http channel: no pending request for reply_to, dropping"
                );
            }
            Ok(())
        })
    }

    fn name(&self) -> &str {
        "http"
    }
}

// ─── Axum state & handlers ──────────────────────────────────────────────────

#[derive(Clone)]
struct HttpState {
    tx: mpsc::Sender<InboundMessage>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<OutboundMessage>>>>,
    bearer_token: Option<String>,
}

/// Request body for `POST /v1/chat`.
#[derive(Deserialize)]
struct ChatRequest {
    /// Message text.
    message: String,
    /// Optional chat ID for session continuity. Defaults to a random UUID.
    #[serde(default)]
    chat_id: String,
    /// Optional sender identifier.
    #[serde(default = "default_sender")]
    sender_id: String,
}

fn default_sender() -> String {
    "http_user".to_string()
}

/// Response body for `POST /v1/chat`.
#[derive(Serialize)]
struct ChatResponse {
    reply: String,
    chat_id: String,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

async fn handle_chat(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Json(req): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Auth check
    if let Some(ref expected) = state.bearer_token {
        let provided = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        if provided != expected.as_str() {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse {
                    error: "invalid or missing Bearer token".into(),
                }),
            ));
        }
    }

    // Validate
    if req.message.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "message must not be empty".into(),
            }),
        ));
    }

    let chat_id = if req.chat_id.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        req.chat_id
    };

    let message_id = uuid::Uuid::new_v4().to_string();

    // Register pending oneshot
    let (resp_tx, resp_rx) = oneshot::channel();
    {
        let mut map = state.pending.lock().await;
        map.insert(message_id.clone(), resp_tx);
    }

    // Build inbound message
    let inbound = InboundMessage {
        channel: "http".into(),
        chat_id: chat_id.clone(),
        sender_id: req.sender_id,
        message_id: message_id.clone(),
        content: MessageContent::Text(req.message),
        timestamp: chrono::Utc::now().timestamp(),
    };

    // Send to gateway
    if state.tx.send(inbound).await.is_err() {
        // Clean up pending
        let mut map = state.pending.lock().await;
        map.remove(&message_id);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "gateway unavailable".into(),
            }),
        ));
    }

    // Wait for reply (timeout: 5 minutes)
    match tokio::time::timeout(std::time::Duration::from_secs(300), resp_rx).await {
        Ok(Ok(reply)) => Ok(Json(ChatResponse {
            reply: reply.content,
            chat_id,
        })),
        Ok(Err(_)) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "reply channel closed".into(),
            }),
        )),
        Err(_) => {
            // Clean up pending on timeout
            let mut map = state.pending.lock().await;
            map.remove(&message_id);
            Err((
                StatusCode::GATEWAY_TIMEOUT,
                Json(ErrorResponse {
                    error: "agent reply timed out".into(),
                }),
            ))
        }
    }
}

async fn handle_health() -> &'static str {
    "ok"
}
