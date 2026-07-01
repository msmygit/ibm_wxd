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

// ===========================================================================
// Generic, selection-driven services module (used by the app).
// ===========================================================================

/// Installs the user-selected set of IBM Software Hub components in a single
/// `cpd-cli manage apply-cr --components <list>`. The selection comes from the
/// UI's multi-select as the comma-separated `components` input (component ids
/// like `watsonx_data,watsonx_ai`); defaults to `watsonx_data`.
pub struct ComponentsModule;

const DEFAULT_COMPONENTS: &str = "watsonx_data";

fn selected_components(ctx: &StepContext) -> String {
    let raw = ctx
        .input("components")
        .filter(|c| !c.trim().is_empty())
        .unwrap_or(DEFAULT_COMPONENTS);
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(",")
}

fn operands_ns(ctx: &StepContext) -> String {
    ctx.input("PROJECT_CPD_INST_OPERANDS").unwrap_or("cpd-instance").to_string()
}

impl Module for ComponentsModule {
    fn id(&self) -> &str {
        "mod-services"
    }
    fn title(&self) -> &str {
        "Install services"
    }
    fn steps(&self) -> Vec<Box<dyn Step>> {
        vec![
            Box::new(SelectComponentsStep),
            Box::new(ApplyComponentsStep),
            Box::new(VerifyComponentsStep),
        ]
    }
}

struct SelectComponentsStep;
#[async_trait]
impl Step for SelectComponentsStep {
    fn id(&self) -> &str {
        "select-services"
    }
    fn title(&self) -> &str {
        "Select services"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        ctx.log(format!("selected components: {}", selected_components(ctx)));
        StepOutcome::Completed
    }
}

struct ApplyComponentsStep;
#[async_trait]
impl Step for ApplyComponentsStep {
    fn id(&self) -> &str {
        "install-services"
    }
    fn title(&self) -> &str {
        "Install selected services"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        let components = selected_components(ctx);
        let op_ns = ctx.input("PROJECT_CPD_INST_OPERATORS").unwrap_or("cpd-operators").to_string();
        let inst_ns = operands_ns(ctx);
        let version = ctx.input("VERSION").unwrap_or("5.4.0").to_string();
        let patch_id = ctx.input("PATCH_ID").unwrap_or("latest").to_string();
        // Software Hub services (watsonx.data et al.) need both a block (RWO) and
        // a file (RWX) storage class. Defaults match a provisioned AWS cluster.
        let block_sc = ctx.input("block_storage_class").unwrap_or("gp3-csi").to_string();
        let file_sc = ctx.input("file_storage_class").unwrap_or("efs-sc").to_string();
        // cpd-cli manage env, per the IBM installation-variables script. VERSION
        // pins the olm-utils image; PATCH_ID selects the patch; OPENSHIFT_TYPE/
        // IMAGE_ARCH describe the cluster. Keep in sync with softwarehub::cpd_env.
        let mut cpd_env = vec![
            ("VERSION".to_string(), version.clone()),
            ("PATCH_ID".to_string(), ctx.input("PATCH_ID").unwrap_or("latest").to_string()),
            ("OPENSHIFT_TYPE".to_string(), ctx.input("OPENSHIFT_TYPE").unwrap_or("self-managed").to_string()),
            ("IMAGE_ARCH".to_string(), ctx.input("IMAGE_ARCH").unwrap_or("amd64").to_string()),
        ];
        if let Some(img) = ctx.input("OLM_UTILS_IMAGE").filter(|v| !v.is_empty()) {
            cpd_env.push(("OLM_UTILS_IMAGE".to_string(), img.to_string()));
        }

        // install-components installs from locally-downloaded CASE packages —
        // fetch them for the selected components first.
        ctx.log(format!("downloading CASE packages for [{components}] (release {version})"));
        let dl = vec![
            "manage".into(),
            "case-download".into(),
            format!("--release={version}"),
            format!("--patch_id={patch_id}"),
            format!("--components={components}"),
        ];
        match ctx.run_in_cluster_pty_env("cpd-cli", &dl, &cpd_env, &[]).await {
            Ok(o) if o.success() => {}
            Ok(o) => {
                return StepOutcome::Failed {
                    error: format!("case-download for services failed (exit {}): {}", o.status, o.stderr.trim()),
                    next_steps: vec!["Confirm network access to the IBM CASE repository, then retry.".into()],
                }
            }
            Err(e) => {
                return StepOutcome::Failed {
                    error: format!("could not run cpd-cli: {e}"),
                    next_steps: vec!["Ensure cpd-cli is installed (Prerequisites), then retry.".into()],
                }
            }
        }

        ctx.log(format!(
            "installing components [{components}] (release {version}); block={block_sc}, file={file_sc}"
        ));
        let mut args = vec![
            "manage".into(),
            "install-components".into(),
            "--license_acceptance=true".into(),
            format!("--components={components}"),
            format!("--release={version}"),
            format!("--patch_id={patch_id}"),
            format!("--operator_ns={op_ns}"),
            format!("--instance_ns={inst_ns}"),
            format!("--block_storage_class={block_sc}"),
            format!("--file_storage_class={file_sc}"),
        ];
        // When a namespace-scoped image pull secret is configured (private
        // registry, or an explicit entitled-registry secret), pass it. The
        // default online path relies on the cluster's global pull secret.
        if let Some(secret) = ctx.input("IMAGE_PULL_SECRET").filter(|v| !v.is_empty()) {
            let prefix = ctx.input("IMAGE_PULL_PREFIX").unwrap_or("icr.io");
            args.push(format!("--image_pull_prefix={prefix}"));
            args.push(format!("--image_pull_secret={secret}"));
        }
        match ctx.run_in_cluster_pty_env("cpd-cli", &args, &cpd_env, &[]).await {
            Ok(o) if o.success() => {
                ctx.progress(100);
                StepOutcome::Completed
            }
            Ok(o) => StepOutcome::Failed {
                error: format!("install-components for services failed (exit {}): {}", o.status, o.stderr.trim()),
                next_steps: vec![
                    "Confirm the entitlement key, storage classes, and component ids, then retry.".into(),
                    format!("Inspect operand status: oc get ZenService -n {inst_ns}"),
                ],
            },
            Err(e) => StepOutcome::Failed {
                error: format!("could not run cpd-cli: {e}"),
                next_steps: vec!["Ensure cpd-cli is installed (Prerequisites), then retry.".into()],
            },
        }
    }
}

