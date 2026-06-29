//! HTTP routing: the OpenAPI 3.1.0 REST surface, the SSE event stream, and the
//! static UI. `/api/*` is token-protected; the UI and the OpenAPI document are
//! public (they carry no secrets).

use crate::{catalog, AppState};
use axum::{
    extract::{Path, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::sse::{Event as SseEvent, KeepAlive, Sse},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures::{Stream, StreamExt};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::convert::Infallible;
use tower_http::services::ServeDir;
use uuid::Uuid;

/// Build the complete router.
pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/runs", post(create_run).get(list_runs))
        .route("/runs/:id", get(get_run))
        .route("/runs/:id/pause", post(pause_run))
        .route("/runs/:id/resume", post(resume_run))
        .route("/runs/:id/retry", post(retry_run))
        .route("/runs/:id/destroy", post(destroy_run))
        .route("/runs/:id/inputs", post(submit_inputs))
        .route("/runs/:id/events", get(events))
        .route("/catalog/hyperscalers", get(get_hyperscalers))
        .route("/catalog/services", get(get_services))
        .route("/modules", get(get_modules))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth));

    let api = Router::new()
        .route("/openapi.yaml", get(openapi))
        .merge(protected)
        .with_state(state.clone());

    let ui = ServeDir::new(&state.ui_dir).append_index_html_on_directories(true);

    Router::new().nest("/api", api).fallback_service(ui)
}

// ---- auth -----------------------------------------------------------------

async fn auth(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if token_from_request(&req).as_deref() == Some(state.token.as_str()) {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, "missing or invalid token").into_response()
    }
}

/// Extract the session token from the `x-wxd-token` header or a `token=` query
/// param (the latter so the header-less `EventSource` can authenticate).
fn token_from_request(req: &Request) -> Option<String> {
    if let Some(v) = req.headers().get("x-wxd-token").and_then(|v| v.to_str().ok()) {
        return Some(v.to_string());
    }
    req.uri().query().and_then(|q| {
        q.split('&').find_map(|kv| {
            let mut it = kv.splitn(2, '=');
            match (it.next(), it.next()) {
                (Some("token"), Some(val)) => Some(val.to_string()),
                _ => None,
            }
        })
    })
}

// ---- helpers --------------------------------------------------------------

fn err500(e: impl std::fmt::Display) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
}

// ---- run handlers ---------------------------------------------------------

async fn create_run(State(state): State<AppState>) -> Response {
    let id = Uuid::new_v4().to_string();
    match state.orch.create_run(id.clone()) {
        Ok(run) => {
            // Drive the run in the background; the UI watches progress via SSE.
            let orch = state.orch.clone();
            tokio::spawn(async move {
                if let Ok(mut st) = orch.store().load(&id) {
                    let _ = orch.drive(&mut st).await;
                }
            });
            (StatusCode::CREATED, Json(run)).into_response()
        }
        Err(e) => err500(e),
    }
}

async fn list_runs(State(state): State<AppState>) -> Response {
    match state.orch.store().list() {
        Ok(ids) => {
            let runs: Vec<_> = ids
                .iter()
                .filter_map(|id| state.orch.store().load(id).ok())
                .collect();
            Json(runs).into_response()
        }
        Err(e) => err500(e),
    }
}

async fn get_run(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.orch.store().load(&id) {
        Ok(run) => Json(run).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "no such run").into_response(),
    }
}

async fn pause_run(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    state.orch.pause(&id);
    StatusCode::ACCEPTED.into_response()
}

async fn resume_run(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let orch = state.orch.clone();
    tokio::spawn(async move {
        let _ = orch.resume(&id).await;
    });
    StatusCode::ACCEPTED.into_response()
}

async fn retry_run(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let orch = state.orch.clone();
    tokio::spawn(async move {
        let _ = orch.retry(&id).await;
    });
    StatusCode::ACCEPTED.into_response()
}

/// Tear down the provisioned cluster (best-effort, runs `openshift-install
/// destroy cluster`). Important for paid clouds so resources aren't orphaned.
async fn destroy_run(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    use sw_mod_provision::{AwsProvisioner, Provisioner};
    let orch = state.orch.clone();
    tokio::spawn(async move {
        let artifacts = orch.store().artifacts_dir(&id);
        let ctx = sw_core::StepContext::with_artifacts(
            id.clone(),
            "mod-provision/destroy".to_string(),
            orch.command_runner(),
            orch.bus().clone(),
            BTreeMap::new(),
            BTreeMap::new(),
            artifacts,
        );
        let _ = AwsProvisioner::new().destroy(&ctx).await;
    });
    StatusCode::ACCEPTED.into_response()
}

#[derive(Debug, Default, Deserialize)]
struct InputsBody {
    #[serde(default)]
    values: BTreeMap<String, String>,
    #[serde(default)]
    secrets: BTreeMap<String, String>,
}

async fn submit_inputs(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<InputsBody>,
) -> Response {
    let orch = state.orch.clone();
    tokio::spawn(async move {
        let _ = orch.submit_inputs(&id, body.values, body.secrets).await;
    });
    StatusCode::ACCEPTED.into_response()
}

// ---- SSE ------------------------------------------------------------------

async fn events(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    // Replay this run's history first, then stream live events. (v1 drives one
    // run at a time, so the live broadcast is effectively per-run.)
    let history = state.orch.store().replay_events(&id).unwrap_or_default();
    let rx = state.orch.bus().subscribe();

    let hist = futures::stream::iter(history.into_iter());
    let live = futures::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(ev) => return Some((ev, rx)),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });

    let stream = hist.chain(live).map(|ev| {
        let sse = SseEvent::default()
            .json_data(&ev)
            .unwrap_or_else(|_| SseEvent::default().data("{}"));
        Ok(sse)
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ---- catalog + modules ----------------------------------------------------

async fn get_hyperscalers() -> Response {
    Json(catalog::hyperscalers()).into_response()
}

async fn get_services() -> Response {
    Json(catalog::services()).into_response()
}

async fn get_modules(State(state): State<AppState>) -> Response {
    Json(state.orch.registry().views()).into_response()
}

async fn openapi() -> Response {
    (
        [(header::CONTENT_TYPE, "application/yaml")],
        include_str!("../openapi.yaml"),
    )
        .into_response()
}
