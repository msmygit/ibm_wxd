//! `wxd-svc-watsonxdata` — the watsonx.data implementation of the generic
//! [`sw_mod_services::ServiceInstaller`] framework.
//!
//! This is the only crate that knows watsonx.data specifics: the component token
//! `watsonx_data`, the `cpd-cli manage apply-cr` invocation, and the operand
//! readiness check. It does no I/O of its own — every external command goes
//! through `ctx.runner()`, so it is fully hermetic under
//! `sw_core::MockCommandRunner`.

use async_trait::async_trait;
use sw_core::{StepContext, StepOutcome};
use sw_mod_services::ServiceInstaller;

/// Default IBM Software Hub release the CR is applied against. Overridable via
/// the `CPD_RELEASE` input.
const DEFAULT_RELEASE: &str = "5.4.0";
/// The operand resource watsonx.data installs; its presence means "already
/// installed" for the idempotency check.
const OPERAND_RESOURCE: &str = "watsonxdataservice";

/// watsonx.data service installer. Stateless — construct with [`Default`].
#[derive(Debug, Default, Clone, Copy)]
pub struct WatsonxDataInstaller;

impl WatsonxDataInstaller {
    /// Operands namespace the CR lives in. Read from the
    /// `PROJECT_CPD_INST_OPERANDS` input collected earlier in the run; falls
    /// back to the conventional default when unset.
    fn operands_namespace(ctx: &StepContext) -> &str {
        ctx.input("PROJECT_CPD_INST_OPERANDS")
            .unwrap_or("cpd-instance")
    }

    fn release(ctx: &StepContext) -> String {
        ctx.input("CPD_RELEASE")
            .filter(|v| !v.is_empty())
            .unwrap_or(DEFAULT_RELEASE)
            .to_string()
    }
}

#[async_trait]
impl ServiceInstaller for WatsonxDataInstaller {
    fn service_id(&self) -> &str {
        "watsonx-data"
    }

    fn display_name(&self) -> &str {
        "watsonx.data"
    }

    fn component(&self) -> &str {
        "watsonx_data"
    }

    async fn install(&self, ctx: &StepContext) -> StepOutcome {
        let namespace = Self::operands_namespace(ctx);

        // Idempotency: if the operand already exists, this is a resume/retry of
        // an already-applied CR — skip the apply and report success.
        let existing = ctx
            .run_in_cluster(
                "oc",
                &[
                    "get".into(),
                    OPERAND_RESOURCE.into(),
                    "-n".into(),
                    namespace.into(),
                ],
            )
            .await;
        if let Ok(out) = &existing {
            if out.success() {
                ctx.log(format!(
                    "watsonx.data operand already present in {namespace}; skipping apply-cr"
                ));
                return StepOutcome::Completed;
            }
        }

        let release = Self::release(ctx);
        ctx.log(format!(
            "applying watsonx.data CR (component={}, release={release}) in {namespace}",
            self.component()
        ));
        let apply = ctx
            .run_in_cluster(
                "cpd-cli",
                &[
                    "manage".into(),
                    "apply-cr".into(),
                    "--components".into(),
                    self.component().to_string(),
                    format!("--release={release}"),
                    "--cpd_instance_ns".into(),
                    namespace.into(),
                ],
            )
            .await;

        match apply {
            Ok(out) if out.success() => {
                ctx.progress(100);
                StepOutcome::Completed
            }
            Ok(out) => StepOutcome::Failed {
                error: format!(
                    "cpd-cli apply-cr for watsonx.data failed (exit {}): {}",
                    out.status,
                    out.stderr.trim()
                ),
                next_steps: vec![
                    "Confirm the entitlement key and registry mirror are configured".into(),
                    format!("Inspect operator logs in namespace {namespace}"),
                    "Re-run this step to retry (apply-cr is idempotent)".into(),
                ],
            },
            Err(e) => StepOutcome::Failed {
                error: format!("could not invoke cpd-cli: {e}"),
                next_steps: vec![
                    "Ensure cpd-cli is installed and on PATH".into(),
                    "Re-run this step once cpd-cli is available".into(),
                ],
            },
        }
    }

