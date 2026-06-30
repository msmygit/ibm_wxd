//! The plug-n-play surface: a `Module` declares ordered, idempotent, resumable
//! `Step`s. A step runs against a `StepContext` (inputs, command runner, event
//! emitter) and returns a `StepOutcome`.

use crate::command::{CommandOutput, CommandRunner};
use crate::event::{Event, EventBus};
use crate::model::{StepId, StepOutcome, StepStatus};
use crate::store::RunStore;
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Everything a step needs while running. Constructed by the orchestrator per
/// step; cheap to build and not held across steps.
pub struct StepContext {
    pub run_id: String,
    pub step_id: StepId,
    runner: Arc<dyn CommandRunner>,
    bus: EventBus,
    inputs: BTreeMap<String, String>,
    secrets: BTreeMap<String, String>,
    artifacts_dir: PathBuf,
    /// When set, step-emitted `Log`/`Progress` events are appended to the run's
    /// `events.log` (not just published to live subscribers) so the live log —
    /// including the `$ command` echoes — survives a page refresh / reconnect.
    store: Option<RunStore>,
}

impl StepContext {
    pub fn new(
        run_id: String,
        step_id: StepId,
        runner: Arc<dyn CommandRunner>,
        bus: EventBus,
        inputs: BTreeMap<String, String>,
        secrets: BTreeMap<String, String>,
    ) -> Self {
        Self::with_artifacts(run_id, step_id, runner, bus, inputs, secrets, PathBuf::new())
    }

    /// Full constructor including the run's artifacts directory (kubeconfig,
    /// install-config.yaml, generated `cpd_vars.sh`, install logs).
    #[allow(clippy::too_many_arguments)]
    pub fn with_artifacts(
        run_id: String,
        step_id: StepId,
        runner: Arc<dyn CommandRunner>,
        bus: EventBus,
        inputs: BTreeMap<String, String>,
        secrets: BTreeMap<String, String>,
        artifacts_dir: PathBuf,
    ) -> Self {
        Self {
            run_id,
            step_id,
            runner,
            bus,
            inputs,
            secrets,
            artifacts_dir,
            store: None,
        }
    }

    /// Enable persistence of step-emitted `Log`/`Progress` events to the run's
    /// `events.log`. The orchestrator turns this on; tests leave it off.
    pub fn with_persistence(mut self, store: RunStore) -> Self {
        self.store = Some(store);
        self
    }

    /// Publish an event to live subscribers and, when persistence is enabled,
    /// append it to the run's `events.log` so it replays after a reconnect.
    fn emit(&self, event: Event) {
        self.bus.publish(event.clone());
        if let Some(store) = &self.store {
            let _ = store.append_event(&self.run_id, &event);
        }
    }

    /// The shared command runner — the only way a step touches the OS. Prefer the
    /// logging wrappers [`run`](Self::run) / [`run_with_env`](Self::run_with_env)
    /// so the exact command shows up in the live log.
    pub fn runner(&self) -> &dyn CommandRunner {
        self.runner.as_ref()
    }

    /// Render a command as a `program arg1 arg2` line for the live log, masking
    /// any known secret values (tokens, passwords) that appear in the arguments.
    fn cmdline(&self, program: &str, args: &[String]) -> String {
        let mut line = if args.is_empty() {
            program.to_string()
        } else {
            format!("{} {}", program, args.join(" "))
        };
        for v in self.secrets.values() {
            // Only mask non-trivial values to avoid redacting an empty/short
            // secret that would otherwise blanket the whole line.
            if v.len() >= 4 && line.contains(v.as_str()) {
                line = line.replace(v.as_str(), "***");
            }
        }
        line
    }

    /// Run an external command, first echoing the exact (secret-redacted) command
    /// line to this step's live log (`$ program args`). Prefer this over
    /// `ctx.runner().run(...)` so the UI shows what is actually executing.
    pub async fn run(&self, program: &str, args: &[String]) -> std::io::Result<CommandOutput> {
        self.run_with_env(program, args, &[]).await
    }

