//! `sw-mod-services` — the generic "install selected IBM services" module.
//!
//! This crate is service-agnostic: it knows nothing about watsonx.data or any
//! other specific service. It defines the [`ServiceInstaller`] trait that each
//! service implements (`wxd-svc-watsonxdata`, future `sw-svc-*` crates), and a
//! [`ServicesModule`] that plugs into the `sw-core` orchestrator. The module
//! contributes, in order: a `select-services` step (the UI chooses which
//! services to install), then for each installer an `install-<id>` step and a
//! `verify-<id>` step.
//!
//! Per the framework contract every external command goes through
//! `ctx.runner()`; nothing here shells out directly, so the whole module is
//! hermetically testable with `sw_core::MockCommandRunner`.

use async_trait::async_trait;
use std::sync::Arc;
use sw_core::{Module, Step, StepContext, StepOutcome};

/// One installable IBM service (watsonx.data, future services). An installer is
/// pure orchestration: it decides what commands to run via `ctx.runner()` and
/// must be idempotent so retry/resume is safe.
#[async_trait]
pub trait ServiceInstaller: Send + Sync {
    /// Stable, URL-safe id used in step ids and the UI selection list
    /// (e.g. "watsonx-data").
    fn service_id(&self) -> &str;
    /// Human-readable name shown in the UI (e.g. "watsonx.data").
    fn display_name(&self) -> &str;
    /// The `cpd_vars.sh` COMPONENTS token for this service (e.g. "watsonx_data").
    fn component(&self) -> &str;
    /// Install the service. Must be idempotent (check-then-act).
    async fn install(&self, ctx: &StepContext) -> StepOutcome;
    /// Verify the service is ready. Safe to call repeatedly.
    async fn verify(&self, ctx: &StepContext) -> StepOutcome;
}

/// The plug-n-play module that installs the user-selected set of services.
///
/// Holds its installers behind `Arc` so each generated [`Step`] can own a cheap
/// clone (trait objects can't be borrowed out of `Module::steps(&self)`).
pub struct ServicesModule {
    installers: Vec<Arc<dyn ServiceInstaller>>,
}

impl ServicesModule {
    /// Build the module from the installers to offer. Order is preserved: it
    /// determines both the default selection and the step order.
    pub fn new(installers: Vec<Arc<dyn ServiceInstaller>>) -> Self {
        Self { installers }
    }
}

impl Module for ServicesModule {
    fn id(&self) -> &str {
        "mod-services"
    }

    fn title(&self) -> &str {
        "Install services"
    }

    fn steps(&self) -> Vec<Box<dyn Step>> {
        let mut steps: Vec<Box<dyn Step>> = Vec::with_capacity(self.installers.len() * 2 + 1);
        let default_ids: Vec<String> = self
            .installers
            .iter()
            .map(|i| i.service_id().to_string())
            .collect();
        steps.push(Box::new(SelectServicesStep { default_ids }));
        for installer in &self.installers {
            steps.push(Box::new(InstallStep {
                installer: Arc::clone(installer),
            }));
            steps.push(Box::new(VerifyStep {
                installer: Arc::clone(installer),
            }));
        }
        steps
    }
}

/// Records the chosen services. The selection itself comes from the UI via the
/// `services` input (comma-separated service ids); when absent we default to all
/// offered services. This step never fails — it just normalizes/echoes intent.
struct SelectServicesStep {
    default_ids: Vec<String>,
}

#[async_trait]
impl Step for SelectServicesStep {
    fn id(&self) -> &str {
        "select-services"
    }

    fn title(&self) -> &str {
        "Select services"
    }

    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        let selected: Vec<String> = match ctx.input("services") {
            Some(raw) => raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            None => self.default_ids.clone(),
        };
        let selected = if selected.is_empty() {
            self.default_ids.clone()
        } else {
            selected
        };
        ctx.log(format!("selected services: {}", selected.join(", ")));
        StepOutcome::Completed
    }
}

/// Drives a single installer's `install()`.
struct InstallStep {
    installer: Arc<dyn ServiceInstaller>,
}

#[async_trait]
impl Step for InstallStep {
    fn id(&self) -> &str {
        // Leaked once per step instance; step ids must outlive the &str borrow
        // required by the trait. `steps()` is called rarely (run setup), so this
        // is bounded by the number of services, not by runtime work.
        Box::leak(format!("install-{}", self.installer.service_id()).into_boxed_str())
    }

    fn title(&self) -> &str {
        Box::leak(format!("Install {}", self.installer.display_name()).into_boxed_str())
    }

    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        self.installer.install(ctx).await
    }
}

/// Drives a single installer's `verify()`.
struct VerifyStep {
    installer: Arc<dyn ServiceInstaller>,
}

#[async_trait]
impl Step for VerifyStep {
    fn id(&self) -> &str {
        Box::leak(format!("verify-{}", self.installer.service_id()).into_boxed_str())
    }

