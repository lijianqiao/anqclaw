//! @file
//! @author <lijianqiao>
//! @since <2026-04-01>
//! @brief 负责 web_fetch 的 URL 校验、受限抓取与响应体限流读取。
//!
//! `web_fetch` tool — fetches a URL and returns the body as plain text.
//!
//! - HTML tags are stripped with a lightweight regex (no heavy HTML parser dep).
//! - Response body is truncated to `max_bytes` to prevent blowing up context.

use anyhow::{Result, bail};
use reqwest::header::LOCATION;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::time::Duration;
use tokio::net::lookup_host;
use url::Url;

use super::Tool;

const BODY_TRUNCATION_SENTINEL_BYTES: usize = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
struct CollectedBody {
    bytes: Vec<u8>,
    truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedRequestTarget {
    host: String,
    addrs: Vec<SocketAddr>,
}

#[derive(Debug)]
struct BodyCollector {
    bytes: Vec<u8>,
    max_bytes: usize,
    stop_bytes: usize,
    truncated: bool,
}

impl BodyCollector {
    fn new(max_bytes: u64) -> Self {
        let max_bytes = usize::try_from(max_bytes)
            .unwrap_or(usize::MAX.saturating_sub(BODY_TRUNCATION_SENTINEL_BYTES));
        let stop_bytes = max_bytes.saturating_add(BODY_TRUNCATION_SENTINEL_BYTES);
        Self {
            bytes: Vec::with_capacity(max_bytes.min(4096)),
            max_bytes,
            stop_bytes,
            truncated: false,
        }
    }

    fn push_chunk(&mut self, chunk: &[u8]) -> bool {
        if self.truncated {
            return false;
        }

        let remaining = self.stop_bytes.saturating_sub(self.bytes.len());
        if remaining == 0 {
            self.truncated = true;
            return false;
        }

        let keep = remaining.min(chunk.len());
        self.bytes.extend_from_slice(&chunk[..keep]);
        if chunk.len() > keep || self.bytes.len() > self.max_bytes {
            self.truncated = true;
        }

        !self.truncated
    }

    fn finish(mut self) -> CollectedBody {
        if self.bytes.len() > self.max_bytes {
            self.bytes.truncate(self.max_bytes);
        }

        CollectedBody {
            bytes: self.bytes,
            truncated: self.truncated,
        }
    }
}

/// WebFetch：受控抓取远程网页并返回清洗后的纯文本内容。
///
/// 详细说明：实例会在请求前校验 URL 协议、主机名和 DNS 解析结果，并在读取响应体时按字节上限流式截断，再做轻量 HTML 与空白折叠。
///
/// # Invariants
/// - 仅允许 `http` 与 `https`
/// - 响应体在清洗前就会按 `max_bytes` 做流式限流
#[derive(Clone)]
pub struct WebFetch {
    timeout: Duration,
    max_bytes: u64,
    blocked_domains: Vec<String>,
}

impl WebFetch {
    /// Create a new web fetch tool instance.
    ///
    /// # Args
    /// - `timeout_secs`: Request timeout in seconds.
    /// - `max_bytes`: Maximum response body bytes retained before truncation.
    /// - `blocked_domains`: Domain suffixes blocked by SSRF policy.
    ///
    /// # Returns
    /// - A configured `WebFetch` instance.
    pub fn new(timeout_secs: u32, max_bytes: u64, blocked_domains: Vec<String>) -> Self {
        Self {
            timeout: Duration::from_secs(timeout_secs as u64),
            max_bytes,
            blocked_domains,
        }
    }

    fn truncation_marker(max_bytes: u64) -> String {
        format!("[truncated at {max_bytes} bytes]")
    }

    fn render_collected_body(collected: CollectedBody, max_bytes: u64) -> String {
        let mut body = String::from_utf8_lossy(&collected.bytes).to_string();
        body = strip_html_tags(&body);
        body = collapse_whitespace(&body);

        if collected.truncated {
            if !body.is_empty() {
                body.push_str("\n\n");
            }
            body.push_str(&Self::truncation_marker(max_bytes));
        }

        body
    }

