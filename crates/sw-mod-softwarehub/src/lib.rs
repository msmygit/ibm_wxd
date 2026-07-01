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
fn scheduler_ns(ctx: &StepContext) -> String {
    input_or(ctx, "PROJECT_SCHEDULING_SERVICE", "ibm-cpd-scheduler")
}
fn scheduler_br_ns(ctx: &StepContext) -> String {
    input_or(ctx, "PROJECT_SCHEDULING_BR_SVC", "ibm-cpd-scheduler-br-svc")
}
fn license_ns(ctx: &StepContext) -> String {
    input_or(ctx, "PROJECT_LICENSE_SERVICE", "ibm-licensing")
}
fn patch_id(ctx: &StepContext) -> String {
    input_or(ctx, "PATCH_ID", "latest")
}
/// icr.io by default (the IBM Entitled Registry); a private registry otherwise.
fn image_pull_prefix(ctx: &StepContext) -> String {
    input_or(ctx, "IMAGE_PULL_PREFIX", "icr.io")
}
fn image_pull_secret(ctx: &StepContext) -> String {
    input_or(ctx, "IMAGE_PULL_SECRET", "ibm-entitlement-key")
}
fn oadp_ns(ctx: &StepContext) -> String {
    input_or(ctx, "OADP_PROJECT", "openshift-adp")
}

/// The platform component installed by `install-components` for the instance —
/// the control plane. Shared components (License Service, scheduler, backup) are
/// installed by their own commands, NOT install-components. Override via the
/// `platform_components` input to add more instance components.
fn platform_components(ctx: &StepContext) -> String {
    input_or(ctx, "platform_components", "cpd_platform")
}

