//! IBM Software Hub / Cloud Pak for Data install module.
//!
//! Drives the online install through `cpd-cli manage` + `oc`, following the
//! current (5.3.x+) command flow — `apply-olm`/`apply-cr` are deprecated and
//! replaced by `install-components`. Every step is idempotent (check-then-act)
//! so retry/resume is safe, and every external command goes through the
//! `CommandRunner` seam (hermetic in tests). `cpd-cli manage` execs into a local
//! olm-utils container with a TTY, so those calls use `run_in_cluster_pty`.
//!
//! Validated flow (per a live run):
//!   preflight → login-to-ocp → entitle-registry → cert-manager → namespaces →
//!   apply-cluster-components → install-platform (case-download + install-components
//!   cpd_platform) → wait-ready.

use async_trait::async_trait;
use sw_core::{InputField, Module, Step, StepContext, StepOutcome};

/// Default Software Hub release. The actual installable version is bounded by the
/// installed `cpd-cli`/olm-utils image; override via the `VERSION` input.
const DEFAULT_VERSION: &str = "5.4.0";

/// Convenience: read an input or fall back to a default.
fn input_or<'a>(ctx: &'a StepContext, key: &str, default: &'a str) -> String {
    ctx.input(key).unwrap_or(default).to_string()
}

fn version(ctx: &StepContext) -> String {
    input_or(ctx, "VERSION", DEFAULT_VERSION)
}
fn operators_ns(ctx: &StepContext) -> String {
    input_or(ctx, "PROJECT_CPD_INST_OPERATORS", "cpd-operators")
}
fn operands_ns(ctx: &StepContext) -> String {
    input_or(ctx, "PROJECT_CPD_INST_OPERANDS", "cpd-instance")
}

