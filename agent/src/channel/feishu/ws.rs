//! Feishu WebSocket connection manager.
//!
//! Implements the Feishu pbbp2 binary frame protocol:
//! - Protobuf-encoded PbFrame for ping/pong/data
//! - ACK within 3 seconds
//! - Fragment reassembly for large messages
//! - Message dedup (30-minute window)
//! - Auto-reconnect with exponential backoff
//!
//! Reference: zeroclaw/lark.rs `listen_ws`

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use prost::Message as ProstMessage;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMsg;

use crate::types::InboundMessage;

use super::api::FeishuApi;
use super::types::{LarkEvent, MsgReceivePayload, PbFrame, PbHeader, WsClientConfig};

/// Heartbeat timeout — if no binary frame in this window, reconnect.
const WS_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(300);

/// Maximum reconnect backoff.
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(60);

/// Fragment cache entry: (slots, created_at)
type FragEntry = (Vec<Option<Vec<u8>>>, Instant);

// ─── Public API ──────────────────────────────────────────────────────────────

/// Runs the WebSocket event loop with automatic reconnection.
///
/// This is the main entry point — it never returns under normal operation.
pub async fn run_with_reconnect(
    api: &FeishuApi,
    tx: mpsc::Sender<InboundMessage>,
    allow_from: &[String],
) -> Result<()> {
    let mut attempt = 0u32;
    // Shared dedup map survives reconnects
    let mut seen_ids: HashMap<String, Instant> = HashMap::new();

    loop {
        match listen_ws(api, &tx, allow_from, &mut seen_ids).await {
            Ok(()) => {
                tracing::info!("Feishu WS: connection closed, reconnecting");
                attempt = 0;
            }
            Err(e) => {
                attempt = attempt.saturating_add(1);
                let delay = reconnect_delay(attempt);
                tracing::warn!(
                    error = %e,
                    attempt,
                    delay_secs = delay.as_secs(),
                    "Feishu WS: error, reconnecting after backoff"
                );
                tokio::time::sleep(delay).await;
            }
        }
    }
}

fn reconnect_delay(attempt: u32) -> Duration {
    let secs = (1u64 << attempt.min(6)).min(MAX_RECONNECT_DELAY.as_secs());
    Duration::from_secs(secs)
}

// ─── WebSocket Event Loop ────────────────────────────────────────────────────

