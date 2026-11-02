use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::types::{BlockRow, ReaderOutput, Tier};

#[derive(Debug, Clone)]
pub struct ParseRequest {
    pub path: PathBuf,
    pub tier: Tier,
    pub page_from: u32,
    pub page_to: u32,
    pub force: bool,
    pub remote: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("{0}")]
    Transport(String),
    #[error("local engine is not running")]
    Unavailable,
    #[error("cloud parse is disabled on the desktop assistant")]
    CloudDisabled,
}

#[async_trait]
pub trait ParseProvider: Send + Sync {
    async fn parse(&self, request: &ParseRequest) -> Result<ReaderOutput, ProviderError>;
}

/// Talks to the assistant-owned MinerU over a Unix socket. TCP is never used.
pub struct LocalDoclibProvider {
    socket: PathBuf,
}

impl LocalDoclibProvider {
    pub fn unix(socket: impl Into<PathBuf>) -> Self {
        Self { socket: socket.into() }
    }
}

#[derive(Debug, Deserialize)]
struct ParseCreated {
    sha256: String,
    status: String,
    #[serde(default)]
    wait_parse_ids: Vec<i64>,
    #[serde(default)]
    created_parse_ids: Vec<i64>,
    #[serde(default)]
    reused_parse_ids: Vec<i64>,
}

#[derive(Debug, Deserialize)]
struct ParseStatus {
    status: String,
    #[serde(default)]
    error_msg: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

#[async_trait]
impl ParseProvider for LocalDoclibProvider {
    async fn parse(&self, request: &ParseRequest) -> Result<ReaderOutput, ProviderError> {
        if request.remote {
            return Err(ProviderError::CloudDisabled);
        }
        if !self.socket.exists() {
            return Err(ProviderError::Unavailable);
        }
        let page_range = if request.page_from == request.page_to {
            request.page_from.to_string()
        } else {
            format!("{}-{}", request.page_from, request.page_to)
        };
        let created: ParseCreated = serde_json::from_value(
            unix_json(
                &self.socket,
                "POST",
                "/api/v1/parses",
                Some(&json!({
                    "path": request.path.to_string_lossy(),
                    "tier": request.tier.as_str(),
                    "page_range": page_range,
                    "force": request.force,
                    "remote": false,
                })),
            )
            .await?,
        )
        .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let mut ids = created.wait_parse_ids;
        ids.extend(created.created_parse_ids);
        ids.extend(created.reused_parse_ids);
        ids.sort_unstable();
        ids.dedup();
        if created.status != "done" || !ids.is_empty() {
            poll_until_ready(&self.socket, &ids).await?;
        }

        let query = format!(
            "/api/v1/docs/{}/content?tier={}&page_range={}&format=markdown",
            created.sha256,
            request.tier.as_str(),
            page_range
        );
        let body = unix_json(&self.socket, "GET", &query, None).await?;
        Ok(content_to_output(&body, request.tier, request.page_from))
    }
}

/// `GET /api/v1/server/status`. `parse_server.local.healthy` is how a caller
/// tells that MinerU has finished loading its models, which happens well after
/// the socket file appears.
pub async fn local_server_status(socket: &Path) -> Result<Value, ProviderError> {
    unix_json(socket, "GET", "/api/v1/server/status", None).await
}

pub async fn local_config(socket: &Path, key: &str) -> Result<String, ProviderError> {
    let body = unix_json(socket, "GET", &format!("/api/v1/configs/{key}"), None).await?;
    Ok(body["value"].as_str().unwrap_or_default().to_string())
}

pub async fn local_set_config(socket: &Path, key: &str, value: &str) -> Result<(), ProviderError> {
    unix_json(
        socket,
        "PUT",
        &format!("/api/v1/configs/{key}"),
        Some(&json!({ "value": value })),
    )
    .await
    .map(|_| ())
}

async fn poll_until_ready(socket: &Path, ids: &[i64]) -> Result<(), ProviderError> {
    if ids.is_empty() {
        return Ok(());
    }
    for _ in 0..600 {
        let mut pending = false;
        for id in ids {
            let status: ParseStatus =
                serde_json::from_value(unix_json(socket, "GET", &format!("/api/v1/parses/{id}"), None).await?)
                    .map_err(|e| ProviderError::Transport(e.to_string()))?;
            match status.status.as_str() {
                "done" | "superseded" => {}
                "failed" | "error" => {
                    return Err(ProviderError::Transport(
                        status
                            .error_msg
                            .or(status.error)
                            .unwrap_or_else(|| "parse failed".into()),
                    ));
                }
                _ => pending = true,
            }
        }
        if !pending {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(ProviderError::Transport("parse timed out".into()))
}

async fn unix_json(socket: &Path, method: &str, path: &str, body: Option<&Value>) -> Result<Value, ProviderError> {
    if !socket.exists() {
        return Err(ProviderError::Unavailable);
    }
    let payload = match body {
        Some(value) => serde_json::to_vec(value).map_err(|e| ProviderError::Transport(e.to_string()))?,
        None => Vec::new(),
    };
    let mut stream = UnixStream::connect(socket).await.map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => ProviderError::Unavailable,
        _ => ProviderError::Transport(e.to_string()),
    })?;
    let extra = if body.is_some() {
        "Content-Type: application/json\r\n"
    } else {
        ""
    };
    let header = format!(
        "{method} {path} HTTP/1.1\r\nHost: mineru\r\nAccept: application/json\r\nConnection: close\r\n{extra}Content-Length: {}\r\n\r\n",
        payload.len()
    );
    stream
        .write_all(header.as_bytes())
        .await
        .map_err(|e| ProviderError::Transport(e.to_string()))?;
    if !payload.is_empty() {
        stream
            .write_all(&payload)
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
    }
    // No write-side shutdown: uvicorn drops a half-closed request without a
    // response. `Connection: close` is what ends the read.
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(600), stream.read_to_end(&mut buf))
        .await
        .map_err(|_| ProviderError::Transport("local parse socket timed out".into()))?
        .map_err(|e| ProviderError::Transport(e.to_string()))?;
    parse_http_json(&buf)
}

