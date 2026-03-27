//! Feishu REST API wrapper.
//!
//! Handles:
//! - Tenant access token acquisition + caching (with proactive refresh)
//! - WebSocket endpoint URL retrieval
//! - Sending messages via Interactive Card (Markdown)
//!
//! Reference: zeroclaw/lark.rs

use anyhow::{Context, Result};
use secrecy::ExposeSecret;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};

use crate::config::FeishuSection;

use super::types::{
    CARD_MARKDOWN_MAX_BYTES, WsClientConfig, WsEndpointResp, build_card_content,
    split_markdown_chunks,
};

const FEISHU_API_BASE: &str = "https://open.feishu.cn/open-apis";
const FEISHU_WS_BASE: &str = "https://open.feishu.cn";

/// Refresh tenant token 120 seconds before the announced expiry.
const TOKEN_REFRESH_SKEW: Duration = Duration::from_secs(120);
/// Fallback TTL when `expire` field is absent.
const DEFAULT_TOKEN_TTL: Duration = Duration::from_secs(7200);

// ─── Cached Token ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct CachedToken {
    value: String,
    refresh_after: Instant,
}

// ─── FeishuApi ───────────────────────────────────────────────────────────────

pub struct FeishuApi {
    http: reqwest::Client,
    api_base: String,
    ws_base: String,
    app_id: String,
    app_secret: secrecy::SecretString,
    token: RwLock<Option<CachedToken>>,
    refresh_lock: Mutex<()>,
}

impl FeishuApi {
    pub fn new(config: &FeishuSection) -> Result<Self> {
        Self::new_with_bases(config, FEISHU_API_BASE, FEISHU_WS_BASE)
    }

    fn new_with_bases(config: &FeishuSection, api_base: &str, ws_base: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("build reqwest client")?;

        Ok(Self {
            http,
            api_base: api_base.to_string(),
            ws_base: ws_base.to_string(),
            app_id: config.app_id.clone(),
            app_secret: config.app_secret.clone(),
            token: RwLock::new(None),
            refresh_lock: Mutex::new(()),
        })
    }

    // ── Token Management ─────────────────────────────────────────────────────

    /// Gets a valid tenant access token, refreshing if needed.
    pub async fn get_tenant_access_token(&self) -> Result<String> {
        // Check cache
        {
            let cached = self.token.read().await;
            if let Some(ref t) = *cached
                && Instant::now() < t.refresh_after
            {
                return Ok(t.value.clone());
            }
        }

        let _refresh_guard = self.refresh_lock.lock().await;

        // Double-check after winning the refresh lock so concurrent callers share one refresh.
        {
            let cached = self.token.read().await;
            if let Some(ref t) = *cached
                && Instant::now() < t.refresh_after
            {
                return Ok(t.value.clone());
            }
        }

        // Refresh
        let url = format!("{}/auth/v3/tenant_access_token/internal", self.api_base);
        let body = serde_json::json!({
            "app_id": self.app_id,
            "app_secret": self.app_secret.expose_secret(),
        });

        let resp = self.http.post(&url).json(&body).send().await?;
        let status = resp.status();
        let data: serde_json::Value = resp.json().await?;

        if !status.is_success() {
            anyhow::bail!("tenant_access_token request failed: status={status}, body={data}");
        }

        let code = data.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
        if code != 0 {
            let msg = data
                .get("msg")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            anyhow::bail!("tenant_access_token failed: code={code}, msg={msg}");
        }

        let token_value = data
            .get("tenant_access_token")
            .and_then(|t| t.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing tenant_access_token in response"))?
            .to_string();

        let ttl_secs = data
            .get("expire")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TOKEN_TTL.as_secs())
            .max(1);

        let ttl = Duration::from_secs(ttl_secs);
        let refresh_in = ttl
            .checked_sub(TOKEN_REFRESH_SKEW)
            .unwrap_or(Duration::from_secs(1));

        let cached = CachedToken {
            value: token_value.clone(),
            refresh_after: Instant::now() + refresh_in,
        };

        {
            let mut guard = self.token.write().await;
            *guard = Some(cached);
        }

        Ok(token_value)
    }

    /// Invalidate cached token (called when API returns 401 / invalid token).
    pub async fn invalidate_token(&self) {
        let mut guard = self.token.write().await;
        *guard = None;
    }

