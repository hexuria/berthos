//! Loopback-only Axum HTTP API.
//!
//! Remote access, if any, is a later optional tunnel in front of this
//! listener. Bind-all is rejected at serve time. `X-Forwarded-*` is ignored
//! when deciding loopback.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use berthos_protocol::{
    CreateLeaseRequest, DoctorReport, EndReason, GuestOs, Lease, LeaseId, LeaseState, Quote,
    Receipt, DEFAULT_DISK_GIB, DEFAULT_MEM_GIB, DEFAULT_VCPU, PROTOCOL_VERSION,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use crate::action::{action_argv, argv_targets_host, parse_button, GuestOp, PNG_MAGIC};
use crate::eligibility::evaluate;
use crate::guest::{GuestError, GuestHandle, GuestRuntime, GuestSpec};
use crate::pairing::{Capability, PairError, PairingBooth};
use crate::view::LoopbackView;
use crate::{NodeConfig, NodeError};

/// Shared node process state.
pub struct NodeInner {
    /// Advertisement + bind.
    pub config: NodeConfig,
    /// Last doctor report. Re-evaluated at start; served as-is unless live.
    pub report: DoctorReport,
    /// Parked nodes accept new leases.
    pub parked: bool,
    /// Pairing booth.
    pub pairing: PairingBooth,
    /// At most one live lease in v1.
    pub live: Option<LiveLease>,
    /// Guest runtime (Docker or memory).
    pub guests: Arc<dyn GuestRuntime>,
    /// When true, `GET /v1/eligibility` re-runs live probes (production).
    /// Tests that inject a report leave this false so the fixture is stable.
    pub live_eligibility: bool,
}

/// A live lease plus its guest handle.
pub struct LiveLease {
    /// Protocol lease record.
    pub lease: Lease,
    /// Runtime handle for destroy-on-end.
    pub handle: GuestHandle,
    /// Loopback-only guest view. Dropped when the lease ends.
    pub view: Option<LoopbackView>,
}

/// Shared state wrapper.
pub type NodeState = Arc<Mutex<NodeInner>>;

/// JSON error body.
#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

/// `POST /v1/pair` body.
#[derive(Debug, Deserialize)]
struct PairRequest {
    code: String,
}

/// `POST /v1/pair` response.
#[derive(Debug, Serialize)]
struct PairResponse {
    token: String,
    capabilities: &'static [&'static str],
}

/// `GET /v1/pairing` response (loopback only).
#[derive(Debug, Serialize)]
struct PairingReveal {
    code: String,
}

/// `GET /v1/node` response.
#[derive(Debug, Serialize)]
struct NodeStatus {
    protocol: &'static str,
    parked: bool,
    eligible: bool,
    bind: String,
    class: berthos_protocol::NodeClass,
    live_lease: Option<LeaseId>,
    settlement: &'static str,
}

/// Build the Axum router.
pub fn router(state: NodeState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/eligibility", get(eligibility))
        .route("/v1/node", get(node_status))
        .route("/v1/park", post(park))
        .route("/v1/unpark", post(unpark))
        .route("/v1/pairing", get(reveal_pairing))
        .route("/v1/pair", post(pair))
        .route("/v1/leases", post(create_lease).get(list_leases))
        .route("/v1/leases/{id}", delete(end_lease).get(get_lease))
        .route("/v1/leases/{id}/screenshot", get(lease_screenshot))
        .route("/v1/leases/{id}/actions", post(lease_action))
        .route("/v1/leases/{id}/view", get(lease_view_info))
        .with_state(state)
}

/// Bind loopback and serve. Non-loopback addresses are refused before listen.
pub async fn serve(state: NodeState, bind: SocketAddr) -> Result<(), NodeError> {
    if !bind.ip().is_loopback() {
        return Err(NodeError::BindAllRejected(bind.ip()));
    }
    let listener = TcpListener::bind(bind).await?;
    let actual = listener.local_addr()?;
    if !actual.ip().is_loopback() {
        return Err(NodeError::BindAllRejected(actual.ip()));
    }
    tracing::info!("listening on {actual}");
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({
        "ok": true,
        "protocol": PROTOCOL_VERSION,
        "settlement": "not this repo",
    }))
}