fn parse_http_json(raw: &[u8]) -> Result<Value, ProviderError> {
    let text = String::from_utf8_lossy(raw);
    let (head, rest) = text
        .split_once("\r\n\r\n")
        .or_else(|| text.split_once("\n\n"))
        .ok_or_else(|| ProviderError::Transport("empty HTTP response from MinerU".into()))?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    let headers = head.to_ascii_lowercase();
    let body = if headers.contains("transfer-encoding: chunked") {
        decode_chunked(rest.as_bytes())
    } else {
        rest.as_bytes().to_vec()
    };
    if status >= 400 {
        return Err(ProviderError::Transport(format!(
            "MinerU HTTP {status}: {}",
            String::from_utf8_lossy(&body)
        )));
    }
    if body.is_empty() {
        return Ok(Value::Null);
    }
    let value: Value = serde_json::from_slice(&body).map_err(|e| ProviderError::Transport(e.to_string()))?;
    if let Some(err) = value.get("error") {
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| err.as_str())
            .unwrap_or("MinerU error");
        return Err(ProviderError::Transport(message.to_string()));
    }
    Ok(value)
}

fn decode_chunked(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = input;
    while let Some(idx) = rest.iter().position(|&b| b == b'\n') {
        let size_line = std::str::from_utf8(&rest[..idx]).unwrap_or("").trim();
        let size = usize::from_str_radix(size_line.trim_end_matches('\r'), 16).unwrap_or(0);
        rest = &rest[idx + 1..];
        if size == 0 {
            break;
        }
        if rest.len() < size {
            out.extend_from_slice(rest);
            break;
        }
        out.extend_from_slice(&rest[..size]);
        rest = if rest.len() > size + 2 { &rest[size + 2..] } else { &[] };
    }
    out
}