    /// Like [`run`](Self::run) but with extra environment variables, and still
    /// logs the command line first. Env values are never logged.
    pub async fn run_with_env(
        &self,
        program: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> std::io::Result<CommandOutput> {
        self.log(format!("$ {}", self.cmdline(program, args)));
        self.runner.run_with_env(program, args, env).await
    }

    /// Directory for this run's artifacts. Steps write kubeconfig,
    /// install-config.yaml, generated `cpd_vars.sh`, and logs here.
    pub fn artifacts_dir(&self) -> &Path {
        &self.artifacts_dir
    }

    /// Standard location of the run's kubeconfig (`<artifacts>/kubeconfig`). The
    /// provisioning module writes the freshly-created cluster's kubeconfig here
    /// (and the existing-cluster path drops a user-supplied one here), so every
    /// downstream cluster command can target the right cluster.
    pub fn kubeconfig_path(&self) -> PathBuf {
        self.artifacts_dir.join("kubeconfig")
    }

    /// Run a cluster-targeting command (`oc`, `cpd-cli`, …) with `KUBECONFIG`
    /// pointed at this run's kubeconfig. Use this for anything that talks to the
    /// provisioned cluster so steps don't depend on the caller's shell session.
    pub async fn run_in_cluster(
        &self,
        program: &str,
        args: &[String],
    ) -> std::io::Result<CommandOutput> {
        let kc = self.kubeconfig_path().to_string_lossy().into_owned();
        self.run_with_env(program, args, &[("KUBECONFIG".to_string(), kc)])
            .await
    }

    /// A non-secret input value collected earlier in the run.
    pub fn input(&self, key: &str) -> Option<&str> {
        self.inputs.get(key).map(String::as_str)
    }

    /// A secret value (entitlement key, password, token). Never logged.
    pub fn secret(&self, key: &str) -> Option<&str> {
        self.secrets.get(key).map(String::as_str)
    }

    /// Emit a log line for this step. Persisted to `events.log` when enabled.
    pub fn log(&self, line: impl Into<String>) {
        self.emit(Event::Log {
            step: self.step_id.clone(),
            line: line.into(),
        });
    }

    /// Emit coarse progress (clamped to 0..=100). Persisted when enabled.
    pub fn progress(&self, percent: u8) {
        self.emit(Event::Progress {
            step: self.step_id.clone(),
            percent: percent.min(100),
        });
    }

    /// Emit a status change for this step.
    pub fn status(&self, status: StepStatus) {
        self.bus.publish(Event::StepStatus {
            step: self.step_id.clone(),
            status,
        });
    }
}

/// One idempotent, resumable unit of work.
#[async_trait]
pub trait Step: Send + Sync {
    /// Stable id, unique within the module.
    fn id(&self) -> &str;
    /// Human-readable title shown in the UI progress tracker.
    fn title(&self) -> &str;
    /// Do the work. Must be idempotent (check-then-act) so retry/resume is safe.
    async fn run(&self, ctx: &StepContext) -> StepOutcome;
}

/// A plug-n-play module contributing an ordered list of steps to a run.
pub trait Module: Send + Sync {
    /// Stable module id (e.g. "mod-provision").
    fn id(&self) -> &str;
    /// Human-readable module title.
    fn title(&self) -> &str;
    /// The ordered steps this module contributes.
    fn steps(&self) -> Vec<Box<dyn Step>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::MockCommandRunner;

    struct EchoStep;

    #[async_trait]
    impl Step for EchoStep {
        fn id(&self) -> &str {
            "echo"
        }
        fn title(&self) -> &str {
            "Echo"
        }
        async fn run(&self, ctx: &StepContext) -> StepOutcome {
            ctx.log("hello");
            ctx.progress(100);
            StepOutcome::Completed
        }
    }

    #[tokio::test]
    async fn step_runs_and_emits_events() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let ctx = StepContext::new(
            "run1".into(),
            "m/echo".into(),
            Arc::new(MockCommandRunner::new(vec![])),
            bus,
            BTreeMap::new(),
            BTreeMap::new(),
        );
        let outcome = EchoStep.run(&ctx).await;
        assert_eq!(outcome, StepOutcome::Completed);
        // First event is the log line.
        let e = rx.recv().await.unwrap();
        assert!(matches!(e, Event::Log { .. }));
    }