/// The (block, file) storage classes for `install-components`. Software Hub +
/// watsonx.data need both a block (RWO) and a file (RWX) class. Defaults match a
/// provisioned AWS cluster: `gp3-csi` (EBS) and `efs-sc` (EFS). Override via the
/// `block_storage_class` / `file_storage_class` inputs (e.g. ODF classes).
fn storage_classes(ctx: &StepContext) -> (String, String) {
    (
        input_or(ctx, "block_storage_class", "gp3-csi"),
        input_or(ctx, "file_storage_class", "efs-sc"),
    )
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
        match ctx.run_in_cluster("oc", &["whoami".to_string()]).await {
            Ok(o) if o.success() => {}
            _ => {
                return StepOutcome::Failed {
                    error: "no active OpenShift session".into(),
                    next_steps: vec![
                        "Provision a cluster or provide an existing kubeconfig/login, then retry.".into(),
                    ],
                }
            }
        }
        if ctx.secret("IBM_ENTITLEMENT_KEY").is_none() {
            return StepOutcome::NeedsInput {
                prompt: "Provide your IBM entitlement key (My IBM → Container software library).".into(),
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

/// Log `cpd-cli manage` into the cluster. All `cpd-cli manage` commands run
/// olm-utils in a container that needs its own OpenShift session (the run's
/// KUBECONFIG is not visible inside that container), so this must run first.
struct LoginToOcp;

#[async_trait]
impl Step for LoginToOcp {
    fn id(&self) -> &str {
        "login-to-ocp"
    }
    fn title(&self) -> &str {
        "Log cpd-cli into the cluster"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        let server = match ctx.run_in_cluster("oc", &["whoami".into(), "--show-server".into()]).await {
            Ok(o) if o.success() => o.stdout.trim().to_string(),
            _ => {
                return StepOutcome::Failed {
                    error: "could not determine the cluster API server URL".into(),
                    next_steps: vec!["Ensure `oc` has an active session against the run's cluster, then retry.".into()],
                }
            }
        };

        let mut args = vec!["manage".into(), "login-to-ocp".into(), format!("--server={server}")];
        let mut mask: Vec<String> = Vec::new();
        // Prefer an explicit token/user/password (existing-cluster path); else
        // fall back to the provisioner's kubeadmin credentials.
        if let Some(token) = ctx.secret("OCP_TOKEN").filter(|t| !t.is_empty()) {
            args.push(format!("--token={token}"));
        } else if let (Some(user), Some(pass)) = (
            ctx.input("OCP_USERNAME").filter(|u| !u.is_empty()),
            ctx.secret("OCP_PASSWORD").filter(|p| !p.is_empty()),
        ) {
            args.push("-u".into());
            args.push(user.to_string());
            args.push("-p".into());
            args.push(pass.to_string());
        } else {
            let pw_path = ctx.artifacts_dir().join("cluster").join("auth").join("kubeadmin-password");
            match std::fs::read_to_string(&pw_path) {
                Ok(pw) if !pw.trim().is_empty() => {
                    let pw = pw.trim().to_string();
                    mask.push(pw.clone());
                    args.push("-u".into());
                    args.push("kubeadmin".into());
                    args.push("-p".into());
                    args.push(pw);
                }
                _ => {
                    return StepOutcome::Failed {
                        error: "no OpenShift credentials available for cpd-cli login".into(),
                        next_steps: vec![
                            "For an existing cluster, provide OCP_TOKEN or OCP_USERNAME/OCP_PASSWORD.".into(),
                            "For a provisioned cluster, ensure auth/kubeadmin-password exists, then retry.".into(),
                        ],
                    }
                }
            }
        }
        args.push("--insecure-skip-tls-verify=true".into());

        ctx.log("logging cpd-cli into the cluster");
        match ctx.run_in_cluster_pty_masking("cpd-cli", &args, &mask).await {
            Ok(o) if o.success() => {
                ctx.progress(100);
                StepOutcome::Completed
            }
            Ok(o) => StepOutcome::Failed {
                error: format!("login-to-ocp failed (exit {}): {}", o.status, o.stderr.trim()),
                next_steps: vec![
                    "Verify the API URL and credentials; ensure a container runtime is running, then retry.".into(),
                ],
            },
            Err(e) => StepOutcome::Failed {
                error: format!("could not run cpd-cli: {e}"),
                next_steps: vec!["Ensure `cpd-cli` is installed and on PATH, then retry.".into()],
            },
        }
    }
}

/// Add the IBM entitled registry (`cp.icr.io`) credential to the cluster's
/// global pull secret. Idempotent (re-applying the same cred is a no-op).
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
            format!("--entitled_registry_key={key}"),
        ];
        match ctx.run_in_cluster_pty("cpd-cli", &args).await {
            Ok(o) if o.success() => {
                ctx.log("entitled registry credential applied");
                ctx.progress(100);
                StepOutcome::Completed
            }
            Ok(o) => StepOutcome::Failed {
                error: format!("add-icr-cred-to-global-pull-secret failed (exit {}): {}", o.status, o.stderr.trim()),
                next_steps: vec![
                    "Verify the IBM entitlement key is valid (it must authenticate to cp.icr.io — it is NOT the IBM Cloud API key).".into(),
                    "Ensure a container runtime is running and cpd-cli is logged in, then retry.".into(),
                ],
            },
            Err(e) => StepOutcome::Failed {
                error: format!("could not run cpd-cli: {e}"),
                next_steps: vec!["Ensure `cpd-cli` is installed and on PATH, then retry.".into()],
            },
        }
    }
}

/// Install the Red Hat cert-manager Operator, a prerequisite for
/// `apply-cluster-components`. Idempotent.
struct InstallCertManager;

const CERT_MANAGER_YAML: &str = "\
apiVersion: v1
kind: Namespace
metadata:
  name: cert-manager-operator
---
apiVersion: operators.coreos.com/v1
kind: OperatorGroup
metadata:
  name: openshift-cert-manager-operator
  namespace: cert-manager-operator
spec:
  upgradeStrategy: Default
---
apiVersion: operators.coreos.com/v1alpha1
kind: Subscription
metadata:
  name: openshift-cert-manager-operator
  namespace: cert-manager-operator
spec:
  channel: stable-v1
  installPlanApproval: Automatic
  name: openshift-cert-manager-operator
  source: redhat-operators
  sourceNamespace: openshift-marketplace