fn content_to_output(body: &Value, tier: Tier, page_no: u32) -> ReaderOutput {
    let text = body
        .get("content")
        .or_else(|| body.get("md_content"))
        .or_else(|| body.get("markdown"))
        .or_else(|| body.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let blocks = body
        .get("blocks")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .enumerate()
                .map(|(i, block)| BlockRow {
                    block_id: format!("mineru-{i}"),
                    sha256: String::new(),
                    page_no: block.get("page").and_then(Value::as_u64).unwrap_or(page_no as u64) as u32,
                    label: block.get("type").and_then(Value::as_str).map(str::to_string),
                    bbox: block_bbox(block),
                    text: block.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
                    html: None,
                    confidence: block.get("score").and_then(Value::as_f64),
                    reading_order: Some(i as i32),
                    provider: format!("mineru-{}", tier.as_str()),
                })
                .collect()
        })
        .unwrap_or_else(|| {
            if text.is_empty() {
                Vec::new()
            } else {
                vec![BlockRow {
                    block_id: format!("mineru-md-{page_no}"),
                    sha256: String::new(),
                    page_no,
                    label: Some("markdown".into()),
                    bbox: None,
                    text: text.clone(),
                    html: None,
                    confidence: Some(0.8),
                    reading_order: Some(0),
                    provider: format!("mineru-{}", tier.as_str()),
                }]
            }
        });
    ReaderOutput {
        provider: format!("mineru-{}", tier.as_str()),
        tier: Some(tier.as_str().into()),
        confidence: if text.is_empty() { 0.2 } else { 0.8 },
        text,
        blocks,
        fields: Vec::new(),
    }
}

fn block_bbox(block: &Value) -> Option<[f64; 4]> {
    let raw = block.get("bbox").or_else(|| block.get("bbox_norm"))?;
    let nums: Vec<f64> = raw.as_array()?.iter().filter_map(Value::as_f64).collect();
    (nums.len() == 4).then(|| [nums[0], nums[1], nums[2], nums[3]])
}

/// Present so the web trial can share the trait. The desktop assistant never
/// constructs this provider.
pub struct CloudParseProvider {
    pub endpoint: String,
}

#[async_trait]
impl ParseProvider for CloudParseProvider {
    async fn parse(&self, _request: &ParseRequest) -> Result<ReaderOutput, ProviderError> {
        let _ = &self.endpoint;
        Err(ProviderError::CloudDisabled)
    }
}

#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
pub struct DesktopCall {
    pub path: PathBuf,
    pub tier: &'static str,
    pub page_range: String,
    pub force: bool,
    pub remote: bool,
}

impl DesktopCall {
    #[allow(dead_code)]
    pub fn local(path: PathBuf, tier: Tier, page: u32) -> Self {
        Self {
            path,
            tier: match tier {
                Tier::Basic => "basic",
                Tier::Standard => "standard",
                Tier::Advanced => "advanced",
            },
            page_range: page.to_string(),
            force: false,
            remote: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_json_reads_content_and_chunked_bodies() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"content\":\"TSN 1\"}";
        assert_eq!(parse_http_json(plain).unwrap()["content"], "TSN 1");
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\na\r\n{\"a\":true}\r\n0\r\n\r\n";
        assert_eq!(parse_http_json(chunked).unwrap()["a"], true);
    }

    #[tokio::test]
    async fn cloud_stays_disabled() {
        let err = CloudParseProvider {
            endpoint: "https://example.invalid".into(),
        }
        .parse(&ParseRequest {
            path: PathBuf::from("/tmp/x.pdf"),
            tier: Tier::Advanced,
            page_from: 1,
            page_to: 1,
            force: false,
            remote: false,
        })
        .await
        .unwrap_err();
        assert!(matches!(err, ProviderError::CloudDisabled));
    }
}
