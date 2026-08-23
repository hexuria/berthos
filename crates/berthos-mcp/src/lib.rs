//! MCP stdio server for a live isolated Linux guest.
//!
//! Tools talk to the node over loopback HTTP with the `lease` bearer.
//! They refuse when no lease is live. They never target the host desktop.

#![forbid(unsafe_code)]

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// MCP content item.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpContent {
    /// Text payload.
    Text {
        /// Message.
        text: String,
    },
    /// PNG (or other) image, base64.
    Image {
        /// Base64 payload.
        data: String,
        /// MIME type.
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}

/// Result of `tools/call`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// Content blocks.
    pub content: Vec<McpContent>,
    /// True when the tool failed.
    #[serde(rename = "isError", skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
}

impl ToolResult {
    /// Plain-text success.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![McpContent::Text { text: text.into() }],
            is_error: false,
        }
    }

    /// Plain-text error.
    pub fn err(text: impl Into<String>) -> Self {
        Self {
            content: vec![McpContent::Text { text: text.into() }],
            is_error: true,
        }
    }
}

/// MCP server bound to a Berthos home (client.toml).
pub struct Mcp {
    home: PathBuf,
}

impl Mcp {
    /// New server that reads `$BERTHOS_HOME` / `~/.berthos`.
    #[must_use]
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    /// Dispatch one tool. Errors become `isError` results.
    pub fn call_tool(&self, name: &str, arguments: serde_json::Value) -> ToolResult {
        match self.dispatch(name, arguments) {
            Ok(result) => result,
            Err(err) => ToolResult::err(err.to_string()),
        }
    }

    fn dispatch(&self, name: &str, args: serde_json::Value) -> Result<ToolResult, McpError> {
        match name {
            "berth_screenshot" => self.screenshot(),
            "berth_click" => self.click(&args),
            "berth_type" => self.type_text(&args),
            "berth_key" => self.key(&args),
            "berth_end" => self.end(),
            other => Err(McpError::Usage(format!("unknown tool `{other}`"))),
        }
    }

    fn screenshot(&self) -> Result<ToolResult, McpError> {
        let (client, lease_id) = self.require_live()?;
        let png = client.get_bytes(&format!("/v1/leases/{lease_id}/screenshot"))?;
        Ok(ToolResult {
            content: vec![McpContent::Image {
                data: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &png),
                mime_type: "image/png".into(),
            }],
            is_error: false,
        })
    }

    fn click(&self, args: &serde_json::Value) -> Result<ToolResult, McpError> {
        let x = require_i32(args, "x")?;
        let y = require_i32(args, "y")?;
        let button = args
            .get("button")
            .and_then(|v| v.as_str())
            .unwrap_or("left");
        let (client, lease_id) = self.require_live()?;
        client.post_json(
            &format!("/v1/leases/{lease_id}/actions"),
            serde_json::json!({ "op": "click", "x": x, "y": y, "button": button }),
        )?;
        Ok(ToolResult::text("OK"))
    }

    fn type_text(&self, args: &serde_json::Value) -> Result<ToolResult, McpError> {
        let text = args
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::Usage("text is required".into()))?;
        if text.is_empty() {
            return Err(McpError::Usage("text is empty".into()));
        }
        let (client, lease_id) = self.require_live()?;
        client.post_json(
            &format!("/v1/leases/{lease_id}/actions"),
            serde_json::json!({ "op": "type", "text": text }),
        )?;
        Ok(ToolResult::text("OK"))
    }

    fn key(&self, args: &serde_json::Value) -> Result<ToolResult, McpError> {
        let keys = parse_keys(args.get("keys"))?;
        let (client, lease_id) = self.require_live()?;
        client.post_json(
            &format!("/v1/leases/{lease_id}/actions"),
            serde_json::json!({ "op": "key", "keys": keys }),
        )?;
        Ok(ToolResult::text("OK"))
    }

    fn end(&self) -> Result<ToolResult, McpError> {
        let (client, lease_id) = self.require_live()?;
        match client.delete(&format!("/v1/leases/{lease_id}")) {
            Ok(_) => Ok(ToolResult::text(format!("lease {lease_id} ended"))),
            Err(McpError::Api { status: 404, .. }) => {
                Ok(ToolResult::text(format!("lease {lease_id} gone")))
            }
            Err(err) => Err(err),
        }
    }

    fn require_live(&self) -> Result<(NodeClient, String), McpError> {
        let cfg = ClientConfig::load(&self.home)?;
        let client = NodeClient {
            url: cfg.url,
            token: cfg.token,
        };
        let leases = client.list_leases()?;
        let lease = leases.first().ok_or_else(|| {
            McpError::Usage(
                "no live lease; create one with berth up --os linux (this repo does not take payment)"
                    .into(),
            )
        })?;
        Ok((client, lease.id.0.clone()))
    }

    /// Handle one JSON-RPC message. Notifications return `None`.
    pub fn handle_rpc(&self, msg: serde_json::Value) -> Option<serde_json::Value> {
        let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let id = msg.get("id").cloned();
        let params = msg
            .get("params")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        if method.is_empty() {
            return Some(rpc_error(id, -32600, "invalid request"));
        }
        match method {
            "initialize" => Some(rpc_ok(id, initialize_result(&params))),
            "notifications/initialized" | "initialized" => None,
            "ping" => Some(rpc_ok(id, serde_json::json!({}))),
            "tools/list" => Some(rpc_ok(id, serde_json::json!({ "tools": tools() }))),
            "tools/call" => {
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                let result = self.call_tool(name, args);
                match serde_json::to_value(&result) {
                    Ok(value) => Some(rpc_ok(id, value)),
                    Err(err) => Some(rpc_error(id, -32603, &format!("serialize: {err}"))),
                }
            }
            other => {
                if id.is_none() {
                    None
                } else {
                    Some(rpc_error(id, -32601, &format!("method not found: {other}")))
                }
            }
        }
    }
}

