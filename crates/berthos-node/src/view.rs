//! Lease-scoped loopback guest view.
//!
//! Binds `127.0.0.1:0` only. Serves a noVNC-equivalent page of the *guest*
//! Xvfb (screenshot stream + click/type, plus a websockify pipe to guest
//! x11vnc). Host DISPLAY / host cursor are never read. The listener is
//! dropped when the lease ends.

use std::net::SocketAddr;
use std::process::Stdio;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use berthos_protocol::LeaseId;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use crate::action::{action_argv, argv_targets_host, parse_button, GuestOp, PNG_MAGIC};
use crate::http::{ApiError, NodeState};
use crate::pairing::Capability;

/// A live loopback viewer for one lease.
pub struct LoopbackView {
    /// Bound address (always loopback).
    pub addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
}

impl LoopbackView {
    /// Bind `127.0.0.1:0` and serve this lease's guest. Bind-all is refused.
    pub async fn start(state: NodeState, lease_id: LeaseId) -> Result<Self, ViewError> {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .map_err(|e| ViewError::Bind(e.to_string()))?;
        let addr = listener
            .local_addr()
            .map_err(|e| ViewError::Bind(e.to_string()))?;
        if !addr.ip().is_loopback() {
            return Err(ViewError::BindAll(addr));
        }
        let (tx, rx) = oneshot::channel();
        let router = view_router(ViewState {
            node: state,
            lease_id,
        });
        tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = rx.await;
                })
                .await;
        });
        Ok(Self {
            addr,
            shutdown: Some(tx),
        })
    }

    /// Loopback URL for this lease's guest desktop.
    pub fn url(&self) -> String {
        format!("http://{}/", self.addr)
    }

    /// Stop the listener. Further connects fail.
    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for LoopbackView {
    fn drop(&mut self) {
        self.stop();
    }
}

/// View start failures.
#[derive(Debug, thiserror::Error)]
pub enum ViewError {
    /// Could not bind loopback.
    #[error("guest view bind: {0}")]
    Bind(String),
    /// Listener came up on a non-loopback address.
    #[error("guest view refused bind-all ({0})")]
    BindAll(SocketAddr),
}

#[derive(Clone)]
struct ViewState {
    node: NodeState,
    lease_id: LeaseId,
}

#[derive(Debug, Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ActionBody {
    op: String,
    x: Option<i32>,
    y: Option<i32>,
    button: Option<String>,
    text: Option<String>,
    keys: Option<Vec<String>>,
}

fn view_router(state: ViewState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/vnc.html", get(novnc_index))
        .route("/screenshot", get(screenshot))
        .route("/action", post(action))
        .route("/websockify", get(websockify))
        .with_state(state)
}

async fn require_lease(
    state: &ViewState,
    headers: &HeaderMap,
    query: &TokenQuery,
) -> Result<(), ApiError> {
    let token = bearer_or_query(headers, query).ok_or_else(|| {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "missing lease bearer (Authorization: Bearer or ?token=)",
        )
    })?;
    let inner = state.node.lock().await;
    inner
        .pairing
        .authorize(&token, Capability::Lease)
        .map_err(ApiError::from)?;
    match inner.live.as_ref() {
        Some(live) if live.lease.id == state.lease_id => Ok(()),
        _ => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "no live lease for this view",
        )),
    }
}

fn bearer_or_query(headers: &HeaderMap, query: &TokenQuery) -> Option<String> {
    if let Some(raw) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(token) = raw.strip_prefix("Bearer ") {
            return Some(token.to_string());
        }
    }
    query
        .token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
}

async fn index(
    State(state): State<ViewState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
) -> Result<Html<&'static str>, ApiError> {
    require_lease(&state, &headers, &query).await?;
    Ok(Html(GUEST_VIEW_HTML))
}

async fn novnc_index(
    State(state): State<ViewState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
) -> Result<Response, ApiError> {
    require_lease(&state, &headers, &query).await?;
    match guest_file(&state, "/usr/share/novnc/vnc.html").await {
        Ok(bytes) if !bytes.is_empty() => Ok(Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Body::from(bytes))
            .expect("response")),
        _ => Ok(Html(GUEST_VIEW_HTML).into_response()),
    }
}