async fn listen_ws(
    api: &FeishuApi,
    tx: &mpsc::Sender<InboundMessage>,
    allow_from: &[String],
    seen_ids: &mut HashMap<String, Instant>,
) -> Result<()> {
    let (wss_url, client_config) = api.get_ws_endpoint().await?;

    // Extract service_id from URL query params
    let service_id = extract_service_id(&wss_url);

    tracing::info!("Feishu WS: connecting to {wss_url}");
    let (ws_stream, _) = tokio_tungstenite::connect_async(&wss_url).await?;
    let (mut write, mut read) = ws_stream.split();
    tracing::info!("Feishu WS: connected (service_id={service_id})");

    // Ping interval from server config (default 120s, min 10s)
    let mut ping_secs = client_config.ping_interval.unwrap_or(120).max(10);
    let mut hb_interval = tokio::time::interval(Duration::from_secs(ping_secs));
    let mut timeout_check = tokio::time::interval(Duration::from_secs(10));
    hb_interval.tick().await; // consume immediate tick

    let mut seq: u64 = 0;
    let mut last_recv = Instant::now();

    // Send initial ping immediately (like the official SDK)
    seq = seq.wrapping_add(1);
    let initial_ping = make_ping_frame(seq, service_id);
    write
        .send(WsMsg::Binary(initial_ping.encode_to_vec().into()))
        .await?;

    // Fragment reassembly cache
    let mut frag_cache: HashMap<String, FragEntry> = HashMap::new();

    loop {
        tokio::select! {
            biased;

            // Periodic ping
            _ = hb_interval.tick() => {
                seq = seq.wrapping_add(1);
                let ping = make_ping_frame(seq, service_id);
                if write.send(WsMsg::Binary(ping.encode_to_vec().into())).await.is_err() {
                    tracing::warn!("Feishu WS: ping failed, reconnecting");
                    break;
                }
                // GC stale fragments > 5 min
                let cutoff = Instant::now() - Duration::from_secs(300);
                frag_cache.retain(|_, (_, ts)| *ts > cutoff);
            }

            // Heartbeat timeout check
            _ = timeout_check.tick() => {
                if last_recv.elapsed() > WS_HEARTBEAT_TIMEOUT {
                    tracing::warn!("Feishu WS: heartbeat timeout, reconnecting");
                    break;
                }
            }

            // Incoming WS message
            msg = read.next() => {
                let raw = match msg {
                    Some(Ok(ws_msg)) => {
                        if should_refresh_last_recv(&ws_msg) {
                            last_recv = Instant::now();
                        }
                        match ws_msg {
                            WsMsg::Binary(b) => b,
                            WsMsg::Ping(d) => {
                                let _ = write.send(WsMsg::Pong(d)).await;
                                continue;
                            }
                            WsMsg::Close(_) => {
                                tracing::info!("Feishu WS: server closed connection");
                                break;
                            }
                            _ => continue,
                        }
                    }
                    None => {
                        tracing::info!("Feishu WS: stream ended");
                        break;
                    }
                    Some(Err(e)) => {
                        tracing::error!(error = %e, "Feishu WS: read error");
                        break;
                    }
                };

                // Decode protobuf frame
                let frame = match PbFrame::decode(&raw[..]) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::error!(error = %e, "Feishu WS: protobuf decode failed");
                        continue;
                    }
                };

                // CONTROL frame (ping/pong)
                if frame.method == 0 {
                    handle_control_frame(&frame, &client_config, &mut ping_secs, &mut hb_interval);
                    continue;
                }

                // DATA frame — ACK immediately (within 3 seconds!)
                {
                    let ack = make_ack_frame(&frame);
                    let _ = write.send(WsMsg::Binary(ack.encode_to_vec().into())).await;
                }

                // Fragment reassembly
                let msg_id = frame.header_value("message_id").to_string();
                let sum = frame.header_value("sum").parse::<usize>().unwrap_or(1).max(1);
                let seq_num = frame.header_value("seq").parse::<usize>().unwrap_or(0);

                let payload = if sum == 1 || msg_id.is_empty() || seq_num >= sum {
                    frame.payload.clone().unwrap_or_default()
                } else {
                    let entry = frag_cache
                        .entry(msg_id.clone())
                        .or_insert_with(|| (vec![None; sum], Instant::now()));
                    if entry.0.len() != sum {
                        *entry = (vec![None; sum], Instant::now());
                    }
                    entry.0[seq_num] = frame.payload.clone();
                    if entry.0.iter().all(|s| s.is_some()) {
                        let full: Vec<u8> = entry.0.iter()
                            .flat_map(|s| s.as_deref().unwrap_or(&[]))
                            .copied()
                            .collect();
                        frag_cache.remove(&msg_id);
                        full
                    } else {
                        continue; // waiting for more fragments
                    }
                };

                // Only process "event" type frames
                let msg_type = frame.header_value("type");
                if msg_type != "event" {
                    continue;
                }

                // Parse event
                let event: LarkEvent = match serde_json::from_slice(&payload) {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::error!(error = %e, "Feishu WS: event JSON parse failed");
                        continue;
                    }
                };

                if event.header.event_type != "im.message.receive_v1" {
                    continue;
                }

                let recv: MsgReceivePayload = match serde_json::from_value(event.event) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::error!(error = %e, "Feishu WS: message payload parse failed");
                        continue;
                    }
                };

                // Skip bot/app messages
                if recv.sender.sender_type == "app" || recv.sender.sender_type == "bot" {
                    continue;
                }

                // Allowlist check
                let sender_id = recv.sender.sender_id.open_id.as_deref().unwrap_or("");
                if !is_user_allowed(allow_from, sender_id) {
                    tracing::warn!(sender = sender_id, "Feishu WS: ignoring (not in allow_from)");
                    continue;
                }

                // Dedup
                let lark_msg_id = recv.message.message_id.clone();
                {
                    let now = Instant::now();
                    // GC old entries
                    seen_ids.retain(|_, t| now.duration_since(*t) < Duration::from_secs(30 * 60));
                    if seen_ids.contains_key(&lark_msg_id) {
                        tracing::debug!(msg_id = %lark_msg_id, "Feishu WS: duplicate, skipping");
                        continue;
                    }
                    seen_ids.insert(lark_msg_id.clone(), now);
                }

                // Convert to InboundMessage
                let inbound = match recv.into_inbound() {
                    Some(msg) => msg,
                    None => continue,
                };

                tracing::debug!(
                    chat_id = %inbound.chat_id,
                    msg_id = %lark_msg_id,
                    "Feishu WS: received message"
                );

                if tx.send(inbound).await.is_err() {
                    tracing::error!("Feishu WS: tx channel closed");
                    break;
                }
            }
        }
    }

    Ok(())
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn make_ping_frame(seq_id: u64, service_id: i32) -> PbFrame {
    PbFrame {
        seq_id,
        log_id: 0,
        service: service_id,
        method: 0,
        headers: vec![PbHeader {
            key: "type".into(),
            value: "ping".into(),
        }],
        payload: None,
    }
}

fn make_ack_frame(original: &PbFrame) -> PbFrame {
    let mut ack = original.clone();
    ack.payload = Some(br#"{"code":200,"headers":{},"data":[]}"#.to_vec());
    ack.headers.push(PbHeader {
        key: "biz_rt".into(),
        value: "0".into(),
    });
    ack
}

fn handle_control_frame(
    frame: &PbFrame,
    _client_config: &WsClientConfig,
    ping_secs: &mut u64,
    hb_interval: &mut tokio::time::Interval,
) {
    if frame.header_value("type") != "pong" {
        return;
    }
    // Pong payload may contain updated PingInterval
    if let Some(ref payload) = frame.payload {
        if let Ok(cfg) = serde_json::from_slice::<WsClientConfig>(payload) {
            if let Some(secs) = cfg.ping_interval {
                let secs = secs.max(10);
                if secs != *ping_secs {
                    *ping_secs = secs;
                    *hb_interval = tokio::time::interval(Duration::from_secs(secs));
                    tracing::info!(ping_interval = secs, "Feishu WS: ping_interval updated");
                }
            }
        }
    }
}

fn should_refresh_last_recv(msg: &WsMsg) -> bool {
    matches!(msg, WsMsg::Binary(_) | WsMsg::Ping(_) | WsMsg::Pong(_))
}

fn is_user_allowed(allow_from: &[String], open_id: &str) -> bool {
    allow_from.is_empty() || allow_from.iter().any(|u| u == "*" || u == open_id)
}

fn extract_service_id(url: &str) -> i32 {
    url.split('?')
        .nth(1)
        .and_then(|qs| {
            qs.split('&')
                .find(|kv| kv.starts_with("service_id="))
                .and_then(|kv| kv.split('=').nth(1))
                .and_then(|v| v.parse::<i32>().ok())
        })
        .unwrap_or(0)
}