    // ── WebSocket Endpoint ───────────────────────────────────────────────────

    /// Gets the WebSocket endpoint URL + client config.
    ///
    /// Uses AppID + AppSecret directly — does NOT need a tenant_access_token.
    pub async fn get_ws_endpoint(&self) -> Result<(String, WsClientConfig)> {
        let url = format!("{}/callback/ws/endpoint", self.ws_base);

        let resp = self
            .http
            .post(&url)
            .header("locale", "zh")
            .json(&serde_json::json!({
                "AppID": self.app_id,
                "AppSecret": self.app_secret.expose_secret(),
            }))
            .send()
            .await?
            .json::<WsEndpointResp>()
            .await
            .context("parse ws endpoint response")?;

        if resp.code != 0 {
            anyhow::bail!(
                "WS endpoint failed: code={}, msg={}",
                resp.code,
                resp.msg.as_deref().unwrap_or("(none)")
            );
        }

        let ep = resp
            .data
            .ok_or_else(|| anyhow::anyhow!("WS endpoint: empty data"))?;
        Ok((ep.url, ep.client_config.unwrap_or_default()))
    }

    // ── Send Messages ────────────────────────────────────────────────────────

    /// Send an Interactive Card (Markdown) to a chat.
    ///
    /// Long messages are automatically split into multiple cards.
    pub async fn send_card(&self, chat_id: &str, markdown: &str) -> Result<()> {
        let chunks = split_markdown_chunks(markdown, CARD_MARKDOWN_MAX_BYTES);
        for chunk in chunks {
            self.do_send_card(chat_id, chunk, None).await?;
        }
        Ok(())
    }

    /// Reply with an Interactive Card (Markdown) to a specific message.
    ///
    /// Long messages are automatically split — only the first chunk is a reply,
    /// subsequent chunks are sent as standalone messages.
    pub async fn reply_card(&self, chat_id: &str, message_id: &str, markdown: &str) -> Result<()> {
        let chunks = split_markdown_chunks(markdown, CARD_MARKDOWN_MAX_BYTES);
        for (i, chunk) in chunks.iter().enumerate() {
            if i == 0 {
                self.do_send_card(chat_id, chunk, Some(message_id)).await?;
            } else {
                self.do_send_card(chat_id, chunk, None).await?;
            }
        }
        Ok(())
    }

    /// Add an emoji reaction to a message.
    /// emoji_type examples: "OnIt", "THUMBSUP", "HEART", etc.
    pub async fn add_reaction(&self, message_id: &str, emoji_type: &str) -> Result<()> {
        let token = self.get_tenant_access_token().await?;
        let url = format!("{}/im/v1/messages/{message_id}/reactions", self.api_base);
        let body = serde_json::json!({
            "reaction_type": {
                "emoji_type": emoji_type
            }
        });

        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json; charset=utf-8")
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            tracing::warn!(status = %status, body = %body, "add_reaction failed (non-critical)");
        }

