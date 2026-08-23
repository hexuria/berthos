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

use crate::eligibility::evaluate;
use crate::guest::{GuestError, GuestHandle, GuestRuntime, GuestSpec};
use crate::pairing::{Capability, PairError, PairingBooth};
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
    };
    inner.live = Some(LiveLease {
        lease: lease.clone(),
        handle,
    });
    Ok((StatusCode::CREATED, Json(lease)))
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

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
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

    #[tokio::test]
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
}