struct VerifyComponentsStep;
#[async_trait]
impl Step for VerifyComponentsStep {
    fn id(&self) -> &str {
        "verify-services"
    }
    fn title(&self) -> &str {
        "Verify services"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        let ns = operands_ns(ctx);
        ctx.log("checking service readiness");
        match ctx
            .run_in_cluster("oc", &["get".into(), "ZenService".into(), "-n".into(), ns.clone()])
            .await
        {
            Ok(o) if o.success() && o.stdout.contains("Completed") => {
                ctx.progress(100);
                StepOutcome::Completed
            }
            Ok(o) => StepOutcome::Failed {
                error: format!("services not ready yet: {}", o.stdout.trim()),
                next_steps: vec![
                    "Reconciliation can take a while; wait, then Retry this step.".into(),
                    format!("Watch: oc get ZenService -n {ns} -o yaml"),
                ],
            },
            Err(e) => StepOutcome::Failed {
                error: format!("could not query service status: {e}"),
                next_steps: vec!["Ensure oc has cluster access, then retry.".into()],
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use sw_core::{EventBus, MockCommandRunner, MockResponse};

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

    // ---- ComponentsModule (selection-driven) ------------------------------

    fn ctx_inputs(runner: Arc<dyn sw_core::CommandRunner>, inputs: &[(&str, &str)]) -> StepContext {
        let inputs: BTreeMap<String, String> =
            inputs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        StepContext::with_artifacts(
            "run".into(),
            "mod-services/x".into(),
            runner,
            EventBus::new(),
            inputs,
            BTreeMap::new(),
            std::env::temp_dir(),
        )
    }

    #[test]
    fn components_module_has_three_steps() {
        let ids: Vec<_> = ComponentsModule.steps().iter().map(|s| s.id().to_string()).collect();
        assert_eq!(ids, vec!["select-services", "install-services", "verify-services"]);
    }

    #[tokio::test]
    async fn install_uses_selected_components_and_defaults() {
        // explicit multi-select: case-download then install-components succeed
        let runner = Arc::new(MockCommandRunner::new(vec![
            MockResponse::ok("case-download", "ok"),
            MockResponse::ok("install-components", "ok"),
        ]));
        let ctx = ctx_inputs(runner.clone(), &[("components", "watsonx_data,watsonx_ai")]);
        assert_eq!(ApplyComponentsStep.run(&ctx).await, StepOutcome::Completed);
        assert!(runner.calls().iter().any(|c| c.contains("install-components") && c.contains("--components=watsonx_data,watsonx_ai")));

        // default when absent
        let runner2 = Arc::new(MockCommandRunner::new(vec![
            MockResponse::ok("case-download", "ok"),
            MockResponse::ok("install-components", "ok"),
        ]));
        let ctx2 = ctx_inputs(runner2.clone(), &[]);
        assert_eq!(ApplyComponentsStep.run(&ctx2).await, StepOutcome::Completed);
        assert!(runner2.calls().iter().any(|c| c.contains("install-components") && c.contains("--components=watsonx_data")));
    }

    #[tokio::test]
    async fn install_failure_reports_next_steps() {
        let runner = Arc::new(MockCommandRunner::new(vec![
            MockResponse::ok("case-download", "ok"),
            MockResponse::fail("install-components", 1, "bad component"),
        ]));
        let ctx = ctx_inputs(runner, &[("components", "nope")]);
        match ApplyComponentsStep.run(&ctx).await {
            StepOutcome::Failed { next_steps, .. } => assert!(!next_steps.is_empty()),
            o => panic!("expected Failed, got {o:?}"),
        }
    }

    #[tokio::test]
    async fn verify_completes_when_zenservice_completed() {
        let runner = Arc::new(MockCommandRunner::new(vec![MockResponse::ok("ZenService", "lite-cr Completed")]));
        let ctx = ctx_inputs(runner, &[]);
        assert_eq!(VerifyComponentsStep.run(&ctx).await, StepOutcome::Completed);
    }
}
