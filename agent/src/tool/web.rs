//! `web_fetch` tool — fetches a URL and returns the body as plain text.
//!
//! - HTML tags are stripped with a lightweight regex (no heavy HTML parser dep).
//! - Response body is truncated to `max_bytes` to prevent blowing up context.

use anyhow::{Result, bail};
use reqwest::header::LOCATION;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::time::Duration;
use tokio::net::lookup_host;
use url::Url;

use super::Tool;

#[derive(Clone)]
pub struct WebFetch {
    timeout: Duration,
    max_bytes: u64,
    blocked_domains: Vec<String>,
}

impl WebFetch {
    pub fn new(timeout_secs: u32, max_bytes: u64, blocked_domains: Vec<String>) -> Self {
        Self {
            timeout: Duration::from_secs(timeout_secs as u64),
            max_bytes,
            blocked_domains,
        }
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

    async fn validate_url(&self, url: &Url) -> Result<()> {
        match url.scheme() {
            "http" | "https" => {}
            scheme => bail!("unsupported URL scheme `{scheme}` / 不支持的 URL 协议 `{scheme}`"),
        }

        let host = url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("URL is missing host / URL 缺少主机名"))?;
        self.check_host(host)?;

        if host.parse::<IpAddr>().is_err() {
            let port = url.port_or_known_default().ok_or_else(|| {
                anyhow::anyhow!("cannot determine port for URL / 无法确定 URL 的端口")
            })?;
            for addr in lookup_host((host, port)).await? {
                if Self::is_blocked_ip(addr.ip()) {
                    bail!(
                        "resolved IP `{}` for host `{host}` is blocked (anti-SSRF) / 主机 `{host}` 解析的 IP `{}` 已被屏蔽（防 SSRF）",
                        addr.ip(),
                        addr.ip()
                    );
                }
            }
        }

        Ok(())
    }

    async fn fetch_url_with_validator<F, Fut>(
        &self,
        mut current_url: Url,
        client: &reqwest::Client,
        validate: &F,
    ) -> Result<String>
    where
        F: Fn(Url) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        validate(current_url.clone()).await?;

        let mut redirect_count = 0u8;
        let resp = loop {
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
                validate(current_url.clone()).await?;
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

        let bytes = resp.bytes().await?;
        let mut body = if bytes.len() as u64 > self.max_bytes {
            let truncated = &bytes[..self.max_bytes as usize];
            let text = String::from_utf8_lossy(truncated).to_string();
            format!("{text}\n\n[truncated at {} bytes]", self.max_bytes)
        } else {
            String::from_utf8_lossy(&bytes).to_string()
        };

        body = strip_html_tags(&body);
        body = collapse_whitespace(&body);

        Ok(body)
    }

    async fn do_execute(&self, args: serde_json::Value) -> Result<String> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing `url` parameter / 缺少 `url` 参数"))?;

        let current_url = Url::parse(url)?;

        let client = reqwest::Client::builder()
            .timeout(self.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        self.fetch_url_with_validator(current_url, &client, &|url| async move {
            self.validate_url(&url).await
        })
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
    async fn test_validate_url_rejects_unsupported_scheme() {
        let web = make_web();
        let url = Url::parse("file:///etc/passwd").unwrap();
        assert!(web.validate_url(&url).await.is_err());
    }

    #[test]
    fn test_collapse_whitespace() {
        let input = "hello   world\n\n\n\nfoo";
        let result = collapse_whitespace(input);
        assert_eq!(result, "hello world\n\nfoo");
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
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .unwrap();
        let start_url = Url::parse(&format!("http://127.0.0.1:{}/start", addr.port())).unwrap();

        let result = web
            .fetch_url_with_validator(start_url, &client, &|url| {
                let validator_web = validator_web.clone();
                async move {
                    if url.path() == "/start" {
                        Ok(())
                    } else {
                        validator_web.validate_url(&url).await
                    }
                }
            })
            .await;

        server.abort();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("blocked"));
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
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .unwrap();
        let start_url = Url::parse(&format!("http://127.0.0.1:{}/start", addr_one.port())).unwrap();

        let result = web
            .fetch_url_with_validator(start_url, &client, &|url| {
                let validator_web = validator_web.clone();
                let validate_calls = validate_calls.clone();
                async move {
                    validate_calls.fetch_add(1, Ordering::SeqCst);
                    if url.path() == "/start" || url.path() == "/middle" {
                        Ok(())
                    } else {
                        validator_web.validate_url(&url).await
                    }
                }
            })
            .await;

        server_one.abort();
        server_two.abort();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("blocked"));
        assert_eq!(validate_calls.load(Ordering::SeqCst), 3);
    }
}