async fn eligibility(State(state): State<NodeState>) -> Json<DoctorReport> {
    let mut inner = state.lock().await;
    if inner.live_eligibility {
        inner.report = evaluate(&crate::probes::observe(&inner.config));
    }
    Json(inner.report.clone())
}

async fn node_status(State(state): State<NodeState>) -> Json<NodeStatus> {
    let inner = state.lock().await;
    Json(NodeStatus {
        protocol: PROTOCOL_VERSION,
        parked: inner.parked,
        eligible: inner.report.eligible,
        bind: format!("{}:{}", inner.config.bind_ip, inner.config.port),
        class: inner.config.class,
        live_lease: inner.live.as_ref().map(|l| l.lease.id.clone()),
        settlement: "quoted, not charged — https://github.com/hexuria/berth-market",
    })
}

async fn park(State(state): State<NodeState>, headers: HeaderMap) -> Result<StatusCode, ApiError> {
    let mut inner = state.lock().await;
    require_token(&inner, &headers, Capability::Operator)?;
    if !inner.report.eligible {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "node is ineligible; run berth doctor",
        ));
    }
    inner.parked = true;
    Ok(StatusCode::NO_CONTENT)
}

async fn unpark(
    State(state): State<NodeState>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let mut inner = state.lock().await;
    require_token(&inner, &headers, Capability::Operator)?;
    if inner.live.is_some() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "cannot unpark while a lease is live",
        ));
    }
    inner.parked = false;
    Ok(StatusCode::NO_CONTENT)
}

async fn reveal_pairing(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<NodeState>,
) -> Result<Json<PairingReveal>, ApiError> {
    // Ignore forwarded headers. Only the real peer address counts.
    let _ = headers.get("x-forwarded-for");
    let _ = headers.get("x-forwarded-proto");
    let _ = headers.get("x-real-ip");
    if !addr.ip().is_loopback() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "not found"));
    }
    let inner = state.lock().await;
    Ok(Json(PairingReveal {
        code: inner.pairing.code.clone(),
    }))
}

async fn pair(
    State(state): State<NodeState>,
    Json(body): Json<PairRequest>,
) -> Result<Json<PairResponse>, ApiError> {
    let mut inner = state.lock().await;
    let token = inner.pairing.pair(&body.code).map_err(ApiError::from)?;
    Ok(Json(PairResponse {
        token,
        capabilities: &["operator", "lease"],
    }))
}

async fn create_lease(
    State(state): State<NodeState>,
    headers: HeaderMap,
    Json(body): Json<CreateLeaseRequest>,
) -> Result<(StatusCode, Json<Lease>), ApiError> {
    let mut inner = state.lock().await;
    require_token(&inner, &headers, Capability::Lease)?;
    if !inner.report.eligible {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "node is ineligible; run berth doctor",
        ));
    }
    if !inner.parked {
        return Err(ApiError::new(StatusCode::CONFLICT, "node is unparked"));
    }
    if inner.live.is_some() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "a lease is already live",
        ));
    }
    if body.os != GuestOs::Linux {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "v1 leases only os=linux; windows and macos are out of scope",
        ));
    }
    let vcpu = body.vcpu.unwrap_or(DEFAULT_VCPU);
    let mem_gib = body.mem_gib.unwrap_or(DEFAULT_MEM_GIB);
    let disk_gib = body.disk_gib.unwrap_or(DEFAULT_DISK_GIB);
    let quote = Quote::linux_isolated(vcpu, mem_gib, disk_gib)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;

    let id = LeaseId::generate();
    let spec = GuestSpec {
        image: inner.config.image.clone(),
        vcpu: quote.vcpu,
        mem_gib: quote.mem_gib,
    };
    let handle = inner
        .guests
        .start(&id, &spec)
        .map_err(|e: GuestError| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, e.to_string()))?;

    let lease = Lease {
        id: id.clone(),
        state: LeaseState::Live,
        quote,
        started_at: OffsetDateTime::now_utc(),
        ended_at: None,
        viewer_url: None,
    };
    inner.live = Some(LiveLease {
        lease: lease.clone(),
        handle,
        view: None,
    });
    drop(inner);

    let view = match LoopbackView::start(state.clone(), id.clone()).await {
        Ok(view) => view,
        Err(e) => {
            let mut inner = state.lock().await;
            if let Some(live) = inner.live.take() {
                if live.lease.id == id {
                    let _ = inner.guests.destroy(&live.handle);
                } else {
                    inner.live = Some(live);
                }
            }
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                e.to_string(),
            ));
        }
    };

    let mut inner = state.lock().await;
    let Some(live) = inner.live.as_mut() else {
        let mut view = view;
        view.stop();
        return Err(ApiError::new(StatusCode::NOT_FOUND, "lease not found"));
    };
    if live.lease.id != id {
        let mut view = view;
        view.stop();
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "a lease is already live",
        ));
    }
    live.lease.viewer_url = Some(view.url());
    live.view = Some(view);
    Ok((StatusCode::CREATED, Json(live.lease.clone())))
}

