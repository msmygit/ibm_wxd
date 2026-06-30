//! Drives a run through its steps: emits events, persists state at every
//! boundary, and implements pause / resume / retry / input submission. Steps are
//! never interrupted mid-call — pause takes effect at the next step boundary.

use crate::command::CommandRunner;
use crate::event::{Event, EventBus};
use crate::model::{RunState, RunStatus, StepOutcome, StepStatus};
use crate::module::StepContext;
use crate::registry::ModuleRegistry;
use crate::store::RunStore;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// Shared, cloneable orchestrator. One per process; safe to hand to many request
/// handlers. Holds one or more registries keyed by **mode** (e.g. `"provision"`
/// for "create a new cluster" vs `"existing"` for "use my cluster"); a run
/// records its mode so resume/retry use the same step graph.
#[derive(Clone)]
pub struct Orchestrator {
    bus: EventBus,
    store: RunStore,
    runner: Arc<dyn CommandRunner>,
    registries: Arc<BTreeMap<String, Arc<ModuleRegistry>>>,
    default_mode: String,
    paused: Arc<Mutex<HashSet<String>>>,
}

impl Orchestrator {
    /// Single-registry orchestrator (mode `"provision"`). Back-compat constructor.
    pub fn new(
        bus: EventBus,
        store: RunStore,
        runner: Arc<dyn CommandRunner>,
        registry: Arc<ModuleRegistry>,
    ) -> Self {
        let mut map = BTreeMap::new();
        map.insert("provision".to_string(), registry);
        Self::with_registries(bus, store, runner, map, "provision")
    }

