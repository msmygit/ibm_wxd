//! IBM Software Hub / Cloud Pak for Data install module.
//!
//! Drives the 5.4.0 install through `cpd-cli` and `oc` — operators → control
//! plane → readiness — entirely via the `CommandRunner` seam (hermetic in
//! tests). Every step is idempotent (check-then-act) so retry/resume is safe.

use async_trait::async_trait;
use sw_core::{InputField, Module, Step, StepContext, StepOutcome};

const DEFAULT_VERSION: &str = "5.4.0";

/// Convenience: read an input or fall back to a default.
fn input_or<'a>(ctx: &'a StepContext, key: &str, default: &'a str) -> String {
    ctx.input(key).unwrap_or(default).to_string()
}

// ---- steps ----------------------------------------------------------------

/// Verify the client tooling and an authenticated session exist, and that the
/// entitlement key is available.
struct PreflightHub;

#[async_trait]
impl Step for PreflightHub {
    fn id(&self) -> &str {
        "preflight-hub"
    }
    fn title(&self) -> &str {
        "Preflight: cpd-cli / oc / session"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        for (tool, arg) in [("cpd-cli", "version"), ("oc", "version")] {
            ctx.log(format!("checking `{tool}`"));
            match ctx.run(tool, &[arg.to_string()]).await {
                Ok(o) if o.success() => {}
                _ => {
                    return StepOutcome::Failed {
                        error: format!("`{tool}` is not available"),
                        next_steps: vec![format!(
                            "Install the `{tool}` client and ensure it is on your PATH, then retry."
                        )],
                    }
                }
            }
        }
        // Authenticated session against the run's cluster?
        match ctx.run_in_cluster("oc", &["whoami".to_string()]).await {
            Ok(o) if o.success() => {}
            _ => {
                return StepOutcome::Failed {
                    error: "no active OpenShift session".into(),
                    next_steps: vec![
                        "Run `oc login <OCP_URL>` (user/password or --token), then retry."
                            .into(),
                    ],
                }
            }
        }
        // Entitlement key present?
        if ctx.secret("IBM_ENTITLEMENT_KEY").is_none() {
            return StepOutcome::NeedsInput {
                prompt: "Provide your IBM entitlement key (from My IBM → Container software library)."
                    .into(),
                fields: vec![InputField {
                    key: "IBM_ENTITLEMENT_KEY".into(),
                    label: "IBM entitlement key".into(),
                    secret: true,
                    default: None,
                }],
            };
        }
        ctx.progress(100);
        StepOutcome::Completed
    }
}

/// Add the IBM entitled registry (`cp.icr.io`) credential to the cluster's
/// global pull secret so OLM/operator images can be pulled. Without this,
/// `apply-olm` pulls fail. `cpd-cli ... add-icr-cred-to-global-pull-secret` is
/// itself idempotent (re-applying the same cred is a no-op that won't roll
/// nodes), so this step can run on every attempt.
struct AddEntitlement;

#[async_trait]
impl Step for AddEntitlement {
    fn id(&self) -> &str {
        "entitle-registry"
    }
    fn title(&self) -> &str {
        "Add IBM entitled registry to pull secret"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        let key = match ctx.secret("IBM_ENTITLEMENT_KEY") {
            Some(k) if !k.is_empty() => k.to_string(),
            _ => {
                return StepOutcome::NeedsInput {
                    prompt: "Provide your IBM entitlement key (My IBM → Container software library) \
                             so the cluster can pull IBM images from cp.icr.io."
                        .into(),
                    fields: vec![InputField {
                        key: "IBM_ENTITLEMENT_KEY".into(),
                        label: "IBM entitlement key".into(),
                        secret: true,
                        default: None,
                    }],
                };
            }
        };
        ctx.log("adding IBM entitled registry (cp.icr.io) to the cluster global pull secret");
        let args = vec![
            "manage".into(),
            "add-icr-cred-to-global-pull-secret".into(),
            format!("--entitled-registry-key={key}"),
        ];
        match ctx.run_in_cluster("cpd-cli", &args).await {
            Ok(o) if o.success() => {
                ctx.log("entitled registry credential applied (nodes roll to pick it up if it changed)");
                ctx.progress(100);
                StepOutcome::Completed
            }
            Ok(o) => StepOutcome::Failed {
                error: format!(
                    "add-icr-cred-to-global-pull-secret failed (exit {}): {}",
                    o.status,
                    o.stderr.trim()
                ),
                next_steps: vec![
                    "Verify the IBM entitlement key is valid (My IBM → Container software library).".into(),
                    "Ensure a container runtime is running and `oc` has an active cluster session, then retry.".into(),
                ],
            },
            Err(e) => StepOutcome::Failed {
                error: format!("could not run cpd-cli: {e}"),
                next_steps: vec!["Ensure `cpd-cli` is installed and on PATH, then retry.".into()],
            },
        }
    }
}