async fn list_leases(
    State(state): State<NodeState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Lease>>, ApiError> {
    let inner = state.lock().await;
    require_token(&inner, &headers, Capability::Lease)?;
    let list = inner
        .live
        .as_ref()
        .map(|l| vec![l.lease.clone()])
        .unwrap_or_default();
    Ok(Json(list))
}

async fn get_lease(
    State(state): State<NodeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Lease>, ApiError> {
    let inner = state.lock().await;
    require_token(&inner, &headers, Capability::Lease)?;
    match inner.live.as_ref() {
        Some(live) if live.lease.id.0 == id => Ok(Json(live.lease.clone())),
        _ => Err(ApiError::new(StatusCode::NOT_FOUND, "lease not found")),
    }
}

async fn end_lease(
    State(state): State<NodeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Receipt>, ApiError> {
    let mut inner = state.lock().await;
    require_token(&inner, &headers, Capability::Lease)?;
    let live = inner
        .live
        .take()
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "lease not found"))?;
    if live.lease.id.0 != id {
        inner.live = Some(live);
        return Err(ApiError::new(StatusCode::NOT_FOUND, "lease not found"));
    }
    if let Some(mut view) = live.view {
        view.stop();
    }
    if let Err(e) = inner.guests.destroy(&live.handle) {
        tracing::warn!("guest destroy: {e}");
    }
    let ended_at = OffsetDateTime::now_utc();
    Ok(Json(Receipt::from_lease(
        &live.lease,
        ended_at,
        EndReason::Graceful,
    )))
}

fn require_token(inner: &NodeInner, headers: &HeaderMap, need: Capability) -> Result<(), ApiError> {
    let header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "missing bearer token"))?;
    let token = header
        .strip_prefix("Bearer ")
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "missing bearer token"))?;
    inner.pairing.authorize(token, need).map_err(ApiError::from)
}

/// JSON body for `POST /v1/leases/{id}/actions`.
#[derive(Debug, Deserialize)]
struct ActionBody {
    op: String,
    x: Option<i32>,
    y: Option<i32>,
    button: Option<String>,
    text: Option<String>,
    keys: Option<Vec<String>>,
}

/// `GET /v1/leases/{id}/view` — pairing token + loopback URL for this lease.
#[derive(Debug, Serialize)]
struct ViewInfo {
    viewer_url: String,
    target: &'static str,
    token: &'static str,
}