    /// Multi-mode orchestrator. `registries` maps a mode name to its step graph;
    /// `default_mode` is used when a run doesn't specify one.
    pub fn with_registries(
        bus: EventBus,
        store: RunStore,
        runner: Arc<dyn CommandRunner>,
        registries: BTreeMap<String, Arc<ModuleRegistry>>,
        default_mode: impl Into<String>,
    ) -> Self {
        Self {
            bus,
            store,
            runner,
            registries: Arc::new(registries),
            default_mode: default_mode.into(),
            paused: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    pub fn store(&self) -> &RunStore {
        &self.store
    }

    /// The registry for a given mode, falling back to the default mode.
    pub fn registry_for(&self, mode: &str) -> &ModuleRegistry {
        self.registries
            .get(mode)
            .or_else(|| self.registries.get(&self.default_mode))
            .expect("default-mode registry must exist")
    }

    /// The default-mode registry (used by `/modules` with no mode specified).
    pub fn registry(&self) -> &ModuleRegistry {
        self.registry_for(&self.default_mode)
    }

    /// Available mode names (for the UI to offer "new cluster" vs "existing").
    pub fn modes(&self) -> Vec<String> {
        self.registries.keys().cloned().collect()
    }

    /// The shared command runner, for actions outside the step graph (teardown).
    pub fn command_runner(&self) -> Arc<dyn CommandRunner> {
        Arc::clone(&self.runner)
    }

    /// Create a fresh run in the default mode.
    pub fn create_run(&self, id: String) -> std::io::Result<RunState> {
        self.create_run_mode(id, self.default_mode.clone())
    }

    /// Create a fresh run in an explicit mode (e.g. `"existing"`).
    pub fn create_run_mode(&self, id: String, mode: String) -> std::io::Result<RunState> {
        let registry = self.registry_for(&mode);
        let mut state = RunState::new(id, registry.initial_steps());
        state.mode = mode;
        self.store.save(&state)?;
        Ok(state)
    }

    /// Publish an event to live subscribers and append it to the run's log.
    fn emit(&self, run_id: &str, event: Event) {
        self.bus.publish(event.clone());
        let _ = self.store.append_event(run_id, &event);
    }

    fn set_run_status(&self, state: &mut RunState, status: RunStatus) {
        state.status = status;
        self.emit(&state.id, Event::RunStatus { status });
    }

    /// Request a pause; it takes effect at the next step boundary.
    pub fn pause(&self, run_id: &str) {
        self.paused.lock().unwrap().insert(run_id.to_string());
    }

    fn is_paused(&self, run_id: &str) -> bool {
        self.paused.lock().unwrap().contains(run_id)
    }

    fn clear_pause(&self, run_id: &str) {
        self.paused.lock().unwrap().remove(run_id);
    }

    /// Resume a paused run from its cursor.
    pub async fn resume(&self, run_id: &str) -> std::io::Result<RunState> {
        self.clear_pause(run_id);
        let mut state = self.store.load(run_id)?;
        self.drive(&mut state).await?;
        Ok(state)
    }

    /// Retry the current failed step: reset it to pending and drive again.
    pub async fn retry(&self, run_id: &str) -> std::io::Result<RunState> {
        self.clear_pause(run_id);
        let mut state = self.store.load(run_id)?;
        if let Some(step) = state.steps.get_mut(state.cursor) {
            step.status = StepStatus::Pending;
            step.error = None;
            step.next_steps.clear();
        }
        self.store.save(&state)?;
        self.drive(&mut state).await?;
        Ok(state)
    }

    /// Submit answers for an `AwaitingInput` step. Non-secret values are merged
    /// into the run state; secrets go to the `0600` secret store only.
    pub async fn submit_inputs(
        &self,
        run_id: &str,
        values: BTreeMap<String, String>,
        secrets: BTreeMap<String, String>,
    ) -> std::io::Result<RunState> {
        let mut state = self.store.load(run_id)?;
        for (k, v) in values {
            state.inputs.insert(k, v);
        }
        if !secrets.is_empty() {
            let mut existing = self.store.load_secrets(run_id)?;
            existing.extend(secrets);
            self.store.save_secrets(run_id, &existing)?;
        }
        state.pending_inputs.clear();
        state.pending_prompt = None;
        // The awaiting step goes back to pending so drive re-runs it.
        if let Some(step) = state.steps.get_mut(state.cursor) {
            if step.status == StepStatus::AwaitingInput {
                step.status = StepStatus::Pending;
            }
        }
        self.store.save(&state)?;
        self.drive(&mut state).await?;
        Ok(state)
    }

    /// Drive the run forward from its cursor until it completes, fails, needs
    /// input, or is paused. Persists state at every boundary.
    pub async fn drive(&self, state: &mut RunState) -> std::io::Result<()> {
        self.set_run_status(state, RunStatus::Running);
        self.store.save(state)?;

        let flat = self.registry_for(&state.mode).flatten();
        let secrets = self.store.load_secrets(&state.id)?;

        while state.cursor < state.steps.len() {
            if self.is_paused(&state.id) {
                self.set_run_status(state, RunStatus::Paused);
                self.store.save(state)?;
                return Ok(());
            }

            let idx = state.cursor;
            let (_module_id, step) = &flat[idx];
            let step_id = state.steps[idx].id.clone();

            state.steps[idx].status = StepStatus::Running;
            self.emit(
                &state.id,
                Event::StepStatus {
                    step: step_id.clone(),
                    status: StepStatus::Running,
                },
            );
            self.store.save(state)?;

            let ctx = StepContext::with_artifacts(
                state.id.clone(),
                step_id.clone(),
                Arc::clone(&self.runner),
                self.bus.clone(),
                state.inputs.clone(),
                secrets.clone(),
                self.store.artifacts_dir(&state.id),
            )
            .with_persistence(self.store.clone());

            match step.run(&ctx).await {
                StepOutcome::Completed => {
                    state.steps[idx].status = StepStatus::Completed;
                    self.emit(
                        &state.id,
                        Event::StepStatus {
                            step: step_id,
                            status: StepStatus::Completed,
                        },
                    );
                    state.cursor += 1;
                    self.store.save(state)?;
                }
                StepOutcome::NeedsInput { prompt, fields } => {
                    state.steps[idx].status = StepStatus::AwaitingInput;
                    state.pending_prompt = Some(prompt);
                    state.pending_inputs = fields;
                    self.emit(
                        &state.id,
                        Event::StepStatus {
                            step: step_id,
                            status: StepStatus::AwaitingInput,
                        },
                    );
                    self.set_run_status(state, RunStatus::AwaitingInput);
                    self.store.save(state)?;
                    return Ok(());
                }
                StepOutcome::Failed { error, next_steps } => {
                    state.steps[idx].status = StepStatus::Failed;
                    state.steps[idx].error = Some(error.clone());
                    state.steps[idx].next_steps = next_steps;
                    self.emit(
                        &state.id,
                        Event::Log {
                            step: step_id.clone(),
                            line: format!("error: {error}"),
                        },
                    );
                    self.emit(
                        &state.id,
                        Event::StepStatus {
                            step: step_id,
                            status: StepStatus::Failed,
                        },
                    );
                    self.set_run_status(state, RunStatus::Failed);
                    self.store.save(state)?;
                    return Ok(());
                }
            }
        }

        self.set_run_status(state, RunStatus::Completed);
        self.store.save(state)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::MockCommandRunner;
    use crate::model::InputField;
    use crate::module::{Module, Step};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ---- test doubles -----------------------------------------------------

    struct OkStep(&'static str);
    #[async_trait]
    impl Step for OkStep {
        fn id(&self) -> &str {
            self.0
        }
        fn title(&self) -> &str {
            "ok"
        }
        async fn run(&self, ctx: &StepContext) -> StepOutcome {
            ctx.log("working");
            StepOutcome::Completed
        }
    }

    /// Fails on first call, succeeds afterwards (for retry tests).
    struct FlakyStep {
        id: &'static str,
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl Step for FlakyStep {
        fn id(&self) -> &str {
            self.id
        }
        fn title(&self) -> &str {
            "flaky"
        }
        async fn run(&self, _ctx: &StepContext) -> StepOutcome {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                StepOutcome::Failed {
                    error: "transient".into(),
                    next_steps: vec!["try again".into()],
                }
            } else {
                StepOutcome::Completed
            }
        }
    }

    /// Asks for input until the value is present, then completes.
    struct NeedsRegionStep;
    #[async_trait]
    impl Step for NeedsRegionStep {
        fn id(&self) -> &str {
            "region"
        }
        fn title(&self) -> &str {
            "region"
        }
        async fn run(&self, ctx: &StepContext) -> StepOutcome {
            if ctx.input("region").is_some() {
                StepOutcome::Completed
            } else {
                StepOutcome::NeedsInput {
                    prompt: "Pick a region".into(),
                    fields: vec![InputField {
                        key: "region".into(),
                        label: "Region".into(),
                        secret: false,
                        default: Some("us-east-1".into()),
                    }],
                }
            }
        }
    }

    // The orchestrator calls flatten() (one steps() call) per drive, and
    // initial_steps() once at create. A module that yields steps freshly each
    // call keeps these doubles honest across repeated calls.
    struct FreshModule<F: Fn() -> Vec<Box<dyn Step>> + Send + Sync> {
        id: &'static str,
        make: F,
    }
    impl<F: Fn() -> Vec<Box<dyn Step>> + Send + Sync> Module for FreshModule<F> {
        fn id(&self) -> &str {
            self.id
        }
        fn title(&self) -> &str {
            "mod"
        }
        fn steps(&self) -> Vec<Box<dyn Step>> {
            (self.make)()
        }
    }

    fn temp_store() -> RunStore {
        use std::sync::atomic::AtomicU64;
        static N: AtomicU64 = AtomicU64::new(1);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("wxd-orch-{}", std::process::id()))
            .join(format!("r{n}"));
        RunStore::new(dir)
    }

    fn orch_with(registry: ModuleRegistry) -> Orchestrator {
        Orchestrator::new(
            EventBus::new(),
            temp_store(),
            Arc::new(MockCommandRunner::new(vec![])),
            Arc::new(registry),
        )
    }

    #[tokio::test]
    async fn happy_path_runs_all_steps_to_completion() {
        let reg = ModuleRegistry::new().with(Box::new(FreshModule {
            id: "m",
            make: || vec![Box::new(OkStep("s1")), Box::new(OkStep("s2"))],
        }));
        let orch = orch_with(reg);
        let mut state = orch.create_run("run-happy".into()).unwrap();
        orch.drive(&mut state).await.unwrap();
        assert_eq!(state.status, RunStatus::Completed);
        assert!(state.steps.iter().all(|s| s.status == StepStatus::Completed));
        assert_eq!(state.cursor, 2);
    }

    #[tokio::test]
    async fn failure_halts_and_retry_resumes() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = Arc::clone(&calls);
        let reg = ModuleRegistry::new().with(Box::new(FreshModule {
            id: "m",
            make: move || {
                vec![
                    Box::new(OkStep("s1")),
                    Box::new(FlakyStep {
                        id: "s2",
                        calls: Arc::clone(&calls2),
                    }),
                    Box::new(OkStep("s3")),
                ]
            },
        }));
        let orch = orch_with(reg);
        let mut state = orch.create_run("run-retry".into()).unwrap();
        orch.drive(&mut state).await.unwrap();
        assert_eq!(state.status, RunStatus::Failed);
        assert_eq!(state.cursor, 1);
        assert_eq!(state.steps[1].status, StepStatus::Failed);
        assert_eq!(state.steps[1].next_steps, vec!["try again".to_string()]);

        // Retry: the flaky step now succeeds and the run completes.
        let after = orch.retry("run-retry").await.unwrap();
        assert_eq!(after.status, RunStatus::Completed);
        assert_eq!(after.cursor, 3);
    }

    #[tokio::test]
    async fn needs_input_pauses_then_submit_completes() {
        let reg = ModuleRegistry::new().with(Box::new(FreshModule {
            id: "m",
            make: || vec![Box::new(NeedsRegionStep), Box::new(OkStep("after"))],
        }));
        let orch = orch_with(reg);
        let mut state = orch.create_run("run-input".into()).unwrap();
        orch.drive(&mut state).await.unwrap();
        assert_eq!(state.status, RunStatus::AwaitingInput);
        assert_eq!(state.pending_inputs.len(), 1);
        assert_eq!(state.pending_inputs[0].key, "region");

        let mut values = BTreeMap::new();
        values.insert("region".to_string(), "us-west-2".to_string());
        let after = orch
            .submit_inputs("run-input", values, BTreeMap::new())
            .await
            .unwrap();
        assert_eq!(after.status, RunStatus::Completed);
        assert_eq!(after.inputs.get("region").unwrap(), "us-west-2");
    }

    #[tokio::test]
    async fn pause_stops_at_next_boundary_and_resume_finishes() {
        let reg = ModuleRegistry::new().with(Box::new(FreshModule {
            id: "m",
            make: || vec![Box::new(OkStep("s1")), Box::new(OkStep("s2"))],
        }));
        let orch = orch_with(reg);
        let mut state = orch.create_run("run-pause".into()).unwrap();
        // Pause before driving: the loop should stop immediately at the first
        // boundary check, before running any step.
        orch.pause("run-pause");
        orch.drive(&mut state).await.unwrap();
        assert_eq!(state.status, RunStatus::Paused);
        assert_eq!(state.cursor, 0);

        let after = orch.resume("run-pause").await.unwrap();
        assert_eq!(after.status, RunStatus::Completed);
        assert_eq!(after.cursor, 2);
    }
}
