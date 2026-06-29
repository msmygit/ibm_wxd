//! `sw-core` — the orchestrator spine for the IBM Software Hub installer.
//!
//! This crate is service-agnostic: it knows nothing about watsonx.data
//! specifically. It provides the plug-n-play [`Module`]/[`Step`] framework, the
//! [`Run`](model::RunState) state machine with pause/resume/retry, the event bus
//! that feeds the UI's SSE stream, the [`CommandRunner`] seam for all external
//! commands, and the [`RunStore`] that persists everything under `~/.wxd`.
//!
//! watsonx.data-specific behavior lives in `wxd-*` modules built on top of these
//! traits; other entitled IBM services plug in the same way.

pub mod command;
pub mod event;
pub mod model;
pub mod module;
pub mod orchestrator;
pub mod registry;
pub mod store;

pub use command::{CommandOutput, CommandRunner, MockCommandRunner, MockResponse, RealCommandRunner};
pub use event::{Event, EventBus};
pub use model::{
    InputField, RunId, RunState, RunStatus, StepId, StepOutcome, StepState, StepStatus,
};
pub use module::{Module, Step, StepContext};
pub use orchestrator::Orchestrator;
pub use registry::{ModuleRegistry, ModuleView, StepView};
pub use store::RunStore;