    async fn verify(&self, ctx: &StepContext) -> StepOutcome {
        let namespace = Self::operands_namespace(ctx);
        let check = ctx
            .run_in_cluster(
                "oc",
                &[
                    "get".into(),
                    OPERAND_RESOURCE.into(),
                    "-n".into(),
                    namespace.into(),
                    "-o".into(),
                    "jsonpath={.status.watsonxDataStatus}".into(),
                ],
            )
            .await;

        match check {
            Ok(out) if out.success() && out.stdout.contains("Completed") => {
                ctx.log("watsonx.data reports Completed");
                StepOutcome::Completed
            }
            Ok(out) => StepOutcome::Failed {
                error: format!(
                    "watsonx.data not ready yet (status: {})",
                    if out.stdout.trim().is_empty() {
                        "<none>"
                    } else {
                        out.stdout.trim()
                    }
                ),
                next_steps: vec![
                    "Reconciliation can take 30-60 minutes; wait and retry".into(),
                    format!("Watch progress: oc get {OPERAND_RESOURCE} -n {namespace} -o yaml"),
                ],
            },
            Err(e) => StepOutcome::Failed {
                error: format!("could not query watsonx.data status: {e}"),
                next_steps: vec!["Ensure oc is logged in to the cluster, then retry".into()],
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use sw_core::{EventBus, MockCommandRunner, MockResponse};

    fn ctx(runner: Arc<MockCommandRunner>) -> StepContext {
        ctx_with_inputs(runner, BTreeMap::new())
    }

    fn ctx_with_inputs(
        runner: Arc<MockCommandRunner>,
        inputs: BTreeMap<String, String>,
    ) -> StepContext {
        StepContext::new(
            "run-test".into(),
            "mod-services/install-watsonx-data".into(),
            runner,
            EventBus::new(),
            inputs,
            BTreeMap::new(),
        )
    }

    #[test]
    fn identity_matches_naming_contract() {
        let i = WatsonxDataInstaller;
        assert_eq!(i.service_id(), "watsonx-data");
        assert_eq!(i.display_name(), "watsonx.data");
        assert_eq!(i.component(), "watsonx_data");
    }

    #[tokio::test]
    async fn install_skips_when_operand_already_exists() {
        // First call (oc get) succeeds -> already installed -> no apply-cr.
        let runner = Arc::new(MockCommandRunner::new(vec![MockResponse::ok(
            "get watsonxdataservice",
            "watsonxdataservice/wxd",
        )]));
        let outcome = WatsonxDataInstaller.install(&ctx(runner.clone())).await;
        assert_eq!(outcome, StepOutcome::Completed);
        let calls = runner.calls();
        assert_eq!(calls.len(), 1, "should not call apply-cr: {calls:?}");
        assert!(calls[0].starts_with("oc get watsonxdataservice"));
    }

    #[tokio::test]
    async fn install_applies_cr_when_absent_and_succeeds() {
        // oc get fails (absent) -> apply-cr succeeds.
        let runner = Arc::new(MockCommandRunner::new(vec![
            MockResponse::fail("get watsonxdataservice", 1, "NotFound"),
            MockResponse::ok("apply-cr", "applied"),
        ]));
        let outcome = WatsonxDataInstaller.install(&ctx(runner.clone())).await;
        assert_eq!(outcome, StepOutcome::Completed);
        let calls = runner.calls();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[1].contains("cpd-cli manage apply-cr"));
        assert!(calls[1].contains("--components watsonx_data"));
        assert!(calls[1].contains("--release=5.4.0"));
    }

    #[tokio::test]
    async fn install_honors_custom_namespace_and_release() {
        let mut inputs = BTreeMap::new();
        inputs.insert("PROJECT_CPD_INST_OPERANDS".into(), "wxd-operands".into());
        inputs.insert("CPD_RELEASE".into(), "5.4.1".into());
        let runner = Arc::new(MockCommandRunner::new(vec![
            MockResponse::fail("get watsonxdataservice", 1, "NotFound"),
            MockResponse::ok("apply-cr", "applied"),
        ]));
        let outcome = WatsonxDataInstaller
            .install(&ctx_with_inputs(runner.clone(), inputs))
            .await;
        assert_eq!(outcome, StepOutcome::Completed);
        let calls = runner.calls();
        assert!(calls[0].contains("-n wxd-operands"), "{calls:?}");
        assert!(calls[1].contains("--release=5.4.1"), "{calls:?}");
        assert!(
            calls[1].contains("--cpd_instance_ns wxd-operands"),
            "{calls:?}"
        );
    }

    #[tokio::test]
    async fn install_fails_with_next_steps_when_apply_fails() {
        let runner = Arc::new(MockCommandRunner::new(vec![
            MockResponse::fail("get watsonxdataservice", 1, "NotFound"),
            MockResponse::fail("apply-cr", 2, "registry unreachable"),
        ]));
        let outcome = WatsonxDataInstaller.install(&ctx(runner)).await;
        match outcome {
            StepOutcome::Failed { error, next_steps } => {
                assert!(error.contains("apply-cr"), "{error}");
                assert!(!next_steps.is_empty());
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_completed_when_status_ready() {
        let runner = Arc::new(MockCommandRunner::new(vec![MockResponse::ok(
            "jsonpath",
            "Completed",
        )]));
        let outcome = WatsonxDataInstaller.verify(&ctx(runner)).await;
        assert_eq!(outcome, StepOutcome::Completed);
    }

    #[tokio::test]
    async fn verify_fails_with_retry_steps_when_not_ready() {
        let runner = Arc::new(MockCommandRunner::new(vec![MockResponse::ok(
            "jsonpath",
            "InProgress",
        )]));
        let outcome = WatsonxDataInstaller.verify(&ctx(runner)).await;
        match outcome {
            StepOutcome::Failed { error, next_steps } => {
                assert!(error.contains("not ready"), "{error}");
                assert!(!next_steps.is_empty());
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}