async fn screenshot(
    State(state): State<ViewState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
) -> Result<Response, ApiError> {
    require_lease(&state, &headers, &query).await?;
    let png = run_op(&state, GuestOp::Screenshot).await?;
    if !png.starts_with(PNG_MAGIC) {
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "guest screenshot was not a PNG",
        ));
    }
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/png")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(png))
        .expect("response"))
}

async fn action(
    State(state): State<ViewState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    Json(body): Json<ActionBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_lease(&state, &headers, &query).await?;
    let op = parse_op(&body)?;
    let _ = run_op(&state, op).await?;
    Ok(Json(serde_json::json!({ "ok": true, "target": "guest" })))
}

async fn websockify(
    State(state): State<ViewState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    require_lease(&state, &headers, &query).await?;
    let container = {
        let inner = state.node.lock().await;
        inner
            .live
            .as_ref()
            .filter(|l| l.lease.id == state.lease_id)
            .map(|l| l.handle.container.clone())
            .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no live lease for this view"))?
    };
    Ok(ws.on_upgrade(move |socket| proxy_guest_vnc(socket, container)))
}

async fn proxy_guest_vnc(mut socket: WebSocket, container: String) {
    // Pipe browser RFB through docker exec into the guest's localhost x11vnc.
    // The guest has --network none; this is the only path to its :5900.
    let child = tokio::process::Command::new("docker")
        .args([
            "exec",
            "-i",
            &container,
            "socat",
            "STDIO",
            "TCP:127.0.0.1:5900",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(_) => {
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };
    let mut stdin = match child.stdin.take() {
        Some(s) => s,
        None => return,
    };
    let mut stdout = match child.stdout.take() {
        Some(s) => s,
        None => return,
    };

    let (mut sink, mut stream) = socket.split();
    let to_guest = async {
        while let Some(Ok(msg)) = stream.next().await {
            match msg {
                Message::Binary(data) => {
                    if stdin.write_all(&data).await.is_err() {
                        break;
                    }
                }
                Message::Text(text) => {
                    if stdin.write_all(text.as_bytes()).await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    };
    let to_browser = async {
        let mut buf = vec![0u8; 8192];
        loop {
            match stdout.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if sink
                        .send(Message::Binary(buf[..n].to_vec().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    };
    tokio::select! {
        _ = to_guest => {}
        _ = to_browser => {}
    }
    let _ = child.kill().await;
}

async fn run_op(state: &ViewState, op: GuestOp) -> Result<Vec<u8>, ApiError> {
    let argv =
        action_argv(&op).map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
    if argv_targets_host(&argv) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "refusing to target the host display",
        ));
    }
    let (guests, handle) = {
        let inner = state.node.lock().await;
        let live = inner
            .live
            .as_ref()
            .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no live lease for this view"))?;
        if live.lease.id != state.lease_id {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "no live lease for this view",
            ));
        }
        (Arc::clone(&inner.guests), live.handle.clone())
    };
    tokio::task::spawn_blocking(move || guests.exec(&handle, &argv))
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, e.to_string()))
}

async fn guest_file(state: &ViewState, path: &str) -> Result<Vec<u8>, ApiError> {
    let argv = vec!["cat".into(), path.to_string()];
    let (guests, handle) = {
        let inner = state.node.lock().await;
        let live = inner
            .live
            .as_ref()
            .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no live lease"))?;
        (Arc::clone(&inner.guests), live.handle.clone())
    };
    // cat is not the driver; only used to serve guest noVNC assets.
    // Refuse anything that looks like a host display path.
    if path.to_ascii_lowercase().contains("x11") || path.contains("wayland") {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "refused"));
    }
    tokio::task::spawn_blocking(move || guests.exec(&handle, &argv))
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, e.to_string()))
}

fn parse_op(body: &ActionBody) -> Result<GuestOp, ApiError> {
    match body.op.as_str() {
        "click" => {
            let x = body
                .x
                .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "x is required"))?;
            let y = body
                .y
                .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "y is required"))?;
            let button = parse_button(body.button.as_deref())
                .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
            Ok(GuestOp::Click { x, y, button })
        }
        "type" => {
            let text = body
                .text
                .clone()
                .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "text is required"))?;
            Ok(GuestOp::Type { text })
        }
        "key" => {
            let keys = body
                .keys
                .clone()
                .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "keys is required"))?;
            Ok(GuestOp::Key { keys })
        }
        "screenshot" => Ok(GuestOp::Screenshot),
        other => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("unknown op `{other}`"),
        )),
    }
}