";

#[async_trait]
impl Step for InstallCertManager {
    fn id(&self) -> &str {
        "install-cert-manager"
    }
    fn title(&self) -> &str {
        "Install cert-manager operator"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        // Idempotency: cert-manager webhook already running?
        if let Ok(o) = ctx
            .run_in_cluster("oc", &["get".into(), "deployment".into(), "cert-manager-webhook".into(), "-n".into(), "cert-manager".into()])
            .await
        {
            if o.success() {
                ctx.log("cert-manager already present; skipping");
                ctx.progress(100);
                return StepOutcome::Completed;
            }
        }
        let manifest = ctx.artifacts_dir().join("cert-manager.yaml");
        if let Err(e) = std::fs::write(&manifest, CERT_MANAGER_YAML) {
            return fail(&format!("could not write cert-manager manifest: {e}"), "Check artifacts-dir permissions, then retry.");
        }
        ctx.log("installing the Red Hat cert-manager Operator");
        match ctx
            .run_in_cluster("oc", &["apply".into(), "-f".into(), manifest.to_string_lossy().into_owned()])
            .await
        {
            Ok(o) if o.success() => {}
            Ok(o) => return fail(&format!("oc apply (cert-manager) failed (exit {}): {}", o.status, o.stderr.trim()), "Confirm the redhat-operators catalog is available, then retry."),
            Err(e) => return fail(&format!("could not run oc: {e}"), "Ensure `oc` has an active session, then retry."),
        }
        // Wait (best-effort, retryable) for the webhook rollout.
        match ctx
            .run_in_cluster(
                "oc",
                &["rollout".into(), "status".into(), "deployment/cert-manager-webhook".into(), "-n".into(), "cert-manager".into(), "--timeout=180s".into()],
            )
            .await
        {
            Ok(o) if o.success() => {
                ctx.progress(100);
                StepOutcome::Completed
            }
            _ => StepOutcome::Failed {
                error: "cert-manager operator is still rolling out".into(),
                next_steps: vec!["Wait a couple of minutes for cert-manager pods to become ready, then Retry.".into()],
            },
        }
    }
}

/// Create the operator + operand namespaces `install-components` requires.
struct CreateNamespaces;

#[async_trait]
impl Step for CreateNamespaces {
    fn id(&self) -> &str {
        "create-namespaces"
    }
    fn title(&self) -> &str {
        "Create Software Hub namespaces"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        for ns in [operators_ns(ctx), operands_ns(ctx)] {
            // `oc create namespace` is not idempotent; ignore an AlreadyExists.
            if let Ok(o) = ctx.run_in_cluster("oc", &["get".into(), "namespace".into(), ns.clone()]).await {
                if o.success() {
                    continue;
                }
            }
            ctx.log(format!("creating namespace {ns}"));
            match ctx.run_in_cluster("oc", &["create".into(), "namespace".into(), ns.clone()]).await {
                Ok(o) if o.success() => {}
                Ok(o) if o.stderr.contains("AlreadyExists") => {}
                Ok(o) => return fail(&format!("could not create namespace {ns} (exit {}): {}", o.status, o.stderr.trim()), "Check cluster permissions, then retry."),
                Err(e) => return fail(&format!("could not run oc: {e}"), "Ensure `oc` has an active session, then retry."),
            }
        }
        ctx.progress(100);
        StepOutcome::Completed
    }
}

/// Install the shared cluster-scoped components (licensing, scheduler, cert
/// checks) and download the CASE catalog. Idempotent.
struct ApplyClusterComponents;

