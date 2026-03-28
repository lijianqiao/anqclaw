//! HTTP channel — lightweight REST API for `POST /v1/chat`.
//!
//! Enables integration with curl, Web UIs, and other tools that speak HTTP.
//! Authentication via optional Bearer token configured in `[channel.http]`.
//!
//! Design: synchronous request/response — each POST blocks until the agent
//! replies (or timeout).  A oneshot channel bridges the async gap between
//! the axum handler and the Gateway send-back path.

use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use axum::{
    Json, Router,
    extract::{Multipart, State},
    http::{HeaderMap, StatusCode},
    response::sse::{Event, Sse},
    routing::{get, post},
};
use dashmap::DashMap;
use futures_util::StreamExt;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;

use crate::agent::AgentCore;
use crate::config::{AppConfig, HttpChannelSection};
use crate::memory::MemoryStore;
use crate::metrics::Metrics;
use crate::types::{ImageData, InboundMessage, MessageContent, OutboundMessage};

// ─── HTTP channel ────────────────────────────────────────────────────────────

pub struct HttpChannel {
    config: HttpChannelSection,
    /// Pending responses: message_id → oneshot sender.
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<OutboundMessage>>>>,
    /// For streaming endpoint: direct agent access.
    agent: Option<Arc<AgentCore>>,
    memory: Option<Arc<MemoryStore>>,
    app_config: Option<Arc<AppConfig>>,
    metrics: Option<Arc<Metrics>>,
}

impl HttpChannel {
    pub fn new(
        config: &HttpChannelSection,
        agent: Option<Arc<AgentCore>>,
        memory: Option<Arc<MemoryStore>>,
        app_config: Option<Arc<AppConfig>>,
        metrics: Option<Arc<Metrics>>,
    ) -> Self {
        Self {
            config: config.clone(),
            pending: Arc::new(Mutex::new(HashMap::new())),
            agent,
            memory,
            app_config,
            metrics,
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
                bearer_token: {
                    let token = self.config.bearer_token.expose_secret();
                    if token.is_empty() {
                        None
                    } else {
                        Some(token.to_string())
                    }
                },
                rate_limiter: Arc::new(DashMap::new()),
                agent: self.agent.clone(),
                memory: self.memory.clone(),
                app_config: self.app_config.clone(),
                metrics: self.metrics.clone(),
            };

            let app = Router::new()
                .route("/v1/chat", post(handle_chat))
                .route("/v1/chat/stream", post(handle_chat_stream))
                .route("/v1/chat/multipart", post(handle_chat_multipart))
                .route("/health", get(handle_health))
                .route("/metrics", get(handle_metrics))
                .with_state(Arc::new(state));

            let listener = tokio::net::TcpListener::bind(&self.config.bind).await?;
            tracing::info!(bind = %self.config.bind, "http channel listening / HTTP 频道正在监听");

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
                tracing::warn!("http channel: send_message with no reply_to, dropping / HTTP 频道: send_message 无 reply_to，已丢弃");
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
                    "http channel: no pending request for reply_to, dropping / HTTP 频道: 无对应 reply_to 的待处理请求，已丢弃"
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
    /// Per-sender_id sliding-window rate limiter (60s window, max 30 req).
    rate_limiter: Arc<DashMap<String, Vec<Instant>>>,
    /// For streaming endpoint.
    agent: Option<Arc<AgentCore>>,
    memory: Option<Arc<MemoryStore>>,
    app_config: Option<Arc<AppConfig>>,
    metrics: Option<Arc<Metrics>>,
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

/// RAII guard that removes the pending entry on drop, preventing leaks when
/// the client disconnects or the handler future is cancelled.
struct PendingGuard {
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<OutboundMessage>>>>,
    message_id: String,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        let pending = self.pending.clone();
        let id = self.message_id.clone();
        // Best-effort cleanup — if lock is contended during shutdown, skip.
        tokio::spawn(async move {
            let mut map = pending.lock().await;
            map.remove(&id);
        });
    }
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

    // Per-sender rate limiting (30 req/min sliding window)
    {
        let sender_key = if req.sender_id.is_empty() {
            "http_user"
        } else {
            &req.sender_id
        };
        let now = Instant::now();
        let window = std::time::Duration::from_secs(60);
        let mut entry = state
            .rate_limiter
            .entry(sender_key.to_string())
            .or_default();
        entry.retain(|t| now.duration_since(*t) < window);
        if entry.len() >= 30 {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                Json(ErrorResponse {
                    error: "rate limit exceeded, please try again later".into(),
                }),
            ));
        }
        entry.push(now);
        // GC: remove empty entries to prevent unbounded DashMap growth
        drop(entry);
        state.rate_limiter.retain(|_, v| !v.is_empty());
    }

    let chat_id = if req.chat_id.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        req.chat_id
    };

    let message_id = uuid::Uuid::new_v4().to_string();

    // Register pending oneshot with a drop guard for cleanup on cancellation
    let (resp_tx, resp_rx) = oneshot::channel();
    {
        let mut map = state.pending.lock().await;
        map.insert(message_id.clone(), resp_tx);
    }
    let _pending_guard = PendingGuard {
        pending: state.pending.clone(),
        message_id: message_id.clone(),
    };

    // Build inbound message
    let inbound = InboundMessage {
        channel: "http".into(),
        chat_id: chat_id.clone(),
        sender_id: req.sender_id,
        message_id: message_id.clone(),
        content: MessageContent::Text(req.message),
        timestamp: chrono::Utc::now().timestamp(),
        trace_id: String::new(),
        images: vec![],
    };

    // Send to gateway
    if state.tx.send(inbound).await.is_err() {
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
        Err(_) => Err((
            StatusCode::GATEWAY_TIMEOUT,
            Json(ErrorResponse {
                error: "agent reply timed out".into(),
            }),
        )),
    }
}

