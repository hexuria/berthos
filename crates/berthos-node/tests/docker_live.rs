//! Live Docker probes and isolated leases.
//!
//! Skips when the daemon is missing (unit CI). When the labeled
//! `berthos-linux-desktop:v1` image is present, starts a guest with
//! `--network none`, asserts no host display, then destroy-and-recreates.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use berthos_node::guest::{inspect_isolation, DockerGuest, GuestRuntime, GuestSpec};
use berthos_node::http::{new_state_with_report, router};
use berthos_node::probes::{docker_available, labeled_guest_image_ready, observe};
use berthos_node::{evaluate, GuestHandle};
use berthos_protocol::{LeaseId, REQUIRED_GUEST_IMAGE};
use http_body_util::BodyExt;
use tower::ServiceExt;

fn skip_without_docker() -> bool {
    if docker_available() {
        return false;
    }
    eprintln!("skip live docker test: daemon not available");
    true
}

fn skip_without_labeled_image() -> bool {
    if labeled_guest_image_ready(REQUIRED_GUEST_IMAGE) {
        return false;
    }
    eprintln!("skip live guest: {REQUIRED_GUEST_IMAGE} with v1 labels is not present");
    true
}

#[test]
fn live_observe_probes_daemon() {
    if skip_without_docker() {
        return;
    }
    let facts = observe(&berthos_node::probes::default_facts_config());
    assert!(facts.runtime_running);
    if facts.guest_image.is_none() {
        let report = evaluate(&facts);
        assert!(!report.ok);
        assert!(report.failed(berthos_protocol::CheckId::GuestImage));
    }
}

#[test]
fn live_guest_is_isolated_and_destroyed() {
    if skip_without_docker() || skip_without_labeled_image() {
        return;
    }
    let runtime = DockerGuest;
    let id = LeaseId::generate();
    let spec = GuestSpec {
        image: REQUIRED_GUEST_IMAGE.to_string(),
        vcpu: 1,
        mem_gib: 1,
    };
    let handle = runtime.start(&id, &spec).expect("start isolated guest");
    let isolation = inspect_isolation(&handle).expect("inspect");
    assert_eq!(isolation.network_mode, "none");
    assert!(
        isolation.is_isolated(),
        "guest must not see host net/display/cursor: {isolation:?}"
    );
    runtime.destroy(&handle).expect("destroy");
    assert!(
        inspect_isolation(&handle).is_err(),
        "destroy must remove the container"
    );

    let again = runtime
        .start(&LeaseId::generate(), &spec)
        .expect("destroy-and-recreate");
    assert!(inspect_isolation(&again).expect("recreate").is_isolated());
    runtime.destroy(&again).expect("destroy recreate");
}

#[tokio::test]
async fn live_http_lease_returns_occupancy_receipt() {
    if skip_without_docker() || skip_without_labeled_image() {
        return;
    }
    let config = berthos_node::probes::default_facts_config();
    let report = evaluate(&observe(&config));
    if !report.ok {
        eprintln!("skip live HTTP lease: doctor not green: {report:?}");
        return;
    }
    let state = new_state_with_report(config, report, Arc::new(DockerGuest));
    let token = pair_token(&state).await;

    let req = Request::builder()
        .method("POST")
        .uri("/v1/leases")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "os": "linux", "vcpu": 1, "mem_gib": 1 }).to_string(),
        ))
        .unwrap();
    let (status, body) = oneshot(state.clone(), req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().expect("lease id").to_string();
    let handle = GuestHandle {
        container: format!("berthos-{id}"),
    };
    let isolation = inspect_isolation(&handle).expect("lease container");
    assert_eq!(isolation.network_mode, "none");
    assert!(isolation.is_isolated(), "{isolation:?}");

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
    assert!(
        inspect_isolation(&handle).is_err(),
        "DELETE must destroy the guest"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn live_view_and_screenshot_die_with_lease() {
    if skip_without_docker() || skip_without_labeled_image() {
        return;
    }
    let config = berthos_node::probes::default_facts_config();
    let report = evaluate(&observe(&config));
    if !report.ok {
        eprintln!("skip live view: doctor not green: {report:?}");
        return;
    }
    let state = new_state_with_report(config, report, Arc::new(DockerGuest));
    let token = pair_token(&state).await;

    let req = Request::builder()
        .method("POST")
        .uri("/v1/leases")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "os": "linux", "vcpu": 1, "mem_gib": 1 }).to_string(),
        ))
        .unwrap();
    let (status, body) = oneshot(state.clone(), req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().expect("lease id").to_string();
    let viewer = body["viewer_url"].as_str().expect("viewer_url").to_string();
    assert!(
        viewer.starts_with("http://127.0.0.1:"),
        "view must be loopback: {viewer}"
    );

    // Guest Xvfb may take a moment after docker run.
    let mut png = Vec::new();
    for _ in 0..40 {
        let req = Request::builder()
            .uri(format!("/v1/leases/{id}/screenshot"))
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let resp = router(state.clone()).oneshot(req).await.expect("router");
        if resp.status() == StatusCode::OK {
            png = resp
                .into_body()
                .collect()
                .await
                .expect("body")
                .to_bytes()
                .to_vec();
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    assert!(
        png.starts_with(b"\x89PNG\r\n\x1a\n"),
        "live guest screenshot must be a PNG ({} bytes)",
        png.len()
    );

    let html = ureq::get(&viewer)
        .set("authorization", &format!("Bearer {token}"))
        .timeout(std::time::Duration::from_secs(5))
        .call()
        .expect("view html");
    assert_eq!(html.status(), 200);
    let page = html.into_string().unwrap_or_default();
    assert!(
        page.contains("GUEST") || page.contains("noVNC") || page.contains("html"),
        "{page}"
    );

    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/v1/leases/{id}"))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let (status, _) = oneshot(state.clone(), req).await;
    assert_eq!(status, StatusCode::OK);

    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    let gone = ureq::get(&viewer)
        .set("authorization", &format!("Bearer {token}"))
        .timeout(std::time::Duration::from_secs(1))
        .call();
    assert!(gone.is_err(), "loopback view must die with the lease");

    let req = Request::builder()
        .uri(format!("/v1/leases/{id}/screenshot"))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let (status, _) = oneshot(state, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

async fn oneshot(
    state: berthos_node::NodeState,
    req: Request<Body>,
) -> (StatusCode, serde_json::Value) {
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

async fn pair_token(state: &berthos_node::NodeState) -> String {
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