#[async_trait]
impl Step for ApplyClusterComponents {
    fn id(&self) -> &str {
        "apply-cluster-components"
    }
    fn title(&self) -> &str {
        "Apply shared cluster components"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        let version = version(ctx);
        ctx.log(format!("applying shared cluster components for release {version} (downloading CASE packages)"));
        let args = vec![
            "manage".into(),
            "apply-cluster-components".into(),
            format!("--release={version}"),
            "--license_acceptance=true".into(),
            "--case_download=true".into(),
        ];
        match ctx.run_in_cluster_pty("cpd-cli", &args).await {
            Ok(o) if o.success() => {
                ctx.progress(100);
                StepOutcome::Completed
            }
            Ok(o) => fail(&format!("apply-cluster-components failed (exit {}): {}", o.status, o.stderr.trim()),
                "Confirm cert-manager is installed and the entitlement key is valid, then retry."),
            Err(e) => fail(&format!("could not run cpd-cli: {e}"), "Ensure `cpd-cli` is installed and on PATH, then retry."),
        }
    }
}

/// Install the platform control plane (`cpd_platform`) via `install-components`,
/// downloading its CASE packages first. Idempotent: skips if the ZenService is
/// already Completed.
struct InstallPlatform;

#[async_trait]
impl Step for InstallPlatform {
    fn id(&self) -> &str {
        "install-platform"
    }
    fn title(&self) -> &str {
        "Install Software Hub platform"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        let version = version(ctx);
        let op_ns = operators_ns(ctx);
        let inst_ns = operands_ns(ctx);
        let (block_sc, file_sc) = storage_classes(ctx);

        if let Ok(o) = ctx
            .run_in_cluster("oc", &["get".into(), "ZenService".into(), "-n".into(), inst_ns.clone()])
            .await
        {
            if o.success() && o.stdout.contains("Completed") {
                ctx.log("platform already installed; skipping");
                ctx.progress(100);
                return StepOutcome::Completed;
            }
        }

        if let Some(outcome) = case_download(ctx, &version, "cpd_platform").await {
            return outcome;
        }

        ctx.log(format!("installing cpd_platform (release {version}); block={block_sc}, file={file_sc}"));
        let args = vec![
            "manage".into(),
            "install-components".into(),
            "--license_acceptance=true".into(),
            "--components=cpd_platform".into(),
            format!("--release={version}"),
            format!("--operator_ns={op_ns}"),
            format!("--instance_ns={inst_ns}"),
            format!("--block_storage_class={block_sc}"),
            format!("--file_storage_class={file_sc}"),
        ];
        match ctx.run_in_cluster_pty("cpd-cli", &args).await {
            Ok(o) if o.success() => {
                ctx.progress(100);
                StepOutcome::Completed
            }
            Ok(o) => fail(&format!("install-components (cpd_platform) failed (exit {}): {}", o.status, o.stderr.trim()),
                "Confirm operators reconciled and the storage classes exist, then retry."),
            Err(e) => fail(&format!("could not run cpd-cli: {e}"), "Ensure `cpd-cli` is installed and on PATH, then retry."),
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
        let inst_ns = operands_ns(ctx);
        ctx.log("checking control-plane readiness");
        match ctx
            .run_in_cluster(
                "oc",
                &[
                    "get".into(), "ZenService".into(), "lite-cr".into(),
                    "-n".into(), inst_ns,
                    "-o".into(), "jsonpath={.status.zenStatus}".into(),
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
                next_steps: vec!["Installation is still reconciling. Wait a few minutes, then Retry this step.".into()],
            },
            Err(e) => StepOutcome::Failed {
                error: format!("could not query readiness: {e}"),
                next_steps: vec!["Ensure `oc` has an active session, then retry.".into()],
            },
        }
    }
}

// ---- shared helpers -------------------------------------------------------

/// Download the CASE packages for `components` at `release` (shared by the
/// platform and services installs — `install-components` needs them locally).
/// Returns `Some(Failed)` on error, or `None` on success.
pub async fn case_download(ctx: &StepContext, release: &str, components: &str) -> Option<StepOutcome> {
    ctx.log(format!("downloading CASE packages for [{components}] (release {release})"));
    let args = vec![
        "manage".into(),
        "case-download".into(),
        format!("--release={release}"),
        format!("--components={components}"),
    ];
    match ctx.run_in_cluster_pty("cpd-cli", &args).await {
        Ok(o) if o.success() => None,
        Ok(o) => Some(fail(&format!("case-download failed (exit {}): {}", o.status, o.stderr.trim()),
            "Confirm network access to the IBM CASE repository (or use --from_oci), then retry.")),
        Err(e) => Some(fail(&format!("could not run cpd-cli: {e}"), "Ensure `cpd-cli` is installed and on PATH, then retry.")),
    }
}

/// Build a Failed outcome with one next-step.
pub fn fail(error: &str, next: &str) -> StepOutcome {
    StepOutcome::Failed {
        error: error.to_string(),
        next_steps: vec![next.to_string()],
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
            Box::new(LoginToOcp),
            Box::new(AddEntitlement),
            Box::new(InstallCertManager),
            Box::new(CreateNamespaces),
            Box::new(ApplyClusterComponents),
            Box::new(InstallPlatform),
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

    fn ctx_with(runner: MockCommandRunner, inputs: &[(&str, &str)], secrets: &[(&str, &str)]) -> StepContext {
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
    fn module_exposes_the_install_components_flow_in_order() {
        let ids: Vec<_> = SoftwareHubModule.steps().iter().map(|s| s.id().to_string()).collect();
        assert_eq!(
            ids,
            vec![
                "preflight-hub",
                "login-to-ocp",
                "entitle-registry",
                "install-cert-manager",
                "create-namespaces",
                "apply-cluster-components",
                "install-platform",
                "wait-ready",
            ]
        );
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
            StepOutcome::NeedsInput { fields, .. } => assert_eq!(fields[0].key, "IBM_ENTITLEMENT_KEY"),
            o => panic!("expected NeedsInput, got {o:?}"),
        }
    }

    #[tokio::test]
    async fn login_uses_token_when_present() {
        let runner = MockCommandRunner::new(vec![
            MockResponse::ok("oc whoami --show-server", "https://api.example.com:6443"),
            MockResponse::ok("login-to-ocp", "ok"),
        ]);
        let ctx = ctx_with(runner, &[], &[("OCP_TOKEN", "sha256~tok")]);
        assert_eq!(LoginToOcp.run(&ctx).await, StepOutcome::Completed);
    }

    #[tokio::test]
    async fn entitle_registry_applies_icr_cred() {
        let runner = MockCommandRunner::new(vec![MockResponse::ok("add-icr-cred-to-global-pull-secret", "updated")]);
        let ctx = ctx_with(runner, &[], &[("IBM_ENTITLEMENT_KEY", "k")]);
        assert_eq!(AddEntitlement.run(&ctx).await, StepOutcome::Completed);
    }

    #[tokio::test]
    async fn cert_manager_skips_when_present() {
        let runner = MockCommandRunner::new(vec![MockResponse::ok("get deployment cert-manager-webhook", "cert-manager-webhook")]);
        let ctx = ctx_with(runner, &[], &[]);
        assert_eq!(InstallCertManager.run(&ctx).await, StepOutcome::Completed);
    }

    #[tokio::test]
    async fn install_platform_runs_case_download_then_install_components() {
        let runner = MockCommandRunner::new(vec![
            MockResponse::ok("get ZenService", ""),          // not yet installed
            MockResponse::ok("case-download", "ok"),         // CASE download
            MockResponse::ok("install-components", "ok"),    // platform install
        ]);
        let ctx = ctx_with(runner, &[("VERSION", "5.3.1")], &[]);
        assert_eq!(InstallPlatform.run(&ctx).await, StepOutcome::Completed);
    }

    #[tokio::test]
    async fn install_platform_fails_actionably() {
        let runner = MockCommandRunner::new(vec![
            MockResponse::ok("get ZenService", ""),
            MockResponse::ok("case-download", "ok"),
            MockResponse::fail("install-components", 1, "storage class missing"),
        ]);
        let ctx = ctx_with(runner, &[], &[]);
        match InstallPlatform.run(&ctx).await {
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
            StepOutcome::Failed { next_steps, .. } => assert!(next_steps.iter().any(|s| s.contains("Retry"))),
            o => panic!("expected Failed, got {o:?}"),
        }
    }
}
