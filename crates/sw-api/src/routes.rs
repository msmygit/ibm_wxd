//! HTTP routing: the OpenAPI 3.1.0 REST surface, the SSE event stream, and the
//! static UI. `/api/*` is token-protected; the UI and the OpenAPI document are
//! public (they carry no secrets).

use crate::{catalog, AppState};
use axum::{
    extract::{Path, Query, Request, State},
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
        .route("/catalog/modes", get(get_modes))
        .route("/catalog/provider-spec", get(get_provider_spec))
        .route("/prereqs", get(get_prereqs))
        .route("/prereqs/install", post(install_prereqs))
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
    // No token configured → auth disabled (the server binds 127.0.0.1 only, so
    // only local processes can reach it). Set WXD_TOKEN to require a token.
    if state.token.is_empty() {
        return next.run(req).await;
    }
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

/// Seed a run's secret store from well-known credential files on the host, if
/// present. The IBM **entitled-registry** key (My IBM → Container software
/// library) authenticates to `cp.icr.io`; it is NOT the IBM Cloud API key, so we
/// only read a dedicated `~/.ibm/IBM_ENTITLEMENT_KEY` file (never the Cloud API
/// key, which `cp.icr.io` rejects). A value entered in the UI always wins.
fn preload_known_secrets(store: &sw_core::RunStore, id: &str) {
    let Some(home) = std::env::var_os("HOME") else { return };
    let path = std::path::Path::new(&home).join(".ibm").join("IBM_ENTITLEMENT_KEY");
    if let Ok(contents) = std::fs::read_to_string(&path) {
        let key = contents.trim().to_string();
        if !key.is_empty() {
            let mut secrets = store.load_secrets(id).unwrap_or_default();
            secrets.entry("IBM_ENTITLEMENT_KEY".to_string()).or_insert(key);
            let _ = store.save_secrets(id, &secrets);
        }
    }
}

/// Store non-empty UI-entered credentials into the run's `0600` secret store.
fn store_ui_credentials(store: &sw_core::RunStore, id: &str, creds: &BTreeMap<String, String>) {
    let provided: BTreeMap<String, String> = creds
        .iter()
        .filter(|(_, v)| !v.trim().is_empty())
        .map(|(k, v)| (k.clone(), v.trim().to_string()))
        .collect();
    if provided.is_empty() {
        return;
    }
    let mut secrets = store.load_secrets(id).unwrap_or_default();
    secrets.extend(provided);
    let _ = store.save_secrets(id, &secrets);
}

#[derive(Debug, Default, Deserialize)]
struct CreateRunBody {
    /// `"provision"` (new AWS cluster) or `"existing"` (adopt a kubeconfig).
    #[serde(default)]
    mode: Option<String>,
    /// Cloud / IBM credentials entered in the UI, stored as run secrets
    /// (e.g. `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `IBM_ENTITLEMENT_KEY`,
    /// `IBMCLOUD_API_KEY`, `ARM_CLIENT_SECRET`, `GOOGLE_CREDENTIALS`). Optional —
    /// blank fields fall back to `~/.aws` / `~/.ibm`.
    #[serde(default)]
    credentials: BTreeMap<String, String>,
    /// Non-secret inputs entered up front (e.g. `OCP_URL`,
    /// `kubeconfig_source_path`, selected `services`). Merged into run state.
    #[serde(default)]
    inputs: BTreeMap<String, String>,
}

async fn create_run(
    State(state): State<AppState>,
    body: Option<axum::Json<CreateRunBody>>,
) -> Response {
    let (mode, ui_creds, ui_inputs) = match body {
        Some(axum::Json(b)) => (
            b.mode.unwrap_or_else(|| "provision".to_string()),
            b.credentials,
            b.inputs,
        ),
        None => ("provision".to_string(), BTreeMap::new(), BTreeMap::new()),
    };
    let id = Uuid::new_v4().to_string();
    match state.orch.create_run_mode(id.clone(), mode) {
        Ok(mut run) => {
            // Convenience: preload credentials from well-known files so the user
            // isn't asked to paste them. AWS creds are read from ~/.aws by the
            // tools themselves; here we seed the IBM entitlement key.
            preload_known_secrets(state.orch.store(), &id);
            // UI-entered credentials override / supplement the file-based ones.
            store_ui_credentials(state.orch.store(), &id, &ui_creds);
            // Up-front non-secret inputs (cluster URL, kubeconfig path, selected
            // services, …) so steps don't have to prompt for them.
            if !ui_inputs.is_empty() {
                for (k, v) in &ui_inputs {
                    if !v.trim().is_empty() {
                        run.inputs.insert(k.clone(), v.trim().to_string());
                    }
                }
                let _ = state.orch.store().save(&run);
            }
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
    let orch = state.orch.clone();
    tokio::spawn(async move {
        // Pick the provisioner for the run's chosen cloud, with its non-secret
        // inputs (so the AWS destroy gets region/etc.) and its secrets.
        let run = orch.store().load(&id).ok();
        let inputs = run.as_ref().map(|r| r.inputs.clone()).unwrap_or_default();
        let secrets = orch.store().load_secrets(&id).unwrap_or_default();
        let provider = inputs.get("hyperscaler").cloned().unwrap_or_else(|| "aws".to_string());
        let artifacts = orch.store().artifacts_dir(&id);
        let ctx = sw_core::StepContext::with_artifacts(
            id.clone(),
            "mod-provision/destroy".to_string(),
            orch.command_runner(),
            orch.bus().clone(),
            inputs,
            secrets,
            artifacts,
        );
        let _ = sw_mod_provision::ProvisionerRegistry::new().get(&provider).destroy(&ctx).await;
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

#[derive(Debug, Default, Deserialize)]
struct ModeQuery {
    mode: Option<String>,
}

async fn get_modules(State(state): State<AppState>, Query(q): Query<ModeQuery>) -> Response {
    let views = match q.mode {
        Some(m) => state.orch.registry_for(&m).views(),
        None => state.orch.registry().views(),
    };
    Json(views).into_response()
}

async fn get_modes(State(state): State<AppState>) -> Response {
    Json(state.orch.modes()).into_response()
}

#[derive(Debug, Default, Deserialize)]
struct ProviderQuery {
    provider: Option<String>,
}

/// Provider-specific cluster-spec fields for the new-cluster form (defaults to AWS).
async fn get_provider_spec(Query(q): Query<ProviderQuery>) -> Response {
    let provider = q.provider.unwrap_or_else(|| "aws".to_string());
    Json(catalog::provider_spec(&provider)).into_response()
}

/// Report which prerequisite CLIs are present/missing on this machine.
async fn get_prereqs(State(state): State<AppState>) -> Response {
    let runner = state.orch.command_runner();
    Json(sw_mod_prereqs::check_all(runner.as_ref()).await).into_response()
}

/// Install every missing, auto-installable prerequisite, then report status.
async fn install_prereqs(State(state): State<AppState>) -> Response {
    let runner = state.orch.command_runner();
    Json(sw_mod_prereqs::install_missing(runner.as_ref()).await).into_response()
}

async fn openapi() -> Response {
    (
        [(header::CONTENT_TYPE, "application/yaml")],
        include_str!("../openapi.yaml"),
    )
        .into_response()
}