async fn handle_health() -> &'static str {
    "ok"
}

async fn handle_metrics(
    State(state): State<Arc<HttpState>>,
) -> Result<Json<crate::metrics::MetricsSnapshot>, StatusCode> {
    match &state.metrics {
        Some(m) => Ok(Json(m.snapshot())),
        None => Err(StatusCode::NOT_IMPLEMENTED),
    }
}

// ─── SSE Streaming handler ──────────────────────────────────────────────────

async fn handle_chat_stream(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Json(req): Json<ChatRequest>,
) -> Result<
    Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>,
    (StatusCode, Json<ErrorResponse>),
> {
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

    if req.message.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "message must not be empty".into(),
            }),
        ));
    }

    // Rate limit
    {
        let sender_key = if req.sender_id.is_empty() {
            "http_user"
        } else {
            &req.sender_id
        };
        let now = Instant::now();
        let window = std::time::Duration::from_secs(60);
        let mut entry = state
            .rate_limiter
            .entry(sender_key.to_string())
            .or_default();
        entry.retain(|t| now.duration_since(*t) < window);
        if entry.len() >= 30 {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                Json(ErrorResponse {
                    error: "rate limit exceeded, please try again later".into(),
                }),
            ));
        }
        entry.push(now);
        // GC: remove empty entries to prevent unbounded DashMap growth
        drop(entry);
        state.rate_limiter.retain(|_, v| !v.is_empty());
    }

    let (agent, memory, app_config) = match (&state.agent, &state.memory, &state.app_config) {
        (Some(a), Some(m), Some(c)) => (a.clone(), m.clone(), c.clone()),
        _ => {
            return Err((
                StatusCode::NOT_IMPLEMENTED,
                Json(ErrorResponse {
                    error: "streaming not configured".into(),
                }),
            ));
        }
    };

    let chat_id = if req.chat_id.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        req.chat_id
    };

    let inbound = InboundMessage {
        channel: "http".into(),
        chat_id: chat_id.clone(),
        sender_id: req.sender_id,
        message_id: uuid::Uuid::new_v4().to_string(),
        content: MessageContent::Text(req.message),
        timestamp: chrono::Utc::now().timestamp(),
        trace_id: String::new(),
        images: vec![],
    };

    // Load history
    let history = memory
        .get_history(&chat_id, app_config.memory.history_limit as usize)
        .await
        .unwrap_or_default();

    // Create streaming channel
    let (delta_tx, delta_rx) = mpsc::channel::<String>(32);

    // Spawn agent task
    let memory_clone = memory.clone();
    let chat_id_clone = chat_id.clone();
    tokio::spawn(async move {
        let (_, persist_messages) = agent.handle_streaming(&inbound, &history, delta_tx).await;
        if !persist_messages.is_empty()
            && let Err(e) = memory_clone
                .save_conversation(&chat_id_clone, &persist_messages)
                .await
        {
            tracing::error!(error = %e, "failed to save streaming conversation / 保存流式对话失败");
        }
    });

    // Convert to SSE stream
    let stream =
        ReceiverStream::new(delta_rx).map(|text| Ok::<_, Infallible>(Event::default().data(text)));

    Ok(Sse::new(stream))
}

// ─── Multipart handler (image + file upload) ────────────────────────────────

/// Accepted image MIME types for vision support.
const ALLOWED_IMAGE_TYPES: &[&str] = &["image/jpeg", "image/png", "image/webp", "image/gif"];

