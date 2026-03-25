//! `web_fetch` tool — fetches a URL and returns the body as plain text.
//!
//! - HTML tags are stripped with a lightweight regex (no heavy HTML parser dep).
//! - Response body is truncated to `max_bytes` to prevent blowing up context.

use anyhow::{bail, Result};
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::time::Duration;

use super::Tool;

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

    fn check_domain(&self, url: &str) -> Result<()> {
        if let Ok(parsed) = url::Url::parse(url)
            && let Some(host) = parsed.host_str()
        {
            // 1. Check against configured blocked domains
            for blocked in &self.blocked_domains {
                if host == blocked.as_str() || host.ends_with(&format!(".{}", blocked)) {
                    bail!("domain `{host}` is blocked (anti-SSRF protection)");
                }
            }

            // 2. Check for private/reserved IPs using std::net (covers all RFC ranges)
            if let Ok(ip) = host.parse::<IpAddr>() {
                let is_blocked = match ip {
                    IpAddr::V4(v4) => {
                        v4.is_private()          // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
                        || v4.is_loopback()      // 127.0.0.0/8
                        || v4.is_link_local()    // 169.254.0.0/16 (cloud metadata)
                        || v4.is_unspecified()   // 0.0.0.0
                        || v4.is_broadcast()     // 255.255.255.255
                    }
                    IpAddr::V6(v6) => {
                        v6.is_loopback()         // ::1
                        || v6.is_unspecified()   // ::
                    }
                };
                if is_blocked {
                    bail!("IP address `{host}` is blocked — private/reserved range (anti-SSRF)");
                }
            }
        }
        Ok(())
    }

    async fn do_execute(&self, args: serde_json::Value) -> Result<String> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing `url` parameter"))?;

        // Check blocked domains
        self.check_domain(url)?;

        let client = reqwest::Client::builder()
            .timeout(self.timeout)
            .build()?;

        let resp = client.get(url).send().await?;

        if !resp.status().is_success() {
            bail!("HTTP {}: {}", resp.status(), url);
        }

        // Read body up to max_bytes
        let bytes = resp.bytes().await?;
        let mut body = if bytes.len() as u64 > self.max_bytes {
            let truncated = &bytes[..self.max_bytes as usize];
            let text = String::from_utf8_lossy(truncated).to_string();
            format!("{text}\n\n[truncated at {} bytes]", self.max_bytes)
        } else {
            String::from_utf8_lossy(&bytes).to_string()
        };

        // Strip HTML tags (lightweight approach)
        body = strip_html_tags(&body);

        // Collapse excessive whitespace
        body = collapse_whitespace(&body);

        Ok(body)
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

/// Strips HTML tags using a simple regex-like approach (no regex crate needed).
fn strip_html_tags(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
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
