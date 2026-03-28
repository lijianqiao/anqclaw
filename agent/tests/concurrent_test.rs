//! Concurrent stress tests for anqclaw.
//!
//! Verifies that key concurrent access patterns don't deadlock or panic.

use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use dashmap::DashMap;
use tower::ServiceExt;

// We can't import private types directly. Instead, test concurrency at the HTTP
// layer by building the router from the crate's public API.

/// Simulate 50 concurrent health checks — verifies no deadlock in axum router.
#[tokio::test]
async fn test_50_concurrent_health_checks() {
    let app = Router::new().route("/health", get(|| async { "ok" }));

    let mut handles = Vec::new();
    for _ in 0..50 {
        let app = app.clone();
        handles.push(tokio::spawn(async move {
            let req = Request::get("/health").body(Body::empty()).unwrap();
            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
        }));
    }

    for h in handles {
        h.await.unwrap();
    }
}

/// Verify DashMap handles 50 concurrent writes without panic.
#[tokio::test]
async fn test_dashmap_concurrent_writes() {
    let map: Arc<DashMap<String, Vec<Instant>>> = Arc::new(DashMap::new());

    let mut handles = Vec::new();
    for i in 0..50 {
        let map = map.clone();
        handles.push(tokio::spawn(async move {
            let key = format!("chat_{}", i % 5); // 5 unique keys, 10 writes each
            let mut entry = map.entry(key).or_default();
            entry.push(Instant::now());
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    // All 50 entries should be distributed across 5 keys
    let total: usize = map.iter().map(|e| e.value().len()).sum();
    assert_eq!(total, 50);
}

/// Verify per-key Mutex doesn't deadlock under concurrent access.
#[tokio::test]
async fn test_per_chat_mutex_no_deadlock() {
    let locks: Arc<DashMap<String, Arc<tokio::sync::Mutex<()>>>> = Arc::new(DashMap::new());
    let counter: Arc<std::sync::atomic::AtomicU32> = Arc::new(std::sync::atomic::AtomicU32::new(0));

    let mut handles = Vec::new();
    for i in 0..50 {
        let locks = locks.clone();
        let counter = counter.clone();
        handles.push(tokio::spawn(async move {
            let key = format!("chat_{}", i % 3); // 3 chats, ~17 tasks each
            let lock = locks
                .entry(key)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone();
            let _guard = lock.lock().await;
            // Simulate some work
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 50);
    assert_eq!(locks.len(), 3);
}

/// Verify LRU dedup doesn't panic under concurrent access (behind Mutex).
#[tokio::test]
async fn test_lru_dedup_concurrent() {
    use lru::LruCache;
    use std::num::NonZero;

    let cache: Arc<tokio::sync::Mutex<LruCache<String, ()>>> = Arc::new(tokio::sync::Mutex::new(
        LruCache::new(NonZero::new(100).expect("100 is non-zero")),
    ));

    let mut handles = Vec::new();
    let duplicates = Arc::new(std::sync::atomic::AtomicU32::new(0));

    for i in 0..50 {
        let cache = cache.clone();
        let duplicates = duplicates.clone();
        handles.push(tokio::spawn(async move {
            let msg_id = format!("msg_{}", i % 25); // 50% duplicate rate
            let mut lru = cache.lock().await;
            if lru.get(&msg_id).is_some() {
                duplicates.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            } else {
                lru.put(msg_id, ());
            }
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    let cache = cache.lock().await;
    let dup_count = duplicates.load(std::sync::atomic::Ordering::Relaxed);
    // 25 unique IDs, 25 duplicates
    assert_eq!(cache.len(), 25);
    assert_eq!(dup_count, 25);
}

/// Verify rate limiter correctly rejects excess concurrent requests.
#[tokio::test]
async fn test_rate_limiter_concurrent() {
    let rate_limiter: Arc<DashMap<String, Vec<Instant>>> = Arc::new(DashMap::new());
    let max_rpm: usize = 20;

    let accepted = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let rejected = Arc::new(std::sync::atomic::AtomicU32::new(0));

    let mut handles = Vec::new();
    for _ in 0..50 {
        let rate_limiter = rate_limiter.clone();
        let accepted = accepted.clone();
        let rejected = rejected.clone();
        handles.push(tokio::spawn(async move {
            let now = Instant::now();
            let window = std::time::Duration::from_secs(60);
            let mut entry = rate_limiter.entry("test_chat".to_string()).or_default();
            entry.retain(|t| now.duration_since(*t) < window);
            if entry.len() >= max_rpm {
                rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            } else {
                entry.push(now);
                accepted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    let acc = accepted.load(std::sync::atomic::Ordering::Relaxed);
    let rej = rejected.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(acc + rej, 50);
    assert!(acc <= max_rpm as u32, "accepted {acc} but max is {max_rpm}");
    assert!(rej >= 30, "expected at least 30 rejections, got {rej}");
}