    #[test]
    fn kubeconfig_path_is_under_artifacts() {
        let ctx = StepContext::with_artifacts(
            "r".into(),
            "m/s".into(),
            Arc::new(MockCommandRunner::new(vec![])),
            EventBus::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            PathBuf::from("/tmp/run-artifacts"),
        );
        assert_eq!(ctx.kubeconfig_path(), PathBuf::from("/tmp/run-artifacts/kubeconfig"));
    }

    /// A runner that records the env it was handed, to prove run_in_cluster
    /// injects KUBECONFIG.
    #[derive(Default)]
    struct EnvSpy {
        last_env: std::sync::Mutex<Vec<(String, String)>>,
    }
    #[async_trait]
    impl CommandRunner for EnvSpy {
        async fn run(&self, _p: &str, _a: &[String]) -> std::io::Result<crate::command::CommandOutput> {
            Ok(crate::command::CommandOutput { status: 0, stdout: String::new(), stderr: String::new() })
        }
        async fn run_with_env(
            &self,
            _p: &str,
            _a: &[String],
            env: &[(String, String)],
        ) -> std::io::Result<crate::command::CommandOutput> {
            *self.last_env.lock().unwrap() = env.to_vec();
            self.run(_p, _a).await
        }
    }

    #[tokio::test]
    async fn step_logs_persist_to_events_log_when_enabled() {
        // With persistence on, ctx.log lines (incl. the `$ cmd` echoes) are
        // appended to events.log so they survive a UI refresh / reconnect.
        let tmp = std::env::temp_dir().join(format!("wxd-persist-{}", std::process::id()));
        let store = crate::store::RunStore::new(&tmp);
        let ctx = StepContext::new(
            "run-persist".into(),
            "m/step".into(),
            Arc::new(MockCommandRunner::new(vec![])),
            EventBus::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        )
        .with_persistence(store.clone());
        ctx.run("aws", &["route53".into(), "list-hosted-zones".into()]).await.unwrap();
        ctx.log("done");

        let events = store.replay_events("run-persist").unwrap();
        let lines: Vec<String> = events
            .into_iter()
            .filter_map(|e| match e {
                Event::Log { line, .. } => Some(line),
                _ => None,
            })
            .collect();
        assert!(lines.iter().any(|l| l == "$ aws route53 list-hosted-zones"), "got {lines:?}");
        assert!(lines.iter().any(|l| l == "done"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn run_echoes_command_to_log_and_redacts_secrets() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let mut secrets = BTreeMap::new();
        secrets.insert("OCP_TOKEN".to_string(), "sha256~supersecrettoken".to_string());
        let ctx = StepContext::new(
            "r".into(),
            "m/login".into(),
            Arc::new(MockCommandRunner::new(vec![])),
            bus,
            BTreeMap::new(),
            secrets,
        );
        ctx.run("oc", &["login".into(), "--token=sha256~supersecrettoken".into()])
            .await
            .unwrap();
        // First event is the echoed command line, with the secret masked.
        match rx.recv().await.unwrap() {
            Event::Log { line, .. } => {
                assert_eq!(line, "$ oc login --token=***");
                assert!(!line.contains("supersecrettoken"));
            }
            other => panic!("expected a Log event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_in_cluster_sets_kubeconfig_env() {
        let spy = Arc::new(EnvSpy::default());
        let ctx = StepContext::with_artifacts(
            "r".into(),
            "m/s".into(),
            spy.clone(),
            EventBus::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            PathBuf::from("/tmp/run-x"),
        );
        ctx.run_in_cluster("oc", &["whoami".into()]).await.unwrap();
        let env = spy.last_env.lock().unwrap().clone();
        assert_eq!(env, vec![("KUBECONFIG".to_string(), "/tmp/run-x/kubeconfig".to_string())]);
    }
}