/// Max image size: 10 MB (base64 will be ~33% larger).
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// Max file size: 5 MB.
const MAX_FILE_BYTES: usize = 5 * 1024 * 1024;

/// Handles `POST /v1/chat/multipart` with multipart/form-data supporting image/file upload.
///
/// Fields:
/// - `message` (text, required): The user's message text.
/// - `chat_id` (text, optional): Session ID.
/// - `sender_id` (text, optional): Sender identifier.
/// - `image` (file, optional): Image attachment (jpeg/png/webp/gif, max 10MB).
/// - `file` (file, optional): File attachment (max 5MB, text files are extracted).
async fn handle_chat_multipart(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    mut multipart: Multipart,
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

    let mut message = String::new();
    let mut chat_id = String::new();
    let mut sender_id = String::new();
    let mut image_data: Option<ImageData> = None;
    let mut file_content: Option<(String, Vec<u8>)> = None; // (filename, bytes)

    while let Ok(Some(field)) = multipart.next_field().await {
        let field_name: String = field.name().unwrap_or("").to_string();
        let content_type_str: String = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();
        let file_name_str: String = field.file_name().unwrap_or("").to_string();

        // Read all fields as bytes first to avoid type inference issues
        let raw_bytes: Vec<u8> = match field.bytes().await {
            Ok(b) => b.to_vec(),
            Err(_) => continue,
        };

        match field_name.as_str() {
            "message" => {
                message = String::from_utf8_lossy(&raw_bytes).to_string();
            }
            "chat_id" => {
                chat_id = String::from_utf8_lossy(&raw_bytes).to_string();
            }
            "sender_id" => {
                sender_id = String::from_utf8_lossy(&raw_bytes).to_string();
            }
            "image" => {
                if !ALLOWED_IMAGE_TYPES.contains(&content_type_str.as_str()) {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        Json(ErrorResponse {
                            error: format!(
                                "unsupported image type: {content_type_str}. Allowed: {}",
                                ALLOWED_IMAGE_TYPES.join(", ")
                            ),
                        }),
                    ));
                }

                if raw_bytes.len() > MAX_IMAGE_BYTES {
                    return Err((
                        StatusCode::PAYLOAD_TOO_LARGE,
                        Json(ErrorResponse {
                            error: format!(
                                "image too large ({} bytes), max {} bytes",
                                raw_bytes.len(),
                                MAX_IMAGE_BYTES
                            ),
                        }),
                    ));
                }

                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&raw_bytes);
                image_data = Some(ImageData {
                    media_type: content_type_str,
                    data: b64,
                });
            }
            "file" => {
                let filename = if file_name_str.is_empty() {
                    "unknown".to_string()
                } else {
                    file_name_str
                };

                if raw_bytes.len() > MAX_FILE_BYTES {
                    return Err((
                        StatusCode::PAYLOAD_TOO_LARGE,
                        Json(ErrorResponse {
                            error: format!(
                                "file too large ({} bytes), max {} bytes",
                                raw_bytes.len(),
                                MAX_FILE_BYTES
                            ),
                        }),
                    ));
                }

                file_content = Some((filename, raw_bytes.to_vec()));
            }
            _ => {
                // Ignore unknown fields
            }
        }
    }

    if message.trim().is_empty() && image_data.is_none() && file_content.is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "message must not be empty (or provide an image/file)".into(),
            }),
        ));
    }

    // Rate limiting
    {
        let sender_key = if sender_id.is_empty() {
            "http_user"
        } else {
            &sender_id
        };
        let now = Instant::now();
        let window = std::time::Duration::from_secs(60);
        let mut entry = state
            .rate_limiter
            .entry(sender_key.to_string())
            .or_default();
        entry.retain(|t| now.duration_since(*t) < window);
        if entry.len() >= 30 {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                Json(ErrorResponse {
                    error: "rate limit exceeded, please try again later".into(),
                }),
            ));
        }
        entry.push(now);
    }

    if chat_id.is_empty() {
        chat_id = uuid::Uuid::new_v4().to_string();
    }
    if sender_id.is_empty() {
        sender_id = "http_user".to_string();
    }

    // Handle file: extract text content from text-like files
    if let Some((ref filename, ref bytes)) = file_content {
        let text_extensions = [
            "txt", "md", "rs", "py", "js", "ts", "json", "toml", "yaml", "yml", "xml", "html",
            "css", "csv", "log", "sh", "bat", "sql", "go", "java", "c", "cpp", "h", "hpp",
        ];
        let ext = std::path::Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        if text_extensions.contains(&ext.as_str()) {
            if let Ok(text) = String::from_utf8(bytes.clone()) {
                // Inject file content into message
                let file_header =
                    format!("\n\n--- 文件: {} ---\n{}\n--- 文件结束 ---", filename, text);
                message.push_str(&file_header);
            } else {
                message.push_str(&format!("\n\n[文件 {} 不是有效的 UTF-8 文本]", filename));
            }
        } else {
            message.push_str(&format!("\n\n[不支持的文件类型: {}]", filename));
        }
    }

    let message_id = uuid::Uuid::new_v4().to_string();

    // Build content: always use Text so agent gets the message via to_text()
    // Images are carried in InboundMessage.images instead.
    let content = MessageContent::Text(if message.is_empty() && image_data.is_some() {
        "请描述这张图片".to_string()
    } else {
        message.clone()
    });

    // Register pending oneshot
    let (resp_tx, resp_rx) = oneshot::channel();
    {
        let mut map = state.pending.lock().await;
        map.insert(message_id.clone(), resp_tx);
    }
    let _pending_guard = PendingGuard {
        pending: state.pending.clone(),
        message_id: message_id.clone(),
    };

    let images = match image_data {
        Some(img) => vec![img],
        None => vec![],
    };

    let inbound = InboundMessage {
        channel: "http".into(),
        chat_id: chat_id.clone(),
        sender_id,
        message_id: message_id.clone(),
        content,
        timestamp: chrono::Utc::now().timestamp(),
        trace_id: String::new(),
        images,
    };

    // Send to gateway
    if state.tx.send(inbound).await.is_err() {
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
        Err(_) => Err((
            StatusCode::GATEWAY_TIMEOUT,
            Json(ErrorResponse {
                error: "agent reply timed out".into(),
            }),
        )),
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn test_state(bearer_token: Option<&str>) -> Arc<HttpState> {
        let (tx, _rx) = mpsc::channel(16);
        Arc::new(HttpState {
            tx,
            pending: Arc::new(Mutex::new(HashMap::new())),
            bearer_token: bearer_token.map(|s| s.to_string()),
            rate_limiter: Arc::new(DashMap::new()),
            agent: None,
            memory: None,
            app_config: None,
            metrics: None,
        })
    }

    fn test_router(state: Arc<HttpState>) -> Router {
        Router::new()
            .route("/v1/chat", post(handle_chat))
            .route("/health", get(handle_health))
            .route("/metrics", get(handle_metrics))
            .with_state(state)
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let app = test_router(test_state(None));
        let req = Request::get("/health").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"ok");
    }

    #[tokio::test]
    async fn test_chat_requires_auth() {
        let app = test_router(test_state(Some("secret123")));
        let req = Request::post("/v1/chat")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"message":"hello"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_chat_wrong_token() {
        let app = test_router(test_state(Some("secret123")));
        let req = Request::post("/v1/chat")
            .header("content-type", "application/json")
            .header("authorization", "Bearer wrong_token")
            .body(Body::from(r#"{"message":"hello"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_chat_empty_message() {
        let app = test_router(test_state(None));
        let req = Request::post("/v1/chat")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"message":""}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_chat_whitespace_only_message() {
        let app = test_router(test_state(None));
        let req = Request::post("/v1/chat")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"message":"   "}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_chat_valid_auth_passes() {
        // Keep receiver alive so tx.send() succeeds and handler blocks on reply
        let (tx, _rx) = mpsc::channel(16);
        let state = Arc::new(HttpState {
            tx,
            pending: Arc::new(Mutex::new(HashMap::new())),
            bearer_token: Some("secret123".to_string()),
            rate_limiter: Arc::new(DashMap::new()),
            agent: None,
            memory: None,
            app_config: None,
            metrics: None,
        });
        let app = test_router(state);
        let req = Request::post("/v1/chat")
            .header("content-type", "application/json")
            .header("authorization", "Bearer secret123")
            .body(Body::from(r#"{"message":"hello"}"#))
            .unwrap();

        // Will timeout waiting for agent reply — that's expected (means auth passed)
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(100), app.oneshot(req)).await;
        assert!(result.is_err(), "expected timeout (no agent configured)");
    }

    #[tokio::test]
    async fn test_metrics_not_configured() {
        let app = test_router(test_state(None));
        let req = Request::get("/metrics").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn test_rate_limiting() {
        let state = test_state(None);
        {
            let now = Instant::now();
            let timestamps: Vec<Instant> = (0..30).map(|_| now).collect();
            state
                .rate_limiter
                .insert("http_user".to_string(), timestamps);
        }

        let app = test_router(state);
        let req = Request::post("/v1/chat")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"message":"hello","sender_id":"http_user"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    }
}