async fn lease_view_info(
    State(state): State<NodeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ViewInfo>, ApiError> {
    let inner = state.lock().await;
    require_token(&inner, &headers, Capability::Lease)?;
    match inner.live.as_ref() {
        Some(live) if live.lease.id.0 == id => {
            let viewer_url =
                live.lease.viewer_url.clone().ok_or_else(|| {
                    ApiError::new(StatusCode::NOT_FOUND, "lease has no guest view")
                })?;
            Ok(Json(ViewInfo {
                viewer_url,
                target: "guest",
                token: "Authorization: Bearer <lease token from POST /v1/pair>",
            }))
        }
        _ => Err(ApiError::new(StatusCode::NOT_FOUND, "lease not found")),
    }
}

async fn lease_screenshot(
    State(state): State<NodeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let png = run_live_op(&state, &headers, &id, GuestOp::Screenshot).await?;
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
        .body(axum::body::Body::from(png))
        .expect("response"))
}

async fn lease_action(
    State(state): State<NodeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ActionBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let op = parse_action_body(&body)?;
    let _ = run_live_op(&state, &headers, &id, op).await?;
    Ok(Json(serde_json::json!({ "ok": true, "target": "guest" })))
}

fn parse_action_body(body: &ActionBody) -> Result<GuestOp, ApiError> {
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

async fn run_live_op(
    state: &NodeState,
    headers: &HeaderMap,
    id: &str,
    op: GuestOp,
) -> Result<Vec<u8>, ApiError> {
    let argv =
        action_argv(&op).map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
    if argv_targets_host(&argv) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "refusing to target the host display",
        ));
    }
    let (guests, handle) = {
        let inner = state.lock().await;
        require_token(&inner, headers, Capability::Lease)?;
        let live = inner
            .live
            .as_ref()
            .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no live lease"))?;
        if live.lease.id.0 != id {
            return Err(ApiError::new(StatusCode::NOT_FOUND, "lease not found"));
        }
        (Arc::clone(&inner.guests), live.handle.clone())
    };
    tokio::task::spawn_blocking(move || guests.exec(&handle, &argv))
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, e.to_string()))
}