    fn title(&self) -> &str {
        Box::leak(format!("Verify {}", self.installer.display_name()).into_boxed_str())
    }

    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        self.installer.verify(ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use sw_core::{EventBus, MockCommandRunner};

    /// A fake installer that records how many times install/verify ran and runs
    /// a marker command through the runner so we can assert it was driven.
    struct FakeInstaller {
        installs: AtomicUsize,
        verifies: AtomicUsize,
    }

    impl FakeInstaller {
        fn new() -> Self {
            Self {
                installs: AtomicUsize::new(0),
                verifies: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl ServiceInstaller for FakeInstaller {
        fn service_id(&self) -> &str {
            "fake"
        }
        fn display_name(&self) -> &str {
            "Fake Service"
        }
        fn component(&self) -> &str {
            "fake_component"
        }
        async fn install(&self, ctx: &StepContext) -> StepOutcome {
            self.installs.fetch_add(1, Ordering::SeqCst);
            let _ = ctx.runner().run("fake-cli", &["install".into()]).await;
            StepOutcome::Completed
        }
        async fn verify(&self, ctx: &StepContext) -> StepOutcome {
            self.verifies.fetch_add(1, Ordering::SeqCst);
            let _ = ctx.runner().run("fake-cli", &["verify".into()]).await;
            StepOutcome::Completed
        }
    }

    fn ctx_with(runner: Arc<dyn sw_core::CommandRunner>) -> StepContext {
        StepContext::new(
            "run-test".into(),
            "mod-services/step".into(),
            runner,
            EventBus::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        )
    }

    #[test]
    fn steps_are_select_then_install_then_verify_in_order() {
        let module = ServicesModule::new(vec![Arc::new(FakeInstaller::new())]);
        let steps = module.steps();
        let ids: Vec<&str> = steps.iter().map(|s| s.id()).collect();
        assert_eq!(ids, vec!["select-services", "install-fake", "verify-fake"]);
        assert_eq!(module.id(), "mod-services");
    }

    #[test]
    fn two_installers_yield_paired_steps_in_declared_order() {
        struct Other;
        #[async_trait]
        impl ServiceInstaller for Other {
            fn service_id(&self) -> &str {
                "other"
            }
            fn display_name(&self) -> &str {
                "Other"
            }
            fn component(&self) -> &str {
                "other_component"
            }
            async fn install(&self, _ctx: &StepContext) -> StepOutcome {
                StepOutcome::Completed
            }
            async fn verify(&self, _ctx: &StepContext) -> StepOutcome {
                StepOutcome::Completed
            }
        }
        let module =
            ServicesModule::new(vec![Arc::new(FakeInstaller::new()), Arc::new(Other)]);
        let ids: Vec<String> = module.steps().iter().map(|s| s.id().to_string()).collect();
        assert_eq!(
            ids,
            vec![
                "select-services",
                "install-fake",
                "verify-fake",
                "install-other",
                "verify-other",
            ]
        );
    }

    #[tokio::test]
    async fn install_step_drives_the_installer_install() {
        let fake = Arc::new(FakeInstaller::new());
        let module = ServicesModule::new(vec![fake.clone()]);
        let steps = module.steps();
        let install_step = steps
            .iter()
            .find(|s| s.id() == "install-fake")
            .expect("install step present");

        let runner = Arc::new(MockCommandRunner::new(vec![]));
        let ctx = ctx_with(runner.clone());
        let outcome = install_step.run(&ctx).await;

        assert_eq!(outcome, StepOutcome::Completed);
        assert_eq!(fake.installs.load(Ordering::SeqCst), 1);
        assert_eq!(fake.verifies.load(Ordering::SeqCst), 0);
        assert_eq!(runner.calls(), vec!["fake-cli install".to_string()]);
    }

    #[tokio::test]
    async fn verify_step_drives_the_installer_verify() {
        let fake = Arc::new(FakeInstaller::new());
        let module = ServicesModule::new(vec![fake.clone()]);
        let steps = module.steps();
        let verify_step = steps
            .iter()
            .find(|s| s.id() == "verify-fake")
            .expect("verify step present");

        let runner = Arc::new(MockCommandRunner::new(vec![]));
        let ctx = ctx_with(runner.clone());
        let outcome = verify_step.run(&ctx).await;

        assert_eq!(outcome, StepOutcome::Completed);
        assert_eq!(fake.verifies.load(Ordering::SeqCst), 1);
        assert_eq!(runner.calls(), vec!["fake-cli verify".to_string()]);
    }

    #[tokio::test]
    async fn select_defaults_to_all_when_input_absent() {
        let module = ServicesModule::new(vec![Arc::new(FakeInstaller::new())]);
        let steps = module.steps();
        let select = &steps[0];
        let ctx = ctx_with(Arc::new(MockCommandRunner::new(vec![])));
        assert_eq!(select.run(&ctx).await, StepOutcome::Completed);
    }
}