    async fn collect_response_body(
        mut resp: reqwest::Response,
        max_bytes: u64,
    ) -> Result<CollectedBody> {
        let mut collector = BodyCollector::new(max_bytes);

        while let Some(chunk) = resp.chunk().await? {
            if !collector.push_chunk(chunk.as_ref()) {
                break;
            }
        }

        Ok(collector.finish())
    }

    fn resolved_override_addr(ip: IpAddr) -> SocketAddr {
        SocketAddr::new(ip, 0)
    }

    fn base_client_builder(&self) -> reqwest::ClientBuilder {
        reqwest::Client::builder()
            .timeout(self.timeout)
            .redirect(reqwest::redirect::Policy::none())
    }

    fn build_client_for_resolved_host_with_builder(
        host: &str,
        addrs: &[SocketAddr],
        builder: reqwest::ClientBuilder,
    ) -> Result<reqwest::Client> {
        if addrs.is_empty() {
            bail!(
                "host `{host}` resolved to no usable addresses / 主机 `{host}` 没有可用的解析地址"
            );
        }

        Ok(builder.resolve_to_addrs(host, addrs).build()?)
    }

    fn build_client_for_resolved_host(
        &self,
        host: &str,
        addrs: &[SocketAddr],
    ) -> Result<reqwest::Client> {
        Self::build_client_for_resolved_host_with_builder(host, addrs, self.base_client_builder())
    }

    fn is_blocked_ip(ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => {
                v4.is_private()
                    || v4.is_loopback()
                    || v4.is_link_local()
                    || v4.is_unspecified()
                    || v4.is_broadcast()
            }
            IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_unique_local()
                    || v6.is_unicast_link_local()
            }
        }
    }

    fn check_host(&self, host: &str) -> Result<()> {
        for blocked in &self.blocked_domains {
            if host == blocked.as_str() || host.ends_with(&format!(".{blocked}")) {
                bail!(
                    "domain `{host}` is blocked (anti-SSRF protection) / 域名 `{host}` 已被屏蔽（防 SSRF 保护）"
                );
            }
        }

        if let Ok(ip) = host.parse::<IpAddr>()
            && Self::is_blocked_ip(ip)
        {
            bail!(
                "IP address `{host}` is blocked — private/reserved range (anti-SSRF) / IP 地址 `{host}` 已被屏蔽——私有/保留地址段（防 SSRF）"
            );
        }

        Ok(())
    }

    async fn resolve_public_addrs(&self, url: &Url) -> Result<Vec<SocketAddr>> {
        match url.scheme() {
            "http" | "https" => {}
            scheme => bail!("unsupported URL scheme `{scheme}` / 不支持的 URL 协议 `{scheme}`"),
        }

        let host = url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("URL is missing host / URL 缺少主机名"))?;
        self.check_host(host)?;

        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok(vec![Self::resolved_override_addr(ip)]);
        }

        let port = url.port_or_known_default().ok_or_else(|| {
            anyhow::anyhow!("cannot determine port for URL / 无法确定 URL 的端口")
        })?;
        let mut addrs = Vec::new();
        for addr in lookup_host((host, port)).await? {
            if Self::is_blocked_ip(addr.ip()) {
                bail!(
                    "resolved IP `{}` for host `{host}` is blocked (anti-SSRF) / 主机 `{host}` 解析的 IP `{}` 已被屏蔽（防 SSRF）",
                    addr.ip(),
                    addr.ip()
                );
            }

            let resolved = Self::resolved_override_addr(addr.ip());
            if !addrs.contains(&resolved) {
                addrs.push(resolved);
            }
        }

        if addrs.is_empty() {
            bail!(
                "host `{host}` resolved to no usable addresses / 主机 `{host}` 没有可用的解析地址"
            );
        }

        Ok(addrs)
    }

    async fn resolve_request_target(&self, url: &Url) -> Result<ResolvedRequestTarget> {
        let host = url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("URL is missing host / URL 缺少主机名"))?;
        let addrs = self.resolve_public_addrs(url).await?;
        Ok(ResolvedRequestTarget {
            host: host.to_string(),
            addrs,
        })
    }

    async fn fetch_url_with_resolver_and_builder<R, Fut, B>(
        &self,
        mut current_url: Url,
        resolve_target: &R,
        build_client: &B,
    ) -> Result<String>
    where
        R: Fn(Url) -> Fut,
        Fut: Future<Output = Result<ResolvedRequestTarget>>,
        B: Fn(&str, &[SocketAddr]) -> Result<reqwest::Client>,
    {
        let mut redirect_count = 0u8;
        let resp = loop {
            let resolved = resolve_target(current_url.clone()).await?;
            let client = build_client(&resolved.host, &resolved.addrs)?;
            let resp = client.get(current_url.clone()).send().await?;
            if resp.status().is_redirection() {
                redirect_count += 1;
                if redirect_count > 5 {
                    bail!("too many redirects while fetching URL / 获取 URL 时重定向次数过多");
                }
                let location = resp
                    .headers()
                    .get(LOCATION)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "redirect response missing Location header / 重定向响应缺少 Location 头"
                        )
                    })?
                    .to_str()?;
                current_url = current_url.join(location)?;
                continue;
            }
            break resp;
        };

        if !resp.status().is_success() {
            bail!(
                "HTTP {}: {} / HTTP 请求失败 {}: {}",
                resp.status(),
                current_url,
                resp.status(),
                current_url
            );
        }

        let collected = Self::collect_response_body(resp, self.max_bytes).await?;

        Ok(Self::render_collected_body(collected, self.max_bytes))
    }

    async fn do_execute(&self, args: serde_json::Value) -> Result<String> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing `url` parameter / 缺少 `url` 参数"))?;

        let current_url = Url::parse(url)?;

        self.fetch_url_with_resolver_and_builder(
            current_url,
            &|url| async move { self.resolve_request_target(&url).await },
            &|host, addrs| self.build_client_for_resolved_host(host, addrs),
        )
        .await
    }
}