/// True when `addr` is a loopback viewer bind.
pub fn view_bind_is_loopback(addr: SocketAddr) -> bool {
    addr.ip().is_loopback()
}

const GUEST_VIEW_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Berthos guest desktop</title>
<style>
  html, body { margin: 0; background: #1a1d23; color: #eceff4; font: 14px/1.4 system-ui, sans-serif; }
  header { padding: 10px 16px; background: #2e3440; display: flex; gap: 16px; align-items: center; }
  header strong { color: #88c0d0; }
  header span { opacity: 0.8; }
  #desk { display: block; margin: 12px auto; background: #000; cursor: crosshair; max-width: 100%; }
  .warn { color: #ebcb8b; }
</style>
</head>
<body>
<header>
  <strong>GUEST desktop</strong>
  <span>isolated Linux Xvfb — not the host cursor, not the host DISPLAY</span>
  <a class="warn" href="vnc.html">noVNC</a>
</header>
<img id="desk" width="1280" height="800" alt="guest screenshot" />
<script>
const img = document.getElementById('desk');
const token = new URLSearchParams(location.search).get('token') || '';
const q = token ? ('?token=' + encodeURIComponent(token)) : '';
function headers() {
  const h = {};
  if (token) h['Authorization'] = 'Bearer ' + token;
  return h;
}
function refresh() {
  img.src = 'screenshot' + q + '&t=' + Date.now();
}
img.addEventListener('click', (ev) => {
  const r = img.getBoundingClientRect();
  const x = Math.round((ev.clientX - r.left) * (img.naturalWidth || 1280) / r.width);
  const y = Math.round((ev.clientY - r.top) * (img.naturalHeight || 800) / r.height);
  fetch('action' + q, {
    method: 'POST',
    headers: Object.assign({'content-type': 'application/json'}, headers()),
    body: JSON.stringify({ op: 'click', x, y, button: ev.button === 2 ? 'right' : 'left' })
  });
});
window.addEventListener('keydown', (ev) => {
  if (ev.target !== document.body && ev.target !== document.documentElement) return;
  ev.preventDefault();
  if (ev.key.length === 1 && !ev.ctrlKey && !ev.metaKey && !ev.altKey) {
    fetch('action' + q, {
      method: 'POST',
      headers: Object.assign({'content-type': 'application/json'}, headers()),
      body: JSON.stringify({ op: 'type', text: ev.key })
    });
    return;
  }
  const keys = [];
  if (ev.ctrlKey) keys.push('ctrl');
  if (ev.metaKey) keys.push('meta');
  if (ev.altKey) keys.push('alt');
  if (ev.shiftKey && ev.key.length > 1) keys.push('shift');
  keys.push(ev.key);
  fetch('action' + q, {
    method: 'POST',
    headers: Object.assign({'content-type': 'application/json'}, headers()),
    body: JSON.stringify({ op: 'key', keys })
  });
});
refresh();
setInterval(refresh, 800);
</script>
</body>
</html>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_predicate() {
        let loopback: SocketAddr = "127.0.0.1:9".parse().unwrap();
        assert!(view_bind_is_loopback(loopback));
        let all: SocketAddr = "0.0.0.0:6080".parse().unwrap();
        assert!(!view_bind_is_loopback(all));
    }

    #[test]
    fn token_from_header_or_query() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer abc".parse().unwrap());
        assert_eq!(
            bearer_or_query(&headers, &TokenQuery { token: None }).as_deref(),
            Some("abc")
        );
        assert_eq!(
            bearer_or_query(
                &HeaderMap::new(),
                &TokenQuery {
                    token: Some("xyz".into())
                }
            )
            .as_deref(),
            Some("xyz")
        );
        assert!(bearer_or_query(&HeaderMap::new(), &TokenQuery { token: None }).is_none());
    }
}