/// Serve MCP on stdio until EOF.
pub fn serve_blocking(home: &Path) -> Result<(), McpError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(serve(home))
}

/// Async stdio loop.
pub async fn serve(home: &Path) -> Result<(), McpError> {
    let mcp = Mcp::new(home);
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(msg) => {
                if let Some(resp) = mcp.handle_rpc(msg) {
                    write_line(&mut stdout, &resp).await?;
                }
            }
            Err(err) => {
                let resp = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("parse error: {err}") }
                });
                write_line(&mut stdout, &resp).await?;
            }
        }
    }
    Ok(())
}

async fn write_line(
    stdout: &mut tokio::io::Stdout,
    value: &serde_json::Value,
) -> Result<(), McpError> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    stdout.write_all(line.as_bytes()).await?;
    stdout.flush().await?;
    Ok(())
}

fn initialize_result(params: &serde_json::Value) -> serde_json::Value {
    let version = params
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("2024-11-05");
    serde_json::json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": {
            "name": "berth",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": "Drive the live isolated Linux guest with berth_screenshot / berth_click / berth_type / berth_key / berth_end. Tools refuse if no lease is live. Never drive the host desktop."
    })
}

fn rpc_ok(id: Option<serde_json::Value>, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(serde_json::Value::Null),
        "result": result
    })
}

fn rpc_error(id: Option<serde_json::Value>, code: i64, message: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(serde_json::Value::Null),
        "error": { "code": code, "message": message }
    })
}

fn tools() -> Vec<serde_json::Value> {
    vec![
        tool(
            "berth_screenshot",
            "Capture the live guest desktop as a PNG. Refuses if no lease is live. Never captures the host display.",
            serde_json::json!({ "type": "object", "properties": {} }),
        ),
        tool(
            "berth_click",
            "Click in the live guest at pixel coordinates (origin top-left). Refuses if no lease is live.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "x": { "type": "integer" },
                    "y": { "type": "integer" },
                    "button": { "type": "string", "enum": ["left", "right", "middle"] }
                },
                "required": ["x", "y"]
            }),
        ),
        tool(
            "berth_type",
            "Type text into the live guest desktop. Refuses if no lease is live.",
            serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            }),
        ),
        tool(
            "berth_key",
            "Press a key or chord in the live guest. Refuses if no lease is live.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "keys": { "type": ["string", "array"], "items": { "type": "string" } }
                },
                "required": ["keys"]
            }),
        ),
        tool(
            "berth_end",
            "End the live lease (destroy the guest and its loopback view). Refuses if no lease is live.",
            serde_json::json!({ "type": "object", "properties": {} }),
        ),
    ]
}

fn tool(name: &str, description: &str, input_schema: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema
    })
}

#[derive(Debug, Deserialize)]
struct ClientConfig {
    url: String,
    token: String,
}

impl ClientConfig {
    fn load(home: &Path) -> Result<Self, McpError> {
        let path = home.join("client.toml");
        let text = std::fs::read_to_string(&path).map_err(|_| {
            McpError::Usage("not paired; run berth pair and create a linux lease".into())
        })?;
        let cfg: Self = toml::from_str(&text)?;
        if cfg.token.is_empty() {
            return Err(McpError::Usage("client.toml has an empty token".into()));
        }
        Ok(cfg)
    }
}