/// Install the operators (apply-olm). Idempotent: if operators already report
/// success, skip.
struct InstallOperators;

#[async_trait]
impl Step for InstallOperators {
    fn id(&self) -> &str {
        "install-operators"
    }
    fn title(&self) -> &str {
        "Install operators"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        let operators_ns = input_or(ctx, "PROJECT_CPD_INST_OPERATORS", "cpd-operators");
        let version = input_or(ctx, "VERSION", DEFAULT_VERSION);

        // check-then-act: are operators already reconciled?
        if let Ok(o) = ctx
            .run_in_cluster("oc", &["get".into(), "csv".into(), "-n".into(), operators_ns.clone()])
            .await
        {
            if o.success() && o.stdout.contains("Succeeded") {
                ctx.log("operators already installed; skipping apply-olm");
                ctx.progress(100);
                return StepOutcome::Completed;
            }
        }

        ctx.log(format!("applying OLM for release {version}"));
        let args = vec![
            "manage".into(),
            "apply-olm".into(),
            format!("--release={version}"),
            format!("--cpd_operator_ns={operators_ns}"),
        ];
        match ctx.run_in_cluster("cpd-cli", &args).await {
            Ok(o) if o.success() => {
                ctx.progress(100);
                StepOutcome::Completed
            }
            Ok(o) => StepOutcome::Failed {
                error: format!("apply-olm failed (exit {}): {}", o.status, o.stderr.trim()),
                next_steps: vec![
                    "Check operator namespace quotas and the entitlement key, then retry.".into(),
                ],
            },
            Err(e) => StepOutcome::Failed {
                error: format!("could not run cpd-cli: {e}"),
                next_steps: vec!["Ensure `cpd-cli` is installed and on PATH, then retry.".into()],
            },
        }
    }
}

/// Install the platform control plane (apply-cr for cpd_platform). Idempotent:
/// if the ZenService is already Completed, skip.
struct InstallControlPlane;

#[async_trait]
impl Step for InstallControlPlane {
    fn id(&self) -> &str {
        "install-control-plane"
    }
    fn title(&self) -> &str {
        "Install control plane"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        let operands_ns = input_or(ctx, "PROJECT_CPD_INST_OPERANDS", "cpd-instance");
        let operators_ns = input_or(ctx, "PROJECT_CPD_INST_OPERATORS", "cpd-operators");
        let version = input_or(ctx, "VERSION", DEFAULT_VERSION);

        if let Ok(o) = ctx
            .run_in_cluster("oc", &["get".into(), "ZenService".into(), "-n".into(), operands_ns.clone()])
            .await
        {
            if o.success() && o.stdout.contains("Completed") {
                ctx.log("control plane already installed; skipping");
                ctx.progress(100);
                return StepOutcome::Completed;
            }
        }

        ctx.log("applying platform control plane (cpd_platform)");
        let args = vec![
            "manage".into(),
            "apply-cr".into(),
            format!("--release={version}"),
            "--components=cpd_platform".into(),
            format!("--cpd_instance_ns={operands_ns}"),
            format!("--cpd_operator_ns={operators_ns}"),
        ];
        match ctx.run_in_cluster("cpd-cli", &args).await {
            Ok(o) if o.success() => {
                ctx.progress(100);
                StepOutcome::Completed
            }
            Ok(o) => StepOutcome::Failed {
                error: format!("apply-cr (cpd_platform) failed (exit {}): {}", o.status, o.stderr.trim()),
                next_steps: vec![
                    "Confirm operators reconciled and storage classes exist, then retry.".into(),
                ],
            },
            Err(e) => StepOutcome::Failed {
                error: format!("could not run cpd-cli: {e}"),
                next_steps: vec!["Ensure `cpd-cli` is installed and on PATH, then retry.".into()],
            },
        }
    }
}

