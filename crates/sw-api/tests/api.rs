//! HTTP-level integration tests for the installer API, driven through the router
//! with `tower::ServiceExt::oneshot` (no real socket). Hermetic: the
//! orchestrator uses a `MockCommandRunner` and a temp-dir run store.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use sw_core::{EventBus, MockCommandRunner, Orchestrator, RunStore};
use tower::ServiceExt;

const TOKEN: &str = "test-token";

fn temp_orch() -> Orchestrator {
    static N: AtomicU64 = AtomicU64::new(1);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir()
        .join(format!("wxd-api-{}", std::process::id()))
        .join(format!("t{n}"));
    Orchestrator::with_registries(
        EventBus::new(),
        RunStore::new(dir),
        Arc::new(MockCommandRunner::new(vec![])),
        sw_api::registries(),
        "provision",
    )
}

fn app() -> axum::Router {
    let ui_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/ui");
    sw_api::app(temp_orch(), TOKEN, ui_dir)
}

async fn body_string(resp: axum::response::Response) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn openapi_is_public() {
    let resp = app()
        .oneshot(
            Request::builder()
                .uri("/api/openapi.yaml")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("openapi: 3.1.0"));
}

#[tokio::test]
async fn protected_route_rejects_missing_token() {
    let resp = app()
        .oneshot(
            Request::builder()
                .uri("/api/catalog/hyperscalers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn protected_route_accepts_header_token() {
    let resp = app()
        .oneshot(
            Request::builder()
                .uri("/api/catalog/hyperscalers")
                .header("x-wxd-token", TOKEN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("\"aws\""));
}

#[tokio::test]
async fn protected_route_accepts_query_token() {
    let resp = app()
        .oneshot(
            Request::builder()
                .uri(format!("/api/modules?token={TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("preflight"));
}

#[tokio::test]
async fn create_run_then_fetch_it() {
    let app = app();
    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/runs")
                .header("x-wxd-token", TOKEN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::CREATED);
    let created = body_string(create).await;
    let run: serde_json::Value = serde_json::from_str(&created).unwrap();
    let id = run["id"].as_str().unwrap().to_string();
    assert!(run["steps"].as_array().unwrap().len() >= 3);

    let get = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/runs/{id}"))
                .header("x-wxd-token", TOKEN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::OK);
    let fetched = body_string(get).await;
    assert!(fetched.contains(&id));
}

#[tokio::test]
async fn create_run_in_existing_mode_uses_existing_graph() {
    let create = app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/runs")
                .header("x-wxd-token", TOKEN)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"mode":"existing"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::CREATED);
    let run: serde_json::Value = serde_json::from_str(&body_string(create).await).unwrap();
    assert_eq!(run["mode"], "existing");
    // Existing-cluster graph starts with adopting a kubeconfig, not provisioning.
    let first = run["steps"][0]["id"].as_str().unwrap();
    assert_eq!(first, "mod-existing/provide-kubeconfig");
}

#[tokio::test]
async fn modules_can_be_queried_per_mode() {
    let resp = app()
        .oneshot(
            Request::builder()
                .uri(format!("/api/modules?mode=existing&token={TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("mod-existing"));
    assert!(!body.contains("mod-provision"));
}

#[tokio::test]
async fn modes_endpoint_lists_both() {
    let resp = app()
        .oneshot(
            Request::builder()
                .uri(format!("/api/catalog/modes?token={TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("provision") && body.contains("existing"));
}

#[tokio::test]
async fn unknown_run_is_404() {
    let resp = app()
        .oneshot(
            Request::builder()
                .uri("/api/runs/does-not-exist")
                .header("x-wxd-token", TOKEN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn ui_index_is_served_at_root() {
    let resp = app()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("watsonx.data Easy Installer"));
}