/// HTTP API error. Shared with the lease-scoped guest view.
pub(crate) struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl From<PairError> for ApiError {
    fn from(err: PairError) -> Self {
        match err {
            PairError::BadCode => Self::new(StatusCode::FORBIDDEN, err.to_string()),
            PairError::UnknownToken | PairError::MissingCapability(_) => {
                Self::new(StatusCode::UNAUTHORIZED, err.to_string())
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}

/// Helper used by the CLI and tests to construct a node.
pub fn new_state(config: NodeConfig, guests: Arc<dyn GuestRuntime>) -> NodeState {
    let facts = crate::probes::observe(&config);
    // Tests pass MemoryGuest and often want fixture eligibility. If live
    // probes fail (typical in CI), keep the observed report — fail closed.
    let report = evaluate(&facts);
    Arc::new(Mutex::new(NodeInner {
        config,
        report,
        parked: true,
        pairing: PairingBooth::new(),
        live: None,
        guests,
        live_eligibility: true,
    }))
}

/// Test/helper constructor that injects a doctor report instead of probing.
pub fn new_state_with_report(
    config: NodeConfig,
    report: DoctorReport,
    guests: Arc<dyn GuestRuntime>,
) -> NodeState {
    Arc::new(Mutex::new(NodeInner {
        config,
        report,
        parked: true,
        pairing: PairingBooth::new(),
        live: None,
        guests,
        live_eligibility: false,
    }))
}

/// Reject a bind IP the same way `serve` does (unit-testable).
pub fn reject_if_bind_all(ip: IpAddr) -> Result<(), NodeError> {
    if ip.is_loopback() {
        Ok(())
    } else {
        Err(NodeError::BindAllRejected(ip))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eligibility::eligible_private_facts;
    use crate::guest::MemoryGuest;
    use axum::body::Body;
    use axum::http::Request;
    use berthos_protocol::CheckId;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn eligible_state() -> NodeState {
        let config = crate::probes::default_facts_config();
        let report = evaluate(&eligible_private_facts());
        assert!(report.eligible);
        new_state_with_report(config, report, Arc::new(MemoryGuest::default()))
    }

    async fn oneshot(state: NodeState, req: Request<Body>) -> (StatusCode, serde_json::Value) {
        let resp = router(state).oneshot(req).await.expect("router");
        let status = resp.status();
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
        };
        (status, json)
    }

    async fn pair_token(state: &NodeState) -> String {
        let code = state.lock().await.pairing.code.clone();
        let req = Request::builder()
            .method("POST")
            .uri("/v1/pair")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::json!({ "code": code }).to_string()))
            .unwrap();
        let (status, body) = oneshot(state.clone(), req).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["token"].as_str().unwrap().to_string()
    }

    #[test]
    fn serve_rejects_bind_all() {
        let ip: IpAddr = "0.0.0.0".parse().unwrap();
        assert!(matches!(
            reject_if_bind_all(ip),
            Err(NodeError::BindAllRejected(_))
        ));
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(reject_if_bind_all(loopback).is_ok());
    }

    #[tokio::test]
    async fn eligibility_endpoint_reports_fail_closed_laptop() {
        let mut facts = eligible_private_facts();
        facts.class = berthos_protocol::NodeClass::Laptop;
        let report = evaluate(&facts);
        assert!(report.failed(CheckId::Class));
        let state = new_state_with_report(
            crate::probes::default_facts_config(),
            report,
            Arc::new(MemoryGuest::default()),
        );
        let req = Request::builder()
            .uri("/v1/eligibility")
            .body(Body::empty())
            .unwrap();
        let (status, body) = oneshot(state, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], false);
        assert_eq!(body["eligible"], false);
        assert_eq!(body["class"], "laptop");
        assert_eq!(body["source"], "berthos.doctor");
        assert!(body["checks"].is_array());
        assert!(body["timestamp"].as_str().is_some());
        assert!(body["image"].is_object());
    }

    #[tokio::test]
    async fn pairing_reveal_ignores_forwarded_and_needs_loopback() {
        let state = eligible_state();
        let mut req = Request::builder()
            .uri("/v1/pairing")
            .header("x-forwarded-for", "8.8.8.8")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([203, 0, 113, 9], 9))));
        let (status, _) = oneshot(state.clone(), req).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let mut req = Request::builder()
            .uri("/v1/pairing")
            .header("x-forwarded-for", "8.8.8.8")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 9))));
        let (status, body) = oneshot(state, req).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["code"].as_str().unwrap().contains('-'));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lease_create_end_records_occupancy_seconds() {
        let state = eligible_state();
        let token = pair_token(&state).await;
        let req = Request::builder()
            .method("POST")
            .uri("/v1/leases")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::json!({ "os": "linux" }).to_string()))
            .unwrap();
        let (status, body) = oneshot(state.clone(), req).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["quote"]["occupancy_unit"], "seconds");
        assert_eq!(body["quote"]["settlement"]["charged_here"], false);
        let id = body["id"].as_str().unwrap().to_string();

        let req = Request::builder()
            .method("DELETE")
            .uri(format!("/v1/leases/{id}"))
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let (status, body) = oneshot(state, req).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["occupancy_unit"], "seconds");
        assert!(body["billed_seconds"].as_u64().unwrap() >= 60);
        assert_eq!(body["settlement"]["charged_here"], false);
    }

    #[tokio::test]
    async fn unpark_blocks_new_lease() {
        let state = eligible_state();
        let token = pair_token(&state).await;
        let req = Request::builder()
            .method("POST")
            .uri("/v1/unpark")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let (status, _) = oneshot(state.clone(), req).await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let req = Request::builder()
            .method("POST")
            .uri("/v1/leases")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::json!({ "os": "linux" }).to_string()))
            .unwrap();
        let (status, body) = oneshot(state, req).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body["error"].as_str().unwrap().contains("unparked"));
    }

    #[tokio::test]
    async fn macos_lease_rejected() {
        let state = eligible_state();
        let token = pair_token(&state).await;
        let req = Request::builder()
            .method("POST")
            .uri("/v1/leases")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::json!({ "os": "macos" }).to_string()))
            .unwrap();
        let (status, _) = oneshot(state, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn windows_lease_rejected() {
        let state = eligible_state();
        let token = pair_token(&state).await;
        let req = Request::builder()
            .method("POST")
            .uri("/v1/leases")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({ "os": "windows-home-oem" }).to_string(),
            ))
            .unwrap();
        let (status, body) = oneshot(state, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }

    #[tokio::test]
    async fn no_lease_view_and_actions_fail() {
        let state = eligible_state();
        let token = pair_token(&state).await;

        let req = Request::builder()
            .uri("/v1/leases/l_missing/screenshot")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let (status, body) = oneshot(state.clone(), req).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert!(body["error"].as_str().unwrap().contains("lease"), "{body}");

        let req = Request::builder()
            .method("POST")
            .uri("/v1/leases/l_missing/actions")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({ "op": "click", "x": 1, "y": 2 }).to_string(),
            ))
            .unwrap();
        let (status, _) = oneshot(state.clone(), req).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let req = Request::builder()
            .uri("/v1/leases/l_missing/view")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let (status, _) = oneshot(state, req).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn screenshot_and_view_require_lease_bearer() {
        let state = eligible_state();
        let req = Request::builder()
            .uri("/v1/leases/l_x/screenshot")
            .body(Body::empty())
            .unwrap();
        let (status, _) = oneshot(state, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn end_lease_tears_down_loopback_view() {
        let state = eligible_state();
        let token = pair_token(&state).await;
        let req = Request::builder()
            .method("POST")
            .uri("/v1/leases")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::json!({ "os": "linux" }).to_string()))
            .unwrap();
        let (status, body) = oneshot(state.clone(), req).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let id = body["id"].as_str().unwrap().to_string();
        let viewer = body["viewer_url"].as_str().expect("viewer_url").to_string();
        assert!(
            viewer.starts_with("http://127.0.0.1:"),
            "view must be loopback: {viewer}"
        );
        assert!(!viewer.contains("0.0.0.0"), "{viewer}");

        let client = reqwest_get_ok(&viewer, &token).await;
        assert_eq!(client.0, StatusCode::OK);
        assert!(client.1.contains("GUEST desktop"), "{}", client.1);

        let req = Request::builder()
            .uri(format!("/v1/leases/{id}/screenshot"))
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let resp = router(state.clone()).oneshot(req).await.expect("router");
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        assert!(bytes.starts_with(crate::action::PNG_MAGIC), "guest PNG");

        let req = Request::builder()
            .method("DELETE")
            .uri(format!("/v1/leases/{id}"))
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let (status, _) = oneshot(state.clone(), req).await;
        assert_eq!(status, StatusCode::OK);

        // View port must die with the lease.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let gone = reqwest_get_status(&viewer, &token).await;
        assert!(
            gone.is_err() || gone == Ok(StatusCode::NOT_FOUND),
            "view must be gone after DELETE, got {gone:?}"
        );

        let req = Request::builder()
            .uri(format!("/v1/leases/{id}/screenshot"))
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let (status, _) = oneshot(state, req).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    async fn reqwest_get_ok(url: &str, token: &str) -> (StatusCode, String) {
        let client = ureq_get(url, token);
        let status = StatusCode::from_u16(client.status()).unwrap();
        let text = client.into_string().unwrap_or_default();
        (status, text)
    }

    async fn reqwest_get_status(url: &str, token: &str) -> Result<StatusCode, String> {
        match ureq::get(url)
            .set("authorization", &format!("Bearer {token}"))
            .timeout(std::time::Duration::from_secs(1))
            .call()
        {
            Ok(resp) => Ok(StatusCode::from_u16(resp.status()).unwrap()),
            Err(ureq::Error::Status(code, _)) => Ok(StatusCode::from_u16(code).unwrap()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn ureq_get(url: &str, token: &str) -> ureq::Response {
        ureq::get(url)
            .set("authorization", &format!("Bearer {token}"))
            .timeout(std::time::Duration::from_secs(2))
            .call()
            .expect("view get")
    }
}