impl Tool for WebFetch {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch a URL and return its content as plain text. HTML tags are stripped. Response is truncated if too large."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch"
                }
            },
            "required": ["url"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(self.do_execute(args))
    }
}

/// Strips HTML tags and suppresses content within `<script>` and `<style>` blocks.
fn strip_html_tags(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut in_tag = false;
    let mut suppress = false; // true when inside <script> or <style>
    let mut tag_buf = String::new();

    for ch in input.chars() {
        match ch {
            '<' => {
                in_tag = true;
                tag_buf.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                let tag_lower = tag_buf.to_ascii_lowercase();
                let tag_name = tag_lower.split_whitespace().next().unwrap_or("");
                if tag_name == "script" || tag_name == "style" {
                    suppress = true;
                } else if tag_name == "/script" || tag_name == "/style" {
                    suppress = false;
                }
                tag_buf.clear();
            }
            _ if in_tag => {
                tag_buf.push(ch);
            }
            _ if !suppress => {
                result.push(ch);
            }
            _ => {} // suppress content inside script/style
        }
    }
    result
}

/// Collapses runs of whitespace (including newlines) into at most two newlines.
fn collapse_whitespace(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut newline_count = 0u32;

    for ch in input.chars() {
        if ch == '\n' {
            newline_count += 1;
            if newline_count <= 2 {
                result.push('\n');
            }
        } else if ch.is_whitespace() {
            if newline_count == 0 && !result.ends_with(' ') {
                result.push(' ');
            }
        } else {
            newline_count = 0;
            result.push(ch);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::TcpListener;

    fn make_web() -> WebFetch {
        WebFetch::new(
            10,
            1024,
            vec!["localhost".to_string(), "169.254.169.254".to_string()],
        )
    }

    fn loopback_resolved_target(host: &str) -> ResolvedRequestTarget {
        ResolvedRequestTarget {
            host: host.to_string(),
            addrs: vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)],
        }
    }

    fn build_test_bound_client(host: &str, addrs: &[SocketAddr]) -> Result<reqwest::Client> {
        WebFetch::build_client_for_resolved_host_with_builder(
            host,
            addrs,
            reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy(),
        )
    }

    #[test]
    fn test_blocked_domain() {
        let web = make_web();
        let result = web.check_host("localhost");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("blocked"));
    }

    #[test]
    fn test_blocked_metadata_ip() {
        let web = make_web();
        let result = web.check_host("169.254.169.254");
        assert!(result.is_err());
    }

    #[test]
    fn test_private_ip_blocked() {
        let web = make_web();
        let result = web.check_host("10.0.0.1");
        assert!(result.is_err());
    }

    #[test]
    fn test_loopback_blocked() {
        let web = make_web();
        let result = web.check_host("127.0.0.1");
        assert!(result.is_err());
    }

    #[test]
    fn test_public_domain_allowed() {
        let web = make_web();
        let result = web.check_host("example.com");
        assert!(result.is_ok());
    }

    #[test]
    fn test_ipv6_unique_local_blocked() {
        assert!(WebFetch::is_blocked_ip("fd00::1".parse().unwrap()));
    }

    #[tokio::test]
    async fn test_resolve_public_addrs_rejects_unsupported_scheme() {
        let web = make_web();
        let url = Url::parse("file:///etc/passwd").unwrap();
        assert!(web.resolve_public_addrs(&url).await.is_err());
    }

    #[test]
    fn test_build_client_for_resolved_host_requires_addresses() {
        let error = WebFetch::build_client_for_resolved_host_with_builder(
            "example.com",
            &[],
            reqwest::Client::builder(),
        )
        .expect_err("empty DNS override list should be rejected / 空 DNS 覆盖列表应被拒绝");
        assert!(error.to_string().contains("no usable addresses"));
    }

    #[test]
    fn test_collapse_whitespace() {
        let input = "hello   world\n\n\n\nfoo";
        let result = collapse_whitespace(input);
        assert_eq!(result, "hello world\n\nfoo");
    }

    #[test]
    fn test_body_collector_stops_after_limit_plus_sentinel() {
        let mut collector = BodyCollector::new(5);

        assert!(collector.push_chunk(b"hello"));
        assert!(!collector.push_chunk(b" world"));

        let collected = collector.finish();
        assert_eq!(collected.bytes, b"hello");
        assert!(collected.truncated);
    }

    #[test]
    fn test_render_collected_body_appends_marker_after_cleanup() {
        let collected = CollectedBody {
            bytes: b"<script>".to_vec(),
            truncated: true,
        };

        let result = WebFetch::render_collected_body(collected, 8);
        assert_eq!(result, "[truncated at 8 bytes]");
    }

    #[tokio::test]
    async fn test_truncated_response_keeps_marker_after_html_cleanup() {
        async fn page() -> impl IntoResponse {
            (StatusCode::OK, "<script>1234567890</script><p>safe</p>")
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route("/page", get(page));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let web = WebFetch::new(5, 8, vec![]);
        let url = Url::parse(&format!("http://127.0.0.1:{}/page", addr.port())).unwrap();

        let result = web
            .fetch_url_with_resolver_and_builder(
                url,
                &|url| async move { Ok(loopback_resolved_target(url.host_str().unwrap())) },
                &build_test_bound_client,
            )
            .await
            .unwrap();

        server.abort();
        assert_eq!(result, "[truncated at 8 bytes]");
    }

    #[tokio::test]
    async fn test_fetch_url_binds_request_to_prevalidated_dns_results() {
        async fn page() -> impl IntoResponse {
            (StatusCode::OK, "bound ok")
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route("/page", get(page));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let web = make_web();
        let url = Url::parse(&format!("http://rebind.test:{}/page", addr.port())).unwrap();

        let result = web
            .fetch_url_with_resolver_and_builder(
                url,
                &|url| async move { Ok(loopback_resolved_target(url.host_str().unwrap())) },
                &build_test_bound_client,
            )
            .await
            .unwrap();

        server.abort();
        assert_eq!(result, "bound ok");
    }

    #[tokio::test]
    async fn test_redirect_chain_revalidates_each_hop() {
        async fn start(State(target): State<String>) -> impl IntoResponse {
            (StatusCode::FOUND, [(LOCATION, target)], "redirecting")
        }

        async fn blocked() -> impl IntoResponse {
            (StatusCode::OK, "should not be reached")
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let redirect_target = format!("http://127.0.0.1:{}/blocked", addr.port());
        let app = Router::new()
            .route("/start", get(start))
            .route("/blocked", get(blocked))
            .with_state(redirect_target.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let web = make_web();
        let validator_web = web.clone();
        let start_url = Url::parse(&format!("http://127.0.0.1:{}/start", addr.port())).unwrap();

        let result = web
            .fetch_url_with_resolver_and_builder(
                start_url,
                &|url| {
                    let validator_web = validator_web.clone();
                    async move {
                        if url.path() == "/start" {
                            Ok(loopback_resolved_target(url.host_str().unwrap()))
                        } else {
                            validator_web.resolve_request_target(&url).await
                        }
                    }
                },
                &build_test_bound_client,
            )
            .await;

        server.abort();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("blocked"));
    }

    #[tokio::test]
    async fn test_redirect_cross_domain_rebinds_each_hop() {
        async fn first_hop(State(target): State<String>) -> impl IntoResponse {
            (StatusCode::FOUND, [(LOCATION, target)], "hop-1")
        }

        async fn done() -> impl IntoResponse {
            (StatusCode::OK, "cross-domain ok")
        }

        let listener_one = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr_one = listener_one.local_addr().unwrap();
        let listener_two = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr_two = listener_two.local_addr().unwrap();

        let second_target = format!("http://second.test:{}/done", addr_two.port());

        let app_one = Router::new()
            .route("/start", get(first_hop))
            .with_state(second_target.clone());
        let app_two = Router::new().route("/done", get(done));

        let server_one = tokio::spawn(async move {
            axum::serve(listener_one, app_one).await.unwrap();
        });
        let server_two = tokio::spawn(async move {
            axum::serve(listener_two, app_two).await.unwrap();
        });

        let web = make_web();
        let start_url =
            Url::parse(&format!("http://first.test:{}/start", addr_one.port())).unwrap();

        let result = web
            .fetch_url_with_resolver_and_builder(
                start_url,
                &|url| async move { Ok(loopback_resolved_target(url.host_str().unwrap())) },
                &build_test_bound_client,
            )
            .await
            .unwrap();

        server_one.abort();
        server_two.abort();
        assert_eq!(result, "cross-domain ok");
    }

    #[tokio::test]
    async fn test_redirect_deep_chain_cross_domain_revalidates_each_hop() {
        async fn first_hop(State(target): State<String>) -> impl IntoResponse {
            (StatusCode::FOUND, [(LOCATION, target)], "hop-1")
        }

        async fn second_hop(State(target): State<String>) -> impl IntoResponse {
            (StatusCode::FOUND, [(LOCATION, target)], "hop-2")
        }

        async fn blocked() -> impl IntoResponse {
            (StatusCode::OK, "blocked target should not be reached")
        }

        let listener_one = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr_one = listener_one.local_addr().unwrap();
        let listener_two = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr_two = listener_two.local_addr().unwrap();

        let final_target = format!("http://127.0.0.1:{}/blocked", addr_one.port());
        let second_target = format!("http://localhost:{}/middle", addr_two.port());

        let app_one = Router::new()
            .route("/start", get(first_hop))
            .route("/blocked", get(blocked))
            .with_state(second_target.clone());
        let app_two = Router::new()
            .route("/middle", get(second_hop))
            .with_state(final_target.clone());

        let server_one = tokio::spawn(async move {
            axum::serve(listener_one, app_one).await.unwrap();
        });
        let server_two = tokio::spawn(async move {
            axum::serve(listener_two, app_two).await.unwrap();
        });

        let web = make_web();
        let validator_web = web.clone();
        let validate_calls = Arc::new(AtomicUsize::new(0));
        let start_url = Url::parse(&format!("http://127.0.0.1:{}/start", addr_one.port())).unwrap();

        let result = web
            .fetch_url_with_resolver_and_builder(
                start_url,
                &|url| {
                    let validator_web = validator_web.clone();
                    let validate_calls = validate_calls.clone();
                    async move {
                        validate_calls.fetch_add(1, Ordering::SeqCst);
                        if url.path() == "/start" || url.path() == "/middle" {
                            Ok(loopback_resolved_target(url.host_str().unwrap()))
                        } else {
                            validator_web.resolve_request_target(&url).await
                        }
                    }
                },
                &build_test_bound_client,
            )
            .await;

        server_one.abort();
        server_two.abort();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("blocked"));
        assert_eq!(validate_calls.load(Ordering::SeqCst), 3);
    }
}
