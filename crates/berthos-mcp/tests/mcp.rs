//! MCP tools refuse without a live lease and talk to the guest only.

use std::fs;

use berthos_mcp::{Mcp, McpContent, ToolResult};
use httptest::{matchers::*, responders::*, Expectation, Server};
use serde_json::json;

const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];
const TOKEN: &str = "lease_secret";

fn home() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn seed_pair(home: &std::path::Path, url: &str, token: &str) {
    fs::create_dir_all(home).unwrap();
    fs::write(
        home.join("client.toml"),
        format!("url = \"{url}\"\ntoken = \"{token}\"\n"),
    )
    .unwrap();
}

fn sample_lease() -> serde_json::Value {
    json!({
        "id": "l_1",
        "state": "live",
        "quote": {
            "vcpu": 2,
            "mem_gib": 4,
            "disk_gib": 40,
            "os": "linux",
            "density": "isolated",
            "min_seconds": 60,
            "occupancy_unit": "seconds",
            "notional_usd_per_hour": "0.048",
            "settlement": { "charged_here": false, "note": "quoted, not charged. listings and settlement live in https://github.com/hexuria/berth-market" }
        },
        "started_at": "2026-01-01T00:00:00Z",
        "ended_at": null,
        "viewer_url": "http://127.0.0.1:6080/"
    })
}

fn text_of(result: &ToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| match c {
            McpContent::Text { text } => Some(text.as_str()),
            McpContent::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn initialize_and_tools_list() {
    let dir = home();
    let mcp = Mcp::new(dir.path());
    let init = mcp
        .handle_rpc(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2024-11-05" }
        }))
        .unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], "berth");
    let listed = mcp
        .handle_rpc(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        }))
        .unwrap();
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "berth_screenshot",
            "berth_click",
            "berth_type",
            "berth_key",
            "berth_end"
        ]
    );
}

#[test]
fn screenshot_without_pair_or_lease_fails() {
    let dir = home();
    let mcp = Mcp::new(dir.path());
    let result = mcp.call_tool("berth_screenshot", json!({}));
    assert!(result.is_error);
    assert!(
        text_of(&result).contains("pair") || text_of(&result).contains("lease"),
        "{}",
        text_of(&result)
    );
}

#[test]
fn tools_refuse_when_lease_list_empty() {
    let dir = home();
    let server = Server::run();
    server.expect(
        Expectation::matching(all_of![
            request::method_path("GET", "/v1/leases"),
            request::headers(contains(("authorization", format!("Bearer {TOKEN}")))),
        ])
        .times(5)
        .respond_with(json_encoded(json!([]))),
    );
    let url = format!("http://{}", server.addr());
    seed_pair(dir.path(), &url, TOKEN);
    let mcp = Mcp::new(dir.path());
    for name in [
        "berth_screenshot",
        "berth_click",
        "berth_type",
        "berth_key",
        "berth_end",
    ] {
        let args = match name {
            "berth_click" => json!({ "x": 1, "y": 2 }),
            "berth_type" => json!({ "text": "hi" }),
            "berth_key" => json!({ "keys": ["Return"] }),
            _ => json!({}),
        };
        let result = mcp.call_tool(name, args);
        assert!(result.is_error, "{name} {}", text_of(&result));
        assert!(
            text_of(&result).contains("no live lease"),
            "{name} {}",
            text_of(&result)
        );
    }
}