/// Wait for the control plane to report ready. Returns Failed (retry-able) if
/// not yet ready, so the orchestrator's retry re-checks at the next attempt.
struct WaitReady;

#[async_trait]
impl Step for WaitReady {
    fn id(&self) -> &str {
        "wait-ready"
    }
    fn title(&self) -> &str {
        "Wait for readiness"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        let operands_ns = input_or(ctx, "PROJECT_CPD_INST_OPERANDS", "cpd-instance");
        ctx.log("checking control-plane readiness");
        match ctx
            .run_in_cluster(
                "oc",
                &[
                    "get".into(),
                    "ZenService".into(),
                    "lite-cr".into(),
                    "-n".into(),
                    operands_ns,
                    "-o".into(),
                    "jsonpath={.status.zenStatus}".into(),
                ],
            )
            .await
        {
            Ok(o) if o.success() && o.stdout.contains("Completed") => {
                ctx.progress(100);
                StepOutcome::Completed
            }
            Ok(o) => StepOutcome::Failed {
                error: format!("control plane not ready yet (status: {})", o.stdout.trim()),
                next_steps: vec![
                    "Installation is still reconciling. Wait a few minutes, then Retry this step."
                        .into(),
                ],
            },
            Err(e) => StepOutcome::Failed {
                error: format!("could not query readiness: {e}"),
                next_steps: vec!["Ensure `oc` has an active session, then retry.".into()],
            },
        }
    }
}

// ---- module ---------------------------------------------------------------

/// The IBM Software Hub install module.
pub struct SoftwareHubModule;

