//! `sw-api` — the axum web server for the IBM Software Hub installer.
//!
//! Exposes the OpenAPI 3.1.0 REST surface plus an SSE event stream, and serves
//! the no-build static UI. Binds `127.0.0.1` only; `/api/*` requires a session
//! token (passed as the `x-wxd-token` header or a `token=` query param) so the
//! browser-native `EventSource`, which cannot set headers, can still authenticate.

pub mod catalog;
pub mod preflight;
pub mod routes;

use std::sync::Arc;
use sw_core::{ModuleRegistry, Orchestrator};

/// Shared application state handed to every request handler.
#[derive(Clone)]
pub struct AppState {
    pub orch: Orchestrator,
    pub token: String,
    /// Absolute path to the static UI directory.
    pub ui_dir: std::path::PathBuf,
}

/// Assemble the module registry that defines a full install run, in order:
/// preflight → provision cluster → install Software Hub → install services
/// (watsonx.data by default; other entitled services plug in here).
pub fn default_registry() -> ModuleRegistry {
    let services = sw_mod_services::ServicesModule::new(vec![Arc::new(
        wxd_svc_watsonxdata::WatsonxDataInstaller,
    )]);
    ModuleRegistry::new()
        .with(Box::new(preflight::PreflightModule))
        .with(Box::new(sw_mod_provision::ProvisionModule))
        .with(Box::new(sw_mod_softwarehub::SoftwareHubModule))
        .with(Box::new(services))
}

/// Build the full axum router from shared state.
pub fn build_router(state: AppState) -> axum::Router {
    routes::router(state)
}

/// Convenience constructor used by the binary and by tests.
pub fn app(orch: Orchestrator, token: impl Into<String>, ui_dir: impl Into<std::path::PathBuf>) -> axum::Router {
    let state = AppState {
        orch,
        token: token.into(),
        ui_dir: ui_dir.into(),
    };
    build_router(state)
}

/// Build a ready-to-serve orchestrator with the default registry, the real
/// command runner, and the home-dir run store.
pub fn default_orchestrator() -> Orchestrator {
    Orchestrator::new(
        sw_core::EventBus::new(),
        sw_core::RunStore::default_home(),
        Arc::new(sw_core::RealCommandRunner),
        Arc::new(default_registry()),
    )
}