/// Whether an optional shared component is opted in (default off). The scheduling
/// service and Backup/Restore Orchestration service are optional and installed
/// via `apply-scheduler`/`apply-br` (the latter also needs OADP).
fn install_scheduler(ctx: &StepContext) -> bool {
    input_or(ctx, "install_scheduler", "false") == "true"
}
fn install_br(ctx: &StepContext) -> bool {
    input_or(ctx, "install_br", "false") == "true"
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

/// Environment for `cpd-cli manage`, per the IBM installation-variables script.
/// `VERSION` pins the `olm-utils-v4` image (and thus the installable release);
/// `PATCH_ID` selects the patch (`latest` by default); `OPENSHIFT_TYPE`/
/// `IMAGE_ARCH` describe the cluster. `OLM_UTILS_IMAGE` optionally overrides the
/// image (e.g. the Premium cartridge image); by default cpd-cli derives it from
/// `VERSION` (`icr.io/cpopen/cpd/olm-utils-v4:${VERSION}`).
pub fn cpd_env(ctx: &StepContext) -> Vec<(String, String)> {
    let v = version(ctx);
    // OLM_UTILS_IMAGE MUST be set explicitly: cpd-cli does not switch the
    // olm-utils image from VERSION alone (it reuses/recreates the container with
    // its baked-in default otherwise), so `apply-*`/`install-components` would run
    // against the wrong release. Default to the documented icr.io/cpopen path
    // derived from VERSION; override via the OLM_UTILS_IMAGE input (Premium image
    // or a private registry).
    let olm_image = ctx
        .input("OLM_UTILS_IMAGE")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("icr.io/cpopen/cpd/olm-utils-v4:{v}"));
    vec![
        ("VERSION".to_string(), v),
        ("PATCH_ID".to_string(), input_or(ctx, "PATCH_ID", "latest")),
        ("OPENSHIFT_TYPE".to_string(), input_or(ctx, "OPENSHIFT_TYPE", "self-managed")),
        ("IMAGE_ARCH".to_string(), input_or(ctx, "IMAGE_ARCH", "amd64")),
        ("OLM_UTILS_IMAGE".to_string(), olm_image),
    ]
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

/// Restart the olm-utils container so it runs the image matching the requested
/// Software Hub `VERSION`. cpd-cli pins the release by the `VERSION` env var and
/// reuses an already-running container, so without this a stale container (e.g.
/// a default 5.3.x image) would silently install the wrong release.
struct RestartContainer;

#[async_trait]
impl Step for RestartContainer {
    fn id(&self) -> &str {
        "restart-container"
    }
    fn title(&self) -> &str {
        "Load olm-utils image for the release"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        let v = version(ctx);
        ctx.log(format!("loading the olm-utils image for Software Hub {v} (VERSION={v})"));
        let args = vec!["manage".into(), "restart-container".into()];
        match ctx.run_in_cluster_pty_env("cpd-cli", &args, &cpd_env(ctx), &[]).await {
            Ok(o) if o.success() => {
                ctx.progress(100);
                StepOutcome::Completed
            }
            Ok(o) => fail(
                &format!("restart-container failed (exit {}): {}", o.status, o.stderr.trim()),
                "Ensure a container runtime is running and can pull the olm-utils image, then retry.",
            ),
            Err(e) => fail(&format!("could not run cpd-cli: {e}"), "Ensure `cpd-cli` is installed and on PATH, then retry."),
        }
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
        match ctx.run_in_cluster_pty_env("cpd-cli", &args, &cpd_env(ctx), &mask).await {
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
        match ctx.run_in_cluster_pty_env("cpd-cli", &args, &cpd_env(ctx), &[]).await {
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

/// Wait for every node to be `Ready` after the entitlement credential updated
/// the global pull secret. That update triggers a MachineConfig rollout — nodes
/// briefly become `Ready,SchedulingDisabled` and reboot — and installs must not
/// proceed until it finishes. Retry-able so the orchestrator re-checks.
struct WaitNodesReady;

#[async_trait]
impl Step for WaitNodesReady {
    fn id(&self) -> &str {
        "wait-nodes-ready"
    }
    fn title(&self) -> &str {
        "Wait for nodes (pull-secret rollout)"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        ctx.log("checking that all nodes are Ready after the pull-secret update");
        match ctx.run_in_cluster("oc", &["get".into(), "nodes".into(), "--no-headers".into()]).await {
            Ok(o) if o.success() => {
                let lines: Vec<&str> = o.stdout.lines().filter(|l| !l.trim().is_empty()).collect();
                if lines.is_empty() {
                    return StepOutcome::Failed {
                        error: "no nodes reported".into(),
                        next_steps: vec!["Ensure `oc` has an active session against the cluster, then retry.".into()],
                    };
                }
                // STATUS column: "Ready" | "Ready,SchedulingDisabled" | "NotReady" …
                let not_ready: Vec<String> = lines
                    .iter()
                    .filter_map(|l| {
                        let mut f = l.split_whitespace();
                        let name = f.next().unwrap_or("");
                        let status = f.next().unwrap_or("");
                        (status != "Ready").then(|| format!("{name}={status}"))
                    })
                    .collect();
                if not_ready.is_empty() {
                    ctx.log(format!("all {} nodes Ready", lines.len()));
                    ctx.progress(100);
                    StepOutcome::Completed
                } else {
                    StepOutcome::Failed {
                        error: format!("nodes still rolling out the pull secret: {}", not_ready.join(", ")),
                        next_steps: vec![
                            "The cluster is applying the updated global pull secret (MachineConfig rollout). Wait a few minutes, then Retry.".into(),
                        ],
                    }
                }
            }
            Ok(o) => fail(&format!("could not get nodes (exit {}): {}", o.status, o.stderr.trim()), "Ensure `oc` has an active session, then retry."),
            Err(e) => fail(&format!("could not run oc: {e}"), "Ensure `oc` is installed and on PATH, then retry."),
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
        let mut namespaces = vec![operators_ns(ctx), operands_ns(ctx)];
        if install_scheduler(ctx) {
            namespaces.push(scheduler_ns(ctx));
        }
        if install_br(ctx) {
            namespaces.push(scheduler_br_ns(ctx));
        }
        for ns in namespaces {
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

/// Generate the cluster-scoped resources (CRDs, ClusterRoles, webhooks) for a
/// component via `case-download --cluster_resources=true`, then apply the
/// resulting `cluster_scoped_resources.yaml` with `oc apply --server-side
/// --force-conflicts`. `dl_args` is the component-specific case-download args.
async fn generate_and_apply_cluster_resources(
    ctx: &StepContext,
    dl_args: Vec<String>,
    what: &str,
) -> StepOutcome {
    ctx.log(format!("generating cluster-scoped resources for {what}"));
    match ctx.run_in_cluster_pty_env("cpd-cli", &dl_args, &cpd_env(ctx), &[]).await {
        Ok(o) if o.success() => {}
        Ok(o) => return fail(&format!("case-download (cluster resources, {what}) failed (exit {}): {}", o.status, o.stderr.trim()),
            "Confirm network access to the IBM CASE repository, then retry."),
        Err(e) => return fail(&format!("could not run cpd-cli: {e}"), "Ensure `cpd-cli` is installed and on PATH, then retry."),
    }
    // Find the generated file (cpd-cli writes it under the workspace `work` dir)
    // and apply it server-side, per the docs.
    let script = "set -e; F=$(find cpd-cli-workspace -name cluster_scoped_resources.yaml 2>/dev/null | head -1); \
                  test -n \"$F\"; oc apply -f \"$F\" --server-side --force-conflicts"
        .to_string();
    ctx.log(format!("applying cluster-scoped resources for {what}"));
    match ctx.run_in_cluster("sh", &["-c".to_string(), script]).await {
        Ok(o) if o.success() => {
            ctx.progress(100);
            StepOutcome::Completed
        }
        Ok(o) => fail(&format!("oc apply (cluster resources, {what}) failed (exit {}): {}", o.status, o.stderr.trim()),
            "Ensure cluster-admin access and that cluster_scoped_resources.yaml was generated, then retry."),
        Err(e) => fail(&format!("could not run oc: {e}"), "Ensure `oc` has an active session, then retry."),
    }
}

/// Create the scheduling service's cluster-scoped resources (only when the
/// `scheduler` component is selected).
struct SchedulerClusterResources;

#[async_trait]
impl Step for SchedulerClusterResources {
    fn id(&self) -> &str {
        "scheduler-cluster-resources"
    }
    fn title(&self) -> &str {
        "Cluster resources: scheduling service"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        if !install_scheduler(ctx) {
            ctx.log("scheduler not selected; skipping its cluster-scoped resources");
            ctx.progress(100);
            return StepOutcome::Completed;
        }
        let args = vec![
            "manage".into(),
            "case-download".into(),
            "--components=scheduler".into(),
            format!("--release={}", version(ctx)),
            format!("--patch_id={}", patch_id(ctx)),
            format!("--scheduler_ns={}", scheduler_ns(ctx)),
            "--cluster_resources=true".into(),
        ];
        generate_and_apply_cluster_resources(ctx, args, "the scheduling service").await
    }
}

/// Create the Backup/Restore Orchestration service's cluster-scoped resources
/// (only when the `br_orchestration` component is selected).
struct BrClusterResources;

#[async_trait]
impl Step for BrClusterResources {
    fn id(&self) -> &str {
        "br-cluster-resources"
    }
    fn title(&self) -> &str {
        "Cluster resources: backup/restore orchestration"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        if !install_br(ctx) {
            ctx.log("br_orchestration not selected; skipping its cluster-scoped resources");
            ctx.progress(100);
            return StepOutcome::Completed;
        }
        let br_ns = scheduler_br_ns(ctx);
        let args = vec![
            "manage".into(),
            "case-download".into(),
            "--components=br_orchestration".into(),
            format!("--release={}", version(ctx)),
            format!("--patch_id={}", patch_id(ctx)),
            format!("--operator_ns={br_ns}"),
            format!("--br_operator_ns={br_ns}"),
            "--cluster_resources=true".into(),
        ];
        generate_and_apply_cluster_resources(ctx, args, "backup/restore orchestration").await
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
        ctx.log(format!("installing the License Service (apply-cluster-components) for release {version}"));
        let args = vec![
            "manage".into(),
            "apply-cluster-components".into(),
            format!("--release={version}"),
            format!("--patch_id={}", patch_id(ctx)),
            "--license_acceptance=true".into(),
            format!("--licensing_ns={}", license_ns(ctx)),
            "--case_download=true".into(),
        ];
        match ctx.run_in_cluster_pty_env("cpd-cli", &args, &cpd_env(ctx), &[]).await {
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

/// Create the image pull secret (entitled registry creds) in `namespace`. Needed
/// by Helm-based shared components (scheduler, backup). Idempotent via apply.
async fn create_image_pull_secret(ctx: &StepContext, namespace: &str) -> StepOutcome {
    let key = match ctx.secret("IBM_ENTITLEMENT_KEY") {
        Some(k) if !k.is_empty() => k.to_string(),
        _ => return fail("IBM entitlement key not available for the image pull secret", "Provide the IBM entitlement key, then retry."),
    };
    let name = image_pull_secret(ctx);
    ctx.log(format!("creating image pull secret {name} in {namespace}"));
    // Build the dockerconfig for cp.icr.io + icr.io and apply idempotently. The
    // key is passed via env ($KEY) so it is never echoed into the live log.
    let script = format!(
        "set -e; CRED=$(printf '%s' \"cp:$KEY\" | base64 | tr -d '\\n'); \
         DC=$(mktemp); printf '{{\"auths\":{{\"cp.icr.io\":{{\"auth\":\"%s\"}},\"icr.io\":{{\"auth\":\"%s\"}}}}}}' \"$CRED\" \"$CRED\" > \"$DC\"; \
         oc create secret docker-registry {name} --from-file=.dockerconfigjson=\"$DC\" --namespace={namespace} \
           --dry-run=client -o yaml | oc apply -f -; rm -f \"$DC\"",
        name = name,
        namespace = namespace,
    );
    let kc = ctx.kubeconfig_path().to_string_lossy().into_owned();
    let env = [("KUBECONFIG".to_string(), kc), ("KEY".to_string(), key)];
    match ctx.run_with_env("sh", &["-c".to_string(), script], &env).await {
        Ok(o) if o.success() => {
            ctx.progress(100);
            StepOutcome::Completed
        }
        Ok(o) => fail(&format!("creating image pull secret in {namespace} failed (exit {}): {}", o.status, o.stderr.trim()),
            "Check cluster permissions and the entitlement key, then retry."),
        Err(e) => fail(&format!("could not run oc: {e}"), "Ensure `oc` has an active session, then retry."),
    }
}

/// Optional: install the scheduling service (`apply-scheduler`). Off by default;
/// enable with the `install_scheduler` input. Creates its image pull secret then
/// runs apply-scheduler.
struct ApplyScheduler;

#[async_trait]
impl Step for ApplyScheduler {
    fn id(&self) -> &str {
        "apply-scheduler"
    }
    fn title(&self) -> &str {
        "Install scheduling service"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        if !install_scheduler(ctx) {
            ctx.log("scheduling service not selected; skipping");
            ctx.progress(100);
            return StepOutcome::Completed;
        }
        let sched = scheduler_ns(ctx);
        match create_image_pull_secret(ctx, &sched).await {
            StepOutcome::Completed => {}
            other => return other,
        }
        ctx.log(format!("installing the scheduling service ({sched})"));
        let args = vec![
            "manage".into(),
            "apply-scheduler".into(),
            "--license_acceptance=true".into(),
            format!("--release={}", version(ctx)),
            format!("--patch_id={}", patch_id(ctx)),
            format!("--scheduler_ns={sched}"),
            format!("--image_pull_prefix={}", image_pull_prefix(ctx)),
            format!("--image_pull_secret={}", image_pull_secret(ctx)),
        ];
        match ctx.run_in_cluster_pty_env("cpd-cli", &args, &cpd_env(ctx), &[]).await {
            Ok(o) if o.success() => {
                ctx.progress(100);
                StepOutcome::Completed
            }
            Ok(o) => fail(&format!("apply-scheduler failed (exit {}): {}", o.status, o.stderr.trim()),
                "Confirm the scheduler cluster-scoped resources and pull secret exist, then retry."),
            Err(e) => fail(&format!("could not run cpd-cli: {e}"), "Ensure `cpd-cli` is installed and on PATH, then retry."),
        }
    }
}

/// Optional: install the Backup/Restore Orchestration service for the scheduler
/// (`apply-br`, needs OADP). Off by default; enable with the `install_br` input.
struct ApplyBr;

#[async_trait]
impl Step for ApplyBr {
    fn id(&self) -> &str {
        "apply-br"
    }
    fn title(&self) -> &str {
        "Install backup/restore orchestration"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        if !install_br(ctx) {
            ctx.log("backup/restore orchestration not selected; skipping");
            ctx.progress(100);
            return StepOutcome::Completed;
        }
        let br_ns = scheduler_br_ns(ctx);
        match create_image_pull_secret(ctx, &br_ns).await {
            StepOutcome::Completed => {}
            other => return other,
        }
        ctx.log(format!("installing backup/restore orchestration ({br_ns}); OADP in {}", oadp_ns(ctx)));
        let args = vec![
            "manage".into(),
            "apply-br".into(),
            "--license_acceptance=true".into(),
            format!("--release={}", version(ctx)),
            format!("--patch_id={}", patch_id(ctx)),
            "--br_tool=oadp".into(),
            format!("--oadp_ns={}", oadp_ns(ctx)),
            format!("--scheduler_ns={}", scheduler_ns(ctx)),
            format!("--br_operator_ns={br_ns}"),
            format!("--image_pull_prefix={}", image_pull_prefix(ctx)),
            format!("--image_pull_secret={}", image_pull_secret(ctx)),
        ];
        match ctx.run_in_cluster_pty_env("cpd-cli", &args, &cpd_env(ctx), &[]).await {
            Ok(o) if o.success() => {
                ctx.progress(100);
                StepOutcome::Completed
            }
            Ok(o) => fail(&format!("apply-br failed (exit {}): {}", o.status, o.stderr.trim()),
                "Ensure the OADP operator is installed in OADP_PROJECT and the br cluster resources/pull secret exist, then retry."),
            Err(e) => fail(&format!("could not run cpd-cli: {e}"), "Ensure `cpd-cli` is installed and on PATH, then retry."),
        }
    }
}

/// Generate + apply the cluster-scoped resources (CRDs, ClusterRoles, webhooks)
/// for the instance's components before `install-components`, per the docs.
struct InstanceClusterResources;

#[async_trait]
impl Step for InstanceClusterResources {
    fn id(&self) -> &str {
        "instance-cluster-resources"
    }
    fn title(&self) -> &str {
        "Cluster resources: platform + services"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        let components = platform_components(ctx);
        let args = vec![
            "manage".into(),
            "case-download".into(),
            format!("--components={components}"),
            format!("--release={}", version(ctx)),
            format!("--patch_id={}", patch_id(ctx)),
            format!("--operator_ns={}", operators_ns(ctx)),
            "--cluster_resources=true".into(),
        ];
        generate_and_apply_cluster_resources(ctx, args, "the platform").await
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

        let components = platform_components(ctx);
        if let Some(outcome) = case_download(ctx, &version, &components).await {
            return outcome;
        }

        ctx.log(format!("installing platform [{components}] (release {version}); block={block_sc}, file={file_sc}"));
        let args = vec![
            "manage".into(),
            "install-components".into(),
            "--license_acceptance=true".into(),
            format!("--components={components}"),
            format!("--release={version}"),
            format!("--operator_ns={op_ns}"),
            format!("--instance_ns={inst_ns}"),
            format!("--block_storage_class={block_sc}"),
            format!("--file_storage_class={file_sc}"),
        ];
        match ctx.run_in_cluster_pty_env("cpd-cli", &args, &cpd_env(ctx), &[]).await {
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
    match ctx.run_in_cluster_pty_env("cpd-cli", &args, &cpd_env(ctx), &[]).await {
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
            Box::new(RestartContainer),
            Box::new(LoginToOcp),
            Box::new(AddEntitlement),
            Box::new(WaitNodesReady),
            Box::new(InstallCertManager),
            Box::new(CreateNamespaces),
            Box::new(ApplyClusterComponents),
            Box::new(SchedulerClusterResources),
            Box::new(ApplyScheduler),
            Box::new(BrClusterResources),
            Box::new(ApplyBr),
            Box::new(InstanceClusterResources),
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
                "restart-container",
                "login-to-ocp",
                "entitle-registry",
                "wait-nodes-ready",
                "install-cert-manager",
                "create-namespaces",
                "apply-cluster-components",
                "scheduler-cluster-resources",
                "apply-scheduler",
                "br-cluster-resources",
                "apply-br",
                "instance-cluster-resources",
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
    async fn wait_nodes_ready_passes_when_all_ready() {
        let runner = MockCommandRunner::new(vec![MockResponse::ok(
            "get nodes",
            "ip-1 Ready worker 20m v1.30\nip-2 Ready master 20m v1.30",
        )]);
        let ctx = ctx_with(runner, &[], &[]);
        assert_eq!(WaitNodesReady.run(&ctx).await, StepOutcome::Completed);
    }

    #[tokio::test]
    async fn wait_nodes_ready_fails_when_scheduling_disabled() {
        let runner = MockCommandRunner::new(vec![MockResponse::ok(
            "get nodes",
            "ip-1 Ready worker 20m v1.30\nip-2 Ready,SchedulingDisabled master 20m v1.30",
        )]);
        let ctx = ctx_with(runner, &[], &[]);
        match WaitNodesReady.run(&ctx).await {
            StepOutcome::Failed { error, .. } => assert!(error.contains("ip-2")),
            o => panic!("expected Failed, got {o:?}"),
        }
    }

    #[tokio::test]
    async fn cert_manager_skips_when_present() {
        let runner = MockCommandRunner::new(vec![MockResponse::ok("get deployment cert-manager-webhook", "cert-manager-webhook")]);
        let ctx = ctx_with(runner, &[], &[]);
        assert_eq!(InstallCertManager.run(&ctx).await, StepOutcome::Completed);
    }

    #[tokio::test]
    async fn scheduler_cluster_resources_skips_by_default() {
        // Scheduler is opt-in (install_scheduler defaults false) → skip.
        let ctx = ctx_with(MockCommandRunner::new(vec![]), &[], &[]);
        assert_eq!(SchedulerClusterResources.run(&ctx).await, StepOutcome::Completed);
    }

    #[tokio::test]
    async fn scheduler_cluster_resources_runs_when_opted_in() {
        let runner = MockCommandRunner::new(vec![
            MockResponse::ok("case-download", "ok"),
            MockResponse::ok("apply", "applied"),
        ]);
        let ctx = ctx_with(runner, &[("install_scheduler", "true")], &[]);
        assert_eq!(SchedulerClusterResources.run(&ctx).await, StepOutcome::Completed);
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
