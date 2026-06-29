//! The plug-n-play surface: a `Module` declares ordered, idempotent, resumable
//! `Step`s. A step runs against a `StepContext` (inputs, command runner, event
//! emitter) and returns a `StepOutcome`.

use crate::command::{CommandOutput, CommandRunner};
use crate::event::{Event, EventBus};
use crate::model::{StepId, StepOutcome, StepStatus};
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
        }
    }

    /// The shared command runner — the only way a step touches the OS.
    pub fn runner(&self) -> &dyn CommandRunner {
        self.runner.as_ref()
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
        self.runner
            .run_with_env(program, args, &[("KUBECONFIG".to_string(), kc)])
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

    /// Emit a log line for this step.
    pub fn log(&self, line: impl Into<String>) {
        self.bus.publish(Event::Log {
            step: self.step_id.clone(),
            line: line.into(),
        });
    }

    /// Emit coarse progress (clamped to 0..=100).
    pub fn progress(&self, percent: u8) {
        self.bus.publish(Event::Progress {
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