#[test]
fn screenshot_returns_guest_png() {
    let dir = home();
    let server = Server::run();
    server.expect(
        Expectation::matching(request::method_path("GET", "/v1/leases"))
            .respond_with(json_encoded(json!([sample_lease()]))),
    );
    server.expect(
        Expectation::matching(all_of![
            request::method_path("GET", "/v1/leases/l_1/screenshot"),
            request::headers(contains(("authorization", format!("Bearer {TOKEN}")))),
        ])
        .respond_with(
            status_code(200)
                .append_header("content-type", "image/png")
                .body(PNG.to_vec()),
        ),
    );
    let url = format!("http://{}", server.addr());
    seed_pair(dir.path(), &url, TOKEN);
    let mcp = Mcp::new(dir.path());
    let result = mcp.call_tool("berth_screenshot", json!({}));
    assert!(!result.is_error, "{}", text_of(&result));
    match &result.content[0] {
        McpContent::Image { mime_type, data } => {
            assert_eq!(mime_type, "image/png");
            let raw = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
                .expect("b64");
            assert_eq!(raw, PNG);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn click_type_go_to_guest_actions() {
    let dir = home();
    let server = Server::run();
    server.expect(
        Expectation::matching(request::method_path("GET", "/v1/leases"))
            .times(2)
            .respond_with(json_encoded(json!([sample_lease()]))),
    );
    server.expect(
        Expectation::matching(all_of![
            request::method_path("POST", "/v1/leases/l_1/actions"),
            request::headers(contains(("authorization", format!("Bearer {TOKEN}")))),
            request::body(json_decoded(|v: &serde_json::Value| {
                v["op"] == "click" && v["x"] == 10 && v["y"] == 20 && v["button"] == "right"
            })),
        ])
        .respond_with(json_encoded(json!({ "ok": true, "target": "guest" }))),
    );
    server.expect(
        Expectation::matching(all_of![
            request::method_path("POST", "/v1/leases/l_1/actions"),
            request::body(json_decoded(|v: &serde_json::Value| {
                v["op"] == "type" && v["text"] == "hello"
            })),
        ])
        .respond_with(json_encoded(json!({ "ok": true, "target": "guest" }))),
    );
    let url = format!("http://{}", server.addr());
    seed_pair(dir.path(), &url, TOKEN);
    let mcp = Mcp::new(dir.path());
    let click = mcp.call_tool(
        "berth_click",
        json!({ "x": 10, "y": 20, "button": "right" }),
    );
    assert!(!click.is_error, "{}", text_of(&click));
    let typed = mcp.call_tool("berth_type", json!({ "text": "hello" }));
    assert!(!typed.is_error, "{}", text_of(&typed));
}

#[test]
fn end_lease_deletes_live_guest() {
    let dir = home();
    let server = Server::run();
    server.expect(
        Expectation::matching(request::method_path("GET", "/v1/leases"))
            .respond_with(json_encoded(json!([sample_lease()]))),
    );
    server.expect(
        Expectation::matching(all_of![
            request::method_path("DELETE", "/v1/leases/l_1"),
            request::headers(contains(("authorization", format!("Bearer {TOKEN}")))),
        ])
        .respond_with(json_encoded(json!({
            "lease_id": "l_1",
            "occupancy_seconds": 1,
            "min_seconds": 60,
            "billed_seconds": 60,
            "occupancy_unit": "seconds",
            "notional_usd": "0.000800",
            "reason": "graceful",
            "settlement": { "charged_here": false, "note": "quoted, not charged. listings and settlement live in https://github.com/hexuria/berth-market" }
        }))),
    );
    let url = format!("http://{}", server.addr());
    seed_pair(dir.path(), &url, TOKEN);
    let mcp = Mcp::new(dir.path());
    let ended = mcp.call_tool("berth_end", json!({}));
    assert!(!ended.is_error, "{}", text_of(&ended));
    assert!(text_of(&ended).contains("ended"));
}

#[test]
fn unauthorized_token_is_error() {
    let dir = home();
    let server = Server::run();
    server.expect(
        Expectation::matching(request::method_path("GET", "/v1/leases")).respond_with(
            status_code(401)
                .append_header("content-type", "application/json")
                .body(r#"{"error":"unknown pairing token"}"#),
        ),
    );
    let url = format!("http://{}", server.addr());
    seed_pair(dir.path(), &url, "stale");
    let mcp = Mcp::new(dir.path());
    let result = mcp.call_tool("berth_screenshot", json!({}));
    assert!(result.is_error);
    assert!(text_of(&result).contains("unknown pairing token"));
}
