//! `sw-api` — the axum web server for the IBM Software Hub installer.
//!
//! Exposes the OpenAPI 3.1.0 REST surface plus an SSE event stream, and serves
//! the no-build static UI. Binds `127.0.0.1` only; `/api/*` requires a session
//! token (passed as the `x-wxd-token` header or a `token=` query param) so the
//! browser-native `EventSource`, which cannot set headers, can still authenticate.

pub mod catalog;
pub mod preflight;
pub mod routes;

use std::collections::BTreeMap;
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

/// The selection-driven services module (shared by both run modes). Installs the
/// components chosen in the UI's multi-select via one `cpd-cli apply-cr`.
fn services_module() -> sw_mod_services::ComponentsModule {
    sw_mod_services::ComponentsModule
}

/// "Provision a new cluster" graph: install prerequisites → provision (AWS IPI)
/// → provision RWX storage (EFS) → install Software Hub → install services
/// (watsonx.data by default).
pub fn default_registry() -> ModuleRegistry {
    ModuleRegistry::new()
        .with(Box::new(sw_mod_prereqs::PrereqsModule))
        .with(Box::new(sw_mod_provision::ProvisionModule::new()))
        .with(Box::new(sw_mod_storage::StorageModule))
        .with(Box::new(sw_mod_softwarehub::SoftwareHubModule))
        .with(Box::new(services_module()))
}

/// "Use my existing cluster" graph: install prerequisites → adopt the user's
/// kubeconfig → install Software Hub → install services. Skips provisioning.
pub fn existing_registry() -> ModuleRegistry {
    ModuleRegistry::new()
        .with(Box::new(sw_mod_prereqs::PrereqsModule))
        .with(Box::new(sw_mod_existing::ExistingClusterModule))
        .with(Box::new(sw_mod_softwarehub::SoftwareHubModule))
        .with(Box::new(services_module()))
}

/// The mode → registry map both run paths share.
pub fn registries() -> BTreeMap<String, Arc<ModuleRegistry>> {
    let mut m = BTreeMap::new();
    m.insert("provision".to_string(), Arc::new(default_registry()));
    m.insert("existing".to_string(), Arc::new(existing_registry()));
    m
}

/// Build the full axum router from shared state.
pub fn build_router(state: AppState) -> axum::Router {
    routes::router(state)
}

/// Convenience constructor used by the binary and by tests.
pub fn app(
    orch: Orchestrator,
    token: impl Into<String>,
    ui_dir: impl Into<std::path::PathBuf>,
) -> axum::Router {
    let state = AppState {
        orch,
        token: token.into(),
        ui_dir: ui_dir.into(),
    };
    build_router(state)
}

/// Build a ready-to-serve orchestrator with both run modes (provision +
/// existing), the real command runner, and the home-dir run store.
pub fn default_orchestrator() -> Orchestrator {
    Orchestrator::with_registries(
        sw_core::EventBus::new(),
        sw_core::RunStore::default_home(),
        Arc::new(sw_core::RealCommandRunner),
        registries(),
        "provision",
    )
}