        Ok(())
    }

    /// Internal: send a single card, with optional reply-to.
    /// Includes 401/invalid-token retry logic.
    async fn do_send_card(
        &self,
        chat_id: &str,
        markdown: &str,
        reply_to: Option<&str>,
    ) -> Result<()> {
        let card_content = build_card_content(markdown);

        let mut token = self.get_tenant_access_token().await?;

        for attempt in 0..2u32 {
            let (url, body) = if let Some(msg_id) = reply_to {
                let url = format!("{}/im/v1/messages/{msg_id}/reply", self.api_base);
                let body = serde_json::json!({
                    "msg_type": "interactive",
                    "content": card_content,
                });
                (url, body)
            } else {
                let url = format!("{}/im/v1/messages?receive_id_type=chat_id", self.api_base);
                let body = serde_json::json!({
                    "receive_id": chat_id,
                    "msg_type": "interactive",
                    "content": card_content,
                });
                (url, body)
            };

            let resp = self
                .http
                .post(&url)
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", "application/json; charset=utf-8")
                .json(&body)
                .send()
                .await?;

            let status = resp.status();
            let resp_body: serde_json::Value = resp.json().await.unwrap_or_default();

            // Check for expired/invalid token → retry once
            let resp_code = resp_body.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
            if (status.as_u16() == 401 || resp_code == 99_991_663) && attempt == 0 {
                tracing::warn!("Feishu: token invalid, refreshing and retrying");
                self.invalidate_token().await;
                token = self.get_tenant_access_token().await?;
                continue;
            }

            if !status.is_success() {
                anyhow::bail!("Feishu send failed: status={status}, body={resp_body}");
            }

            if resp_code != 0 {
                let msg = resp_body
                    .get("msg")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error");
                anyhow::bail!("Feishu send failed: code={resp_code}, msg={msg}");
            }

            return Ok(());
        }

        anyhow::bail!("Feishu send failed after token refresh retry")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::State, http::{HeaderMap, StatusCode}, routing::post};
    use secrecy::SecretString;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::TcpListener;
    use tokio::sync::Barrier;

    #[tokio::test]
    async fn test_concurrent_token_refresh_is_singleflight() {
        let request_count = Arc::new(AtomicUsize::new(0));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let app = {
            let request_count = request_count.clone();
            Router::new().route(
                "/open-apis/auth/v3/tenant_access_token/internal",
                post(move || {
                    let request_count = request_count.clone();
                    async move {
                        request_count.fetch_add(1, Ordering::SeqCst);
                        Json(json!({
                            "code": 0,
                            "tenant_access_token": "token-123",
                            "expire": 7200
                        }))
                    }
                }),
            )
        };

        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let config = FeishuSection {
            app_id: "app-id".to_string(),
            app_secret: SecretString::new("app-secret".to_string().into()),
            allow_from: vec![],
        };
        let api = Arc::new(
            FeishuApi::new_with_bases(
                &config,
                &format!("http://127.0.0.1:{}/open-apis", addr.port()),
                &format!("http://127.0.0.1:{}", addr.port()),
            )
            .unwrap(),
        );

        let barrier = Arc::new(Barrier::new(8));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let api = api.clone();
            let barrier = barrier.clone();
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                api.get_tenant_access_token().await.unwrap()
            }));
        }

        for handle in handles {
            assert_eq!(handle.await.unwrap(), "token-123");
        }

        server.abort();
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_send_card_retries_after_401_and_refreshes_token() {
        #[derive(Clone)]
        struct FeishuTestState {
            token_requests: Arc<AtomicUsize>,
            send_requests: Arc<AtomicUsize>,
        }

        async fn token_handler(State(state): State<FeishuTestState>) -> Json<serde_json::Value> {
            let request_no = state.token_requests.fetch_add(1, Ordering::SeqCst);
            let token = if request_no == 0 { "token-old" } else { "token-new" };
            Json(json!({
                "code": 0,
                "tenant_access_token": token,
                "expire": 7200
            }))
        }

        async fn send_handler(
            State(state): State<FeishuTestState>,
            headers: HeaderMap,
        ) -> (StatusCode, Json<serde_json::Value>) {
            state.send_requests.fetch_add(1, Ordering::SeqCst);
            let auth = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();

            if auth == "Bearer token-old" {
                (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({ "code": 99_991_663, "msg": "invalid tenant access token" })),
                )
            } else {
                (StatusCode::OK, Json(json!({ "code": 0, "msg": "ok" })))
            }
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = FeishuTestState {
            token_requests: Arc::new(AtomicUsize::new(0)),
            send_requests: Arc::new(AtomicUsize::new(0)),
        };

        let app = Router::new()
            .route("/open-apis/auth/v3/tenant_access_token/internal", post(token_handler))
            .route("/open-apis/im/v1/messages", post(send_handler))
            .with_state(state.clone());

        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let config = FeishuSection {
            app_id: "app-id".to_string(),
            app_secret: SecretString::new("app-secret".to_string().into()),
            allow_from: vec![],
        };
        let api = FeishuApi::new_with_bases(
            &config,
            &format!("http://127.0.0.1:{}/open-apis", addr.port()),
            &format!("http://127.0.0.1:{}", addr.port()),
        )
        .unwrap();

        api.send_card("chat-1", "hello retry").await.unwrap();

        server.abort();
        assert_eq!(state.token_requests.load(Ordering::SeqCst), 2);
        assert_eq!(state.send_requests.load(Ordering::SeqCst), 2);
    }
}