struct NodeClient {
    url: String,
    token: String,
}

impl NodeClient {
    fn list_leases(&self) -> Result<Vec<berthos_protocol::Lease>, McpError> {
        let value = self.get_json("/v1/leases")?;
        Ok(serde_json::from_value(value)?)
    }

    fn get_json(&self, path: &str) -> Result<serde_json::Value, McpError> {
        let resp = self
            .agent()
            .get(&format!("{}{path}", self.url.trim_end_matches('/')))
            .set("authorization", &format!("Bearer {}", self.token))
            .call()
            .map_err(map_ureq)?;
        read_json(resp)
    }

    fn get_bytes(&self, path: &str) -> Result<Vec<u8>, McpError> {
        let resp = self
            .agent()
            .get(&format!("{}{path}", self.url.trim_end_matches('/')))
            .set("authorization", &format!("Bearer {}", self.token))
            .call()
            .map_err(map_ureq)?;
        if !(200..300).contains(&resp.status()) {
            return Err(api_from_response(resp));
        }
        let mut buf = Vec::new();
        resp.into_reader()
            .take(8 * 1024 * 1024)
            .read_to_end(&mut buf)?;
        Ok(buf)
    }

    fn post_json(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, McpError> {
        let resp = self
            .agent()
            .post(&format!("{}{path}", self.url.trim_end_matches('/')))
            .set("authorization", &format!("Bearer {}", self.token))
            .set("content-type", "application/json")
            .send_json(body)
            .map_err(map_ureq)?;
        read_json(resp)
    }

    fn delete(&self, path: &str) -> Result<serde_json::Value, McpError> {
        let resp = self
            .agent()
            .delete(&format!("{}{path}", self.url.trim_end_matches('/')))
            .set("authorization", &format!("Bearer {}", self.token))
            .call()
            .map_err(map_ureq)?;
        read_json(resp)
    }

    fn agent(&self) -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(30))
            .build()
    }
}

fn read_json(resp: ureq::Response) -> Result<serde_json::Value, McpError> {
    let status = resp.status();
    if !(200..300).contains(&status) {
        return Err(api_from_response(resp));
    }
    if resp
        .header("content-type")
        .is_some_and(|c| c.contains("application/json"))
        || status != 204
    {
        let value: serde_json::Value = resp.into_json().unwrap_or(serde_json::Value::Null);
        return Ok(value);
    }
    Ok(serde_json::Value::Null)
}

fn api_from_response(resp: ureq::Response) -> McpError {
    let status = resp.status();
    let message = resp
        .into_json::<serde_json::Value>()
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| format!("http {status}"));
    McpError::Api { status, message }
}

fn map_ureq(err: ureq::Error) -> McpError {
    match err {
        ureq::Error::Status(status, resp) => {
            let message = resp
                .into_json::<serde_json::Value>()
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .and_then(|e| e.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| format!("http {status}"));
            McpError::Api { status, message }
        }
        ureq::Error::Transport(t) => McpError::Http(t.to_string()),
    }
}

fn require_i32(args: &serde_json::Value, key: &str) -> Result<i32, McpError> {
    let value = args
        .get(key)
        .ok_or_else(|| McpError::Usage(format!("{key} is required")))?;
    if let Some(n) = value.as_i64() {
        return i32::try_from(n).map_err(|_| McpError::Usage(format!("{key} is out of range")));
    }
    Err(McpError::Usage(format!("{key} must be an integer")))
}

fn parse_keys(value: Option<&serde_json::Value>) -> Result<Vec<String>, McpError> {
    let Some(value) = value else {
        return Err(McpError::Usage("keys is required".into()));
    };
    let keys = match value {
        serde_json::Value::String(s) => split_keys(s),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str())
            .flat_map(split_keys)
            .collect(),
        _ => {
            return Err(McpError::Usage(
                "keys must be a string or array of strings".into(),
            ));
        }
    };
    if keys.is_empty() {
        return Err(McpError::Usage("keys is empty".into()));
    }
    Ok(keys)
}

fn split_keys(text: &str) -> Vec<String> {
    text.split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// MCP failures.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    /// Bad tool arguments or missing pair/lease.
    #[error("{0}")]
    Usage(String),
    /// Node HTTP error.
    #[error("{message}")]
    Api {
        /// Status code.
        status: u16,
        /// Body message.
        message: String,
    },
    /// Transport.
    #[error("could not reach node: {0}")]
    Http(String),
    /// JSON.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// IO.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Config TOML.
    #[error("config: {0}")]
    Config(#[from] toml::de::Error),
}