impl Module for SoftwareHubModule {
    fn id(&self) -> &str {
        "mod-softwarehub"
    }
    fn title(&self) -> &str {
        "Install IBM Software Hub"
    }
    fn steps(&self) -> Vec<Box<dyn Step>> {
        vec![
            Box::new(PreflightHub),
            Box::new(AddEntitlement),
            Box::new(InstallOperators),
            Box::new(InstallControlPlane),
            Box::new(WaitReady),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use sw_core::{EventBus, MockCommandRunner, MockResponse};

    fn ctx_with(
        runner: MockCommandRunner,
        inputs: &[(&str, &str)],
        secrets: &[(&str, &str)],
    ) -> StepContext {
        let inputs: BTreeMap<String, String> =
            inputs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let secrets: BTreeMap<String, String> =
            secrets.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        StepContext::with_artifacts(
            "run".into(),
            "mod-softwarehub/x".into(),
            Arc::new(runner),
            EventBus::new(),
            inputs,
            secrets,
            std::env::temp_dir(),
        )
    }

    #[test]
    fn module_exposes_steps_in_order() {
        let ids: Vec<_> = SoftwareHubModule.steps().iter().map(|s| s.id().to_string()).collect();
        assert_eq!(
            ids,
            vec![
                "preflight-hub",
                "entitle-registry",
                "install-operators",
                "install-control-plane",
                "wait-ready"
            ]
        );
    }

    #[tokio::test]
    async fn entitle_registry_applies_icr_cred() {
        let runner = MockCommandRunner::new(vec![MockResponse::ok(
            "add-icr-cred-to-global-pull-secret",
            "updated",
        )]);
        let ctx = ctx_with(runner, &[], &[("IBM_ENTITLEMENT_KEY", "k")]);
        assert_eq!(AddEntitlement.run(&ctx).await, StepOutcome::Completed);
    }

    #[tokio::test]
    async fn entitle_registry_needs_key_when_absent() {
        let ctx = ctx_with(MockCommandRunner::new(vec![]), &[], &[]);
        match AddEntitlement.run(&ctx).await {
            StepOutcome::NeedsInput { fields, .. } => {
                assert_eq!(fields[0].key, "IBM_ENTITLEMENT_KEY");
            }
            o => panic!("expected NeedsInput, got {o:?}"),
        }
    }

    #[tokio::test]
    async fn preflight_fails_without_session() {
        // tools ok, but `oc whoami` fails.
        let runner = MockCommandRunner::new(vec![
            MockResponse::ok("cpd-cli version", "1.0"),
            MockResponse::ok("oc version", "4.x"),
            MockResponse::fail("oc whoami", 1, "not logged in"),
        ]);
        let ctx = ctx_with(runner, &[], &[("IBM_ENTITLEMENT_KEY", "k")]);
        match PreflightHub.run(&ctx).await {
            StepOutcome::Failed { next_steps, .. } => assert!(!next_steps.is_empty()),
            o => panic!("expected Failed, got {o:?}"),
        }
    }

    #[tokio::test]
    async fn preflight_needs_entitlement_when_absent() {
        let runner = MockCommandRunner::new(vec![
            MockResponse::ok("cpd-cli version", "1.0"),
            MockResponse::ok("oc version", "4.x"),
            MockResponse::ok("oc whoami", "admin"),
        ]);
        let ctx = ctx_with(runner, &[], &[]);
        match PreflightHub.run(&ctx).await {
            StepOutcome::NeedsInput { fields, .. } => {
                assert_eq!(fields[0].key, "IBM_ENTITLEMENT_KEY");
                assert!(fields[0].secret);
            }
            o => panic!("expected NeedsInput, got {o:?}"),
        }
    }

    #[tokio::test]
    async fn operators_skip_when_already_succeeded() {
        let runner = MockCommandRunner::new(vec![MockResponse::ok("get csv", "my-op Succeeded")]);
        let ctx = ctx_with(runner, &[("PROJECT_CPD_INST_OPERATORS", "ops")], &[]);
        assert_eq!(InstallOperators.run(&ctx).await, StepOutcome::Completed);
    }

    #[tokio::test]
    async fn operators_apply_when_not_present() {
        // get csv returns nothing useful; apply-olm succeeds.
        let runner = MockCommandRunner::new(vec![
            MockResponse::ok("get csv", ""),
            MockResponse::ok("apply-olm", "done"),
        ]);
        let ctx = ctx_with(runner, &[], &[]);
        assert_eq!(InstallOperators.run(&ctx).await, StepOutcome::Completed);
    }

    #[tokio::test]
    async fn control_plane_failure_reports_next_steps() {
        let runner = MockCommandRunner::new(vec![
            MockResponse::ok("get ZenService", ""),
            MockResponse::fail("apply-cr", 2, "storage class missing"),
        ]);
        let ctx = ctx_with(runner, &[], &[]);
        match InstallControlPlane.run(&ctx).await {
            StepOutcome::Failed { next_steps, .. } => assert!(!next_steps.is_empty()),
            o => panic!("expected Failed, got {o:?}"),
        }
    }

    #[tokio::test]
    async fn wait_ready_completes_when_status_completed() {
        let runner = MockCommandRunner::new(vec![MockResponse::ok("ZenService", "Completed")]);
        let ctx = ctx_with(runner, &[], &[]);
        assert_eq!(WaitReady.run(&ctx).await, StepOutcome::Completed);
    }

    #[tokio::test]
    async fn wait_ready_fails_retryable_when_not_completed() {
        let runner = MockCommandRunner::new(vec![MockResponse::ok("ZenService", "InProgress")]);
        let ctx = ctx_with(runner, &[], &[]);
        match WaitReady.run(&ctx).await {
            StepOutcome::Failed { next_steps, .. } => {
                assert!(next_steps.iter().any(|s| s.contains("Retry")))
            }
            o => panic!("expected Failed, got {o:?}"),
        }
    }
}
