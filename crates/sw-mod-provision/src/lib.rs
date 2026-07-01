//! `sw-mod-provision` — the OpenShift cluster provisioning module.
//!
//! This module owns getting a freshly-installed OpenShift cluster onto a cloud
//! so that downstream watsonx.data modules (operators, instance creation) have
//! somewhere to run. It is cloud-agnostic at the seam: the [`Provisioner`] trait
//! abstracts "create / status / destroy a cluster", and [`AwsProvisioner`] is
//! the first (v1) implementation, driving Red Hat's `openshift-install` in IPI
//! (installer-provisioned infrastructure) mode. IBM Cloud / Azure / GCP would
//! plug in as additional `Provisioner` impls without touching the steps.
//!
//! Every external command goes through [`sw_core::CommandRunner`] (via
//! `ctx.runner()`), so the whole module is hermetically testable with
//! `sw_core::MockCommandRunner` — no real cloud, no real `openshift-install`.

use async_trait::async_trait;
use sw_core::{
    InputField, Module, Step, StepContext, StepOutcome,
};

/// The cloud-agnostic provisioning seam.
///
/// An implementation knows how to materialize, inspect, and tear down an
/// OpenShift cluster on a specific cloud. All work goes through the
/// [`StepContext`]'s command runner so it stays testable; implementations must
/// never call `std::process` directly.
#[async_trait]
pub trait Provisioner: Send + Sync {
    /// Stable identifier for this provisioner (e.g. `"aws"`). Matches the
    /// hyperscaler id chosen in the UI.
    fn id(&self) -> &str;

    /// Human-readable name shown in the UI (e.g. "Amazon Web Services").
    fn display_name(&self) -> &str {
        self.id()
    }

    /// The cluster-spec fields this cloud needs (region/zone, machine/VM types,
    /// node counts, base domain, tags, …). The UI renders the spec form from
    /// these, so a new cloud declares its own without any UI change.
    fn spec_fields(&self) -> Vec<InputField>;

    /// Input keys that must be present (non-empty) before provisioning proceeds.
    fn required_inputs(&self) -> Vec<&'static str>;

    /// Verify the CLIs and credentials this cloud needs (e.g. AWS: openshift-install
    /// + aws + `aws sts get-caller-identity`; GCP would check gcloud, etc.).
    async fn preflight(&self, ctx: &StepContext) -> StepOutcome;

    /// Ensure the cluster's base DNS zone exists (validate / create / delegate).
    /// Each cloud uses its own DNS service (Route53 / Cloud DNS / Azure DNS).
    async fn ensure_dns(&self, ctx: &StepContext) -> StepOutcome;

    /// Render and write this cloud's cluster install config. Returns the path on
    /// success, or a `StepOutcome` (NeedsInput / Failed) to surface to the user.
    fn write_install_config(&self, ctx: &StepContext) -> Result<std::path::PathBuf, StepOutcome>;

    /// Create the cluster. Must be idempotent: if the cluster already exists
    /// (detected via on-disk auth artifacts), it should succeed without
    /// re-running the installer.
    async fn create(&self, ctx: &StepContext) -> StepOutcome;

    /// Report whether the cluster appears provisioned. `Completed` means the
    /// cluster's auth artifacts are present; otherwise `Failed` with guidance.
    async fn status(&self, ctx: &StepContext) -> StepOutcome;

    /// Destroy the cluster and its cloud resources.
    async fn destroy(&self, ctx: &StepContext) -> StepOutcome;
}

/// The selected provisioner for a run, from the `hyperscaler` input (default AWS).
fn provider_id(ctx: &StepContext) -> String {
    ctx.input("hyperscaler").unwrap_or("aws").to_string()
}

/// Registry of cloud provisioners, keyed by id. New clouds register here; the
/// generic provision steps dispatch to the one matching the run's `hyperscaler`.
#[derive(Clone)]
pub struct ProvisionerRegistry {
    by_id: std::collections::BTreeMap<String, std::sync::Arc<dyn Provisioner>>,
    default: String,
}

impl Default for ProvisionerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ProvisionerRegistry {
    /// A registry with the built-in providers (AWS today).
    pub fn new() -> Self {
        let mut by_id: std::collections::BTreeMap<String, std::sync::Arc<dyn Provisioner>> =
            std::collections::BTreeMap::new();
        by_id.insert("aws".to_string(), std::sync::Arc::new(AwsProvisioner::new()));
        Self { by_id, default: "aws".to_string() }
    }

    /// Register (or replace) a provisioner. Returns self for chaining.
    pub fn with(mut self, p: std::sync::Arc<dyn Provisioner>) -> Self {
        self.by_id.insert(p.id().to_string(), p);
        self
    }

    /// The provisioner for `id`, falling back to the default for execution.
    pub fn get(&self, id: &str) -> std::sync::Arc<dyn Provisioner> {
        self.by_id
            .get(id)
            .or_else(|| self.by_id.get(&self.default))
            .expect("default provisioner must exist")
            .clone()
    }

    /// Spec fields for a provider, or empty if it isn't implemented yet
    /// (so the UI shows "coming soon" rather than borrowing AWS's fields).
    pub fn spec_fields(&self, id: &str) -> Vec<InputField> {
        self.by_id.get(id).map(|p| p.spec_fields()).unwrap_or_default()
    }
}

/// The directory `openshift-install` operates on, under the run's artifacts.
fn cluster_dir(ctx: &StepContext) -> std::path::PathBuf {
    ctx.artifacts_dir().join("cluster")
}

/// Path to the kubeconfig `openshift-install` writes on success. Its presence is
/// our idempotency signal: a cluster has been provisioned.
fn kubeconfig_path(ctx: &StepContext) -> std::path::PathBuf {
    cluster_dir(ctx).join("auth").join("kubeconfig")
}

/// Copy the installer's kubeconfig to the run's standard location
/// (`<artifacts>/kubeconfig`) so downstream modules' `ctx.run_in_cluster(...)`
/// targets the cluster we just created. Best-effort; logs on failure.
fn publish_kubeconfig(ctx: &StepContext) {
    let src = kubeconfig_path(ctx);
    let dst = ctx.kubeconfig_path();
    match std::fs::copy(&src, &dst) {
        Ok(_) => ctx.log(format!("published kubeconfig to {}", dst.display())),
        Err(e) => ctx.log(format!(
            "warning: could not publish kubeconfig from {} to {}: {e}",
            src.display(),
            dst.display()
        )),
    }
}

/// Marker written only after the install is confirmed complete. Its presence —
/// not a kubeconfig (which appears during bootstrap) — is the safe "provisioned"
/// signal for idempotent retries.
fn install_complete_marker(ctx: &StepContext) -> std::path::PathBuf {
    cluster_dir(ctx).join(".wxd_install_complete")
}

/// Map a user-supplied OpenShift version to an OpenShift mirror channel/dir.
/// `4.21` → `stable-4.21` (latest 4.21.z); `4.21.5` → `4.21.5` (exact); an
/// explicit channel (`stable-4.21`, `latest-4.20`, `candidate-4.22`) is kept.
fn ocp_channel(version: &str) -> String {
    let v = version.trim();
    if v.is_empty() {
        return "stable".to_string();
    }
    if v.starts_with("stable") || v.starts_with("latest") || v.starts_with("fast") || v.starts_with("candidate") {
        return v.to_string();
    }
    if v.matches('.').count() >= 2 {
        v.to_string() // exact x.y.z
    } else {
        format!("stable-{v}") // x.y → latest patch on that minor
    }
}

/// Download the `openshift-install` binary for the requested OpenShift version
/// into `~/.wxd/bin` so `create cluster` installs that version. Best-effort: on
/// failure it logs a warning and leaves the existing installer in place. Skips
/// the download when the installed binary already matches the requested minor.
async fn ensure_installer_version(ctx: &StepContext, version: &str) {
    let minor: String = version
        .trim()
        .trim_start_matches("stable-")
        .trim_start_matches("latest-")
        .trim_start_matches("fast-")
        .trim_start_matches("candidate-")
        .split('.')
        .take(2)
        .collect::<Vec<_>>()
        .join(".");
    if let Ok(o) = ctx.run("openshift-install", &["version".to_string()]).await {
        if o.success() && o.stdout.contains(&format!("openshift-install {minor}.")) {
            ctx.log(format!("openshift-install already matches OpenShift {minor}"));
            return;
        }
    }
    let channel = ocp_channel(version);
    let arm = std::env::consts::ARCH == "aarch64";
    let (arch, file) = match std::env::consts::OS {
        "macos" => (
            if arm { "arm64" } else { "x86_64" },
            if arm { "openshift-install-mac-arm64.tar.gz" } else { "openshift-install-mac.tar.gz" },
        ),
        _ => (
            if arm { "arm64" } else { "x86_64" },
            if arm { "openshift-install-linux-arm64.tar.gz" } else { "openshift-install-linux.tar.gz" },
        ),
    };
    let script = format!(
        "set -e; BIN=\"$HOME/.wxd/bin\"; mkdir -p \"$BIN\"; \
         curl -fsSL \"https://mirror.openshift.com/pub/openshift-v4/{arch}/clients/ocp/{channel}/{file}\" -o /tmp/wxd-ois.tgz; \
         tar xzf /tmp/wxd-ois.tgz -C \"$BIN\" openshift-install; chmod +x \"$BIN/openshift-install\""
    );
    ctx.log(format!("installing openshift-install for OpenShift {version} (channel {channel})"));
    match ctx.run("sh", &["-c".to_string(), script]).await {
        Ok(o) if o.success() => ctx.log("openshift-install version ready"),
        Ok(o) => ctx.log(format!(
            "warning: could not install openshift-install {version} (exit {}): {} — using the existing installer",
            o.status,
            o.stderr.trim()
        )),
        Err(e) => ctx.log(format!("warning: could not run installer download: {e} — using the existing installer")),
    }
}

/// Backup location for `metadata.json`, kept outside the cluster dir so it
/// survives openshift-install pruning the cluster dir on destroy.
fn metadata_backup_path(ctx: &StepContext) -> std::path::PathBuf {
    ctx.artifacts_dir().join("metadata.json.bak")
}

/// Best-effort: copy the freshly-written `metadata.json` to the backup location.
/// `metadata.json` carries the random infra-ID suffix that destroy needs.
fn backup_metadata(ctx: &StepContext) {
    let src = cluster_dir(ctx).join("metadata.json");
    if src.exists() {
        let _ = std::fs::copy(&src, metadata_backup_path(ctx));
    }
}

/// Restore `metadata.json` into the cluster dir from the backup when it is
/// missing (e.g. an interrupted prior destroy removed it). Returns whether a
/// restore happened.
fn restore_metadata_if_missing(ctx: &StepContext) -> bool {
    let dst = cluster_dir(ctx).join("metadata.json");
    let bak = metadata_backup_path(ctx);
    if !dst.exists() && bak.exists() {
        let _ = std::fs::create_dir_all(cluster_dir(ctx));
        return std::fs::copy(&bak, &dst).is_ok();
    }
    false
}

/// The cluster infra ID (e.g. `swwxd-w4lcm`), read from the provisioner's
/// `metadata.json`. It tags every cluster resource and disambiguates the VPC.
/// Mirrors the storage module's helper.
fn infra_id(ctx: &StepContext) -> Option<String> {
    let path = ctx.artifacts_dir().join("cluster").join("metadata.json");
    let body = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    v.get("infraID").and_then(|x| x.as_str()).map(String::from)
}

/// First filesystem id in `aws efs describe-file-systems` JSON, if any (handles
/// both the `FileSystems[]` list and a bare object). Copied from the storage
/// module so destroy can locate the EFS filesystem to tear down.
fn parse_fs_id(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    if let Some(id) = v.get("FileSystemId").and_then(|x| x.as_str()) {
        return Some(id.to_string());
    }
    v.get("FileSystems")
        .and_then(|a| a.as_array())
        .and_then(|a| a.first())
        .and_then(|f| f.get("FileSystemId"))
        .and_then(|x| x.as_str())
        .map(String::from)
}

/// Mount-target ids from `aws efs describe-mount-targets` JSON.
fn parse_mount_target_ids(json: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("MountTargets").and_then(|m| m.as_array()).cloned())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("MountTargetId").and_then(|x| x.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Resource ARNs from `aws resourcegroupstaggingapi get-resources` JSON.
fn parse_resource_arns(json: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("ResourceTagMappingList").and_then(|l| l.as_array()).cloned())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("ResourceARN").and_then(|x| x.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// A still-tagged AWS resource extracted from an ARN.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RemainingResource {
    service: String,
    resource_type: String,
    id: String,
    billable: bool,
}

/// Parse an AWS ARN into `(service, resource_type, id)`.
///
/// Handles both ARN shapes:
///   - `arn:aws:<service>:<region>:<acct>:<resourcetype>/<id>`
///   - `arn:aws:<service>:<region>:<acct>:<resourcetype>:<id>`
/// When the resource portion has no separator (e.g. an S3 bucket
/// `arn:aws:s3:::bucket`), the whole resource is treated as the id with an empty
/// resource_type.
fn parse_arn(arn: &str) -> (String, String, String) {
    let parts: Vec<&str> = arn.splitn(6, ':').collect();
    if parts.len() < 6 {
        // Not a well-formed ARN — return it as an opaque id.
        return (String::new(), String::new(), arn.to_string());
    }
    let service = parts[2].to_string();
    let resource = parts[5];
    // The resource portion separates type and id by '/' or ':'.
    if let Some((rtype, id)) = resource.split_once('/') {
        (service, rtype.to_string(), id.to_string())
    } else if let Some((rtype, id)) = resource.split_once(':') {
        (service, rtype.to_string(), id.to_string())
    } else {
        (service, String::new(), resource.to_string())
    }
}

/// Whether a resource type incurs ongoing AWS charges and so is worth flagging
/// loudly when it survives a destroy.
fn is_billable(resource_type: &str) -> bool {
    let t = resource_type.to_ascii_lowercase();
    matches!(
        t.as_str(),
        "instance"
            | "natgateway"
            | "elastic-ip"
            | "eip"
            | "address"
            | "load-balancer"
            | "elb"
            | "elbv2"
            | "volume"
            | "file-system"
            | "efs"
            | "db"
            | "rds"
            | "fsx"
    )
}

/// Expand a leading `~/` to `$HOME` for user-supplied file paths.
fn expand_tilde(p: &str) -> std::path::PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return std::path::Path::new(&home).join(rest);
        }
    }
    std::path::PathBuf::from(p)
}

/// Resolve the Red Hat pull secret from either a pasted JSON value
/// (`pull_secret` secret) or a path to a file containing it (`pull_secret_path`
/// input, `~`-expanded). Prompts for either when neither is supplied.
fn resolve_pull_secret(ctx: &StepContext) -> Result<String, StepOutcome> {
    if let Some(s) = ctx.secret("pull_secret").filter(|s| !s.trim().is_empty()) {
        return Ok(s.trim().to_string());
    }
    if let Some(p) = ctx.input("pull_secret_path").filter(|p| !p.trim().is_empty()) {
        let path = expand_tilde(p.trim());
        return match std::fs::read_to_string(&path) {
            Ok(c) if !c.trim().is_empty() => Ok(c.trim().to_string()),
            Ok(_) => Err(StepOutcome::Failed {
                error: format!("pull secret file is empty: {}", path.display()),
                next_steps: vec![
                    "Point pull_secret_path at a non-empty Red Hat pull-secret JSON file, then retry.".to_string(),
                ],
            }),
            Err(e) => Err(StepOutcome::Failed {
                error: format!("could not read pull secret file {}: {e}", path.display()),
                next_steps: vec![
                    "Check the path/permissions, or paste the pull secret instead, then retry.".to_string(),
                ],
            }),
        };
    }
    Err(StepOutcome::NeedsInput {
        prompt: "Provide your Red Hat pull secret (console.redhat.com/openshift/install/pull-secret) — \
                 paste the JSON OR give a path to a file containing it. Optionally add an SSH public key."
            .to_string(),
        fields: vec![
            InputField {
                key: "pull_secret".to_string(),
                label: "Red Hat pull secret (JSON) — paste".to_string(),
                secret: true,
                default: None,
            },
            InputField {
                key: "pull_secret_path".to_string(),
                label: "…or path to a pull-secret file (e.g. ~/.ibm/pull-secret.json)".to_string(),
                secret: false,
                default: None,
            },
            InputField {
                key: "ssh_key".to_string(),
                label: "SSH public key (optional)".to_string(),
                secret: false,
                default: None,
            },
        ],
    })
}

/// AWS implementation of [`Provisioner`] using `openshift-install` IPI.
///
/// The installer reads `install-config.yaml` from the cluster dir (written by
/// the `write-install-config` step) and provisions all infrastructure itself.
#[derive(Debug, Default, Clone)]
pub struct AwsProvisioner;

impl AwsProvisioner {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Provisioner for AwsProvisioner {
    fn id(&self) -> &str {
        "aws"
    }

    fn display_name(&self) -> &str {
        "Amazon Web Services"
    }

    fn spec_fields(&self) -> Vec<InputField> {
        aws_spec_fields()
    }

    fn required_inputs(&self) -> Vec<&'static str> {
        REQUIRED_INPUTS.to_vec()
    }

    async fn preflight(&self, ctx: &StepContext) -> StepOutcome {
        aws_preflight(ctx).await
    }

    async fn ensure_dns(&self, ctx: &StepContext) -> StepOutcome {
        aws_ensure_dns(ctx).await
    }

    fn write_install_config(&self, ctx: &StepContext) -> Result<std::path::PathBuf, StepOutcome> {
        write_install_config(ctx)
    }

    async fn create(&self, ctx: &StepContext) -> StepOutcome {
        let dir = cluster_dir(ctx);
        let dir_str = dir.to_string_lossy().into_owned();
        let env = aws_env(ctx);

        // True idempotency: only a recorded completion marker means done.
        // (A kubeconfig appears during bootstrap — long before the install
        // actually finishes — so it is NOT a safe "complete" signal.)
        if install_complete_marker(ctx).exists() {
            ctx.log("cluster already provisioned (install previously completed)");
            publish_kubeconfig(ctx);
            ctx.progress(100);
            return StepOutcome::Completed;
        }

        // If an install was already started but not confirmed complete (e.g. a
        // prior attempt timed out waiting for the API), `create cluster` is not
        // resumable once its local control plane is gone — resume with the
        // idempotent `wait-for` path instead of starting over.
        let started = dir.join(".openshift_install_state.json").exists();
        // On a fresh attempt, make sure `openshift-install` matches the requested
        // OpenShift version (it pins the release payload it installs).
        if !started {
            if let Some(v) = ctx.input("ocp_version").filter(|v| !v.trim().is_empty()) {
                ensure_installer_version(ctx, v).await;
            }
        }
        let outcome = if started {
            ctx.log("install already in progress — resuming (wait-for bootstrap + install-complete)");
            ctx.progress(20);
            let bc = ctx
                .run_with_env(
                    "openshift-install",
                    &["wait-for".into(), "bootstrap-complete".into(), "--dir".into(), dir_str.clone()],
                    &env,
                )
                .await;
            if matches!(&bc, Ok(o) if o.success()) {
                // Bootstrap is done — remove the bootstrap node (best-effort;
                // a normal `create cluster` does this automatically).
                let _ = ctx
                    .run_with_env(
                        "openshift-install",
                        &["destroy".into(), "bootstrap".into(), "--dir".into(), dir_str.clone()],
                        &env,
                    )
                    .await;
            }
            ctx.progress(60);
            ctx.run_with_env(
                "openshift-install",
                &["wait-for".into(), "install-complete".into(), "--dir".into(), dir_str.clone()],
                &env,
            )
            .await
        } else {
            ctx.log("provisioning OpenShift cluster via openshift-install (AWS IPI)");
            ctx.progress(10);
            ctx.run_with_env(
                "openshift-install",
                &["create".into(), "cluster".into(), "--dir".into(), dir_str.clone()],
                &env,
            )
            .await
        };

        match outcome {
            Ok(out) if out.success() => {
                // Record completion so retries don't re-provision, and back up
                // metadata.json so a later destroy works even if openshift-install
                // prunes the cluster dir.
                let _ = std::fs::write(install_complete_marker(ctx), "ok\n");
                backup_metadata(ctx);
                ctx.log("cluster provisioned");
                publish_kubeconfig(ctx);
                ctx.progress(100);
                StepOutcome::Completed
            }
            Ok(out) => StepOutcome::Failed {
                error: format!(
                    "openshift-install failed (exit {}): {}",
                    out.status,
                    out.stderr.trim()
                ),
                next_steps: provision_failure_next_steps(),
            },
            Err(e) => StepOutcome::Failed {
                error: format!("could not run openshift-install: {e}"),
                next_steps: provision_failure_next_steps(),
            },
        }
    }

    async fn status(&self, ctx: &StepContext) -> StepOutcome {
        if install_complete_marker(ctx).exists() {
            StepOutcome::Completed
        } else {
            StepOutcome::Failed {
                error: "install not confirmed complete; cluster does not appear provisioned"
                    .to_string(),
                next_steps: vec![
                    "Run the create-cluster step to provision (or finish provisioning) the cluster.".to_string(),
                ],
            }
        }
    }

    async fn destroy(&self, ctx: &StepContext) -> StepOutcome {
        let dir = cluster_dir(ctx);
        let dir_str = dir.to_string_lossy().into_owned();
        // `openshift-install destroy cluster` needs metadata.json — and a prior
        // (possibly interrupted) destroy may have already deleted it, orphaning
        // resources. Restore it from our backup so teardown can always proceed.
        if restore_metadata_if_missing(ctx) {
            ctx.log("restored metadata.json from backup for destroy");
        }

        // Read the infra ID once: it drives both EFS teardown and the report.
        let infra = infra_id(ctx);

        // EFS is created by the storage module and is NOT removed by
        // openshift-install. Its mount targets keep ENIs in the cluster subnets,
        // which can block VPC deletion — so tear EFS down first, best-effort.
        if let Some(infra) = infra.as_deref() {
            teardown_efs(ctx, infra).await;
        } else {
            ctx.log("warning: could not read cluster infra ID (metadata.json) — skipping EFS teardown");
        }

        ctx.log("destroying OpenShift cluster via openshift-install");
        let args = vec![
            "destroy".to_string(),
            "cluster".to_string(),
            "--dir".to_string(),
            dir_str,
        ];
        let outcome = match ctx.run_with_env("openshift-install", &args, &aws_env(ctx)).await {
            Ok(out) if out.success() => StepOutcome::Completed,
            Ok(out) => StepOutcome::Failed {
                error: format!(
                    "openshift-install destroy cluster failed (exit {}): {}",
                    out.status,
                    out.stderr.trim()
                ),
                next_steps: vec![
                    "Inspect the cluster dir's .openshift_install.log for details."
                        .to_string(),
                    "Some cloud resources may need manual cleanup in the AWS console."
                        .to_string(),
                ],
            },
            Err(e) => StepOutcome::Failed {
                error: format!("could not run openshift-install: {e}"),
                next_steps: vec![
                    "Ensure openshift-install is installed and on PATH.".to_string(),
                ],
            },
        };

        // Always emit the post-destroy resource inventory — on success it confirms
        // a clean teardown, on failure it tells the user exactly what to clean up.
        if let Some(infra) = infra.as_deref() {
            destroy_report(ctx, infra).await;
        } else {
            ctx.log("warning: could not read cluster infra ID (metadata.json) — skipping destroy report");
        }

        outcome
    }
}

/// Tear down the EFS filesystem (and its mount targets) the storage module
/// created for this cluster. Best-effort: every failure logs a warning and
/// continues so the cluster destroy still runs.
async fn teardown_efs(ctx: &StepContext, infra: &str) {
    let env = aws_env(ctx);
    let token = format!("{infra}-efs");

    // Locate the filesystem by its creation token.
    let described = ctx
        .run_with_env(
            "aws",
            &[
                "efs".into(),
                "describe-file-systems".into(),
                "--creation-token".into(),
                token.clone(),
                "--output".into(),
                "json".into(),
            ],
            &env,
        )
        .await;
    let fs = match described {
        Ok(o) if o.success() => parse_fs_id(&o.stdout),
        Ok(o) => {
            ctx.log(format!(
                "warning: efs describe-file-systems failed (exit {}): {} — skipping EFS teardown",
                o.status,
                o.stderr.trim()
            ));
            return;
        }
        Err(e) => {
            ctx.log(format!("warning: could not run aws efs describe-file-systems: {e} — skipping EFS teardown"));
            return;
        }
    };
    let Some(fs) = fs else {
        ctx.log("no EFS filesystem tagged for this cluster — nothing to tear down");
        return;
    };
    ctx.log(format!("tearing down EFS filesystem {fs} ({token})"));

    // Delete every mount target (each holds an ENI in a cluster subnet).
    let mt = ctx
        .run_with_env(
            "aws",
            &["efs".into(), "describe-mount-targets".into(), "--file-system-id".into(), fs.clone(), "--output".into(), "json".into()],
            &env,
        )
        .await;
    let mount_targets = mt.ok().filter(|o| o.success()).map(|o| parse_mount_target_ids(&o.stdout)).unwrap_or_default();
    for id in &mount_targets {
        ctx.log(format!("deleting EFS mount target {id}"));
        let _ = ctx
            .run_with_env(
                "aws",
                &["efs".into(), "delete-mount-target".into(), "--mount-target-id".into(), id.clone()],
                &env,
            )
            .await;
    }

    // Mount-target deletion is async; poll a bounded number of times until none
    // remain. We can't sleep easily here, so just re-describe up to N times; if
    // they're still detaching the delete-file-system below will fail and we log
    // a warning (the report then flags the leftover EFS).
    if !mount_targets.is_empty() {
        for _ in 0..10 {
            let still = ctx
                .run_with_env(
                    "aws",
                    &["efs".into(), "describe-mount-targets".into(), "--file-system-id".into(), fs.clone(), "--output".into(), "json".into()],
                    &env,
                )
                .await;
            let remaining = still.ok().filter(|o| o.success()).map(|o| parse_mount_target_ids(&o.stdout)).unwrap_or_default();
            if remaining.is_empty() {
                break;
            }
        }
    }

    // Delete the filesystem. May fail if mount targets are still detaching — log
    // and continue; the destroy report will flag it if it survives.
    match ctx
        .run_with_env(
            "aws",
            &["efs".into(), "delete-file-system".into(), "--file-system-id".into(), fs.clone()],
            &env,
        )
        .await
    {
        Ok(o) if o.success() => ctx.log(format!("deleted EFS filesystem {fs}")),
        Ok(o) => ctx.log(format!(
            "warning: efs delete-file-system {fs} failed (exit {}): {} — it may still be detaching mount targets; verify in the AWS console",
            o.status,
            o.stderr.trim()
        )),
        Err(e) => ctx.log(format!("warning: could not run aws efs delete-file-system: {e}")),
    }
}

/// Enumerate every AWS resource still tagged for this cluster and emit a report
/// (live log + `destroy-report.txt` / `destroy-report.json` artifacts) so the
/// user can manually clean up any leftovers and avoid recurring cost.
async fn destroy_report(ctx: &StepContext, infra: &str) {
    let env = aws_env(ctx);
    let region = ctx.input("region").unwrap_or("us-east-1").to_string();
    let tag_keys = [
        format!("kubernetes.io/cluster/{infra}"),
        format!("sigs.k8s.io/cluster-api-provider-aws/cluster/{infra}"),
    ];

    let mut remaining: Vec<RemainingResource> = Vec::new();
    for key in &tag_keys {
        let out = ctx
            .run_with_env(
                "aws",
                &[
                    "resourcegroupstaggingapi".into(),
                    "get-resources".into(),
                    "--region".into(),
                    region.clone(),
                    "--tag-filters".into(),
                    format!("Key={key},Values=owned"),
                    "--output".into(),
                    "json".into(),
                ],
                &env,
            )
            .await;
        let arns = match out {
            Ok(o) if o.success() => parse_resource_arns(&o.stdout),
            Ok(o) => {
                ctx.log(format!(
                    "warning: get-resources for {key} failed (exit {}): {}",
                    o.status,
                    o.stderr.trim()
                ));
                Vec::new()
            }
            Err(e) => {
                ctx.log(format!("warning: could not run aws resourcegroupstaggingapi get-resources: {e}"));
                Vec::new()
            }
        };
        for arn in arns {
            let (service, resource_type, id) = parse_arn(&arn);
            let res = RemainingResource {
                billable: is_billable(&resource_type),
                service,
                resource_type,
                id,
            };
            if !remaining.contains(&res) {
                remaining.push(res);
            }
        }
    }

    // Live log report.
    ctx.log(format!("=== destroy report for {infra} ({region}) ==="));
    if remaining.is_empty() {
        ctx.log("all tagged resources deleted — no leftovers");
    } else {
        for r in &remaining {
            let billable = if r.billable { " [BILLABLE]" } else { "" };
            ctx.log(format!("REMAINING{billable} {}/{} {}", r.service, r.resource_type, r.id));
        }
        let billable_count = remaining.iter().filter(|r| r.billable).count();
        ctx.log(format!(
            "{} resource(s) still tagged (may be eventual-consistency lag) — {} billable; verify/clean up in the AWS console",
            remaining.len(),
            billable_count
        ));
    }

    // Artifact files (human-readable + structured) for later tracking.
    write_destroy_report_files(ctx, infra, &region, &remaining);
}

/// Write `destroy-report.txt` (human) and `destroy-report.json` (structured).
fn write_destroy_report_files(
    ctx: &StepContext,
    infra: &str,
    region: &str,
    remaining: &[RemainingResource],
) {
    let mut txt = format!("=== destroy report for {infra} ({region}) ===\n");
    if remaining.is_empty() {
        txt.push_str("all tagged resources deleted — no leftovers\n");
    } else {
        for r in remaining {
            let billable = if r.billable { " [BILLABLE]" } else { "" };
            txt.push_str(&format!("REMAINING{billable} {}/{} {}\n", r.service, r.resource_type, r.id));
        }
        let billable_count = remaining.iter().filter(|r| r.billable).count();
        txt.push_str(&format!(
            "{} resource(s) still tagged (may be eventual-consistency lag) — {} billable; verify/clean up in the AWS console\n",
            remaining.len(),
            billable_count
        ));
    }
    let txt_path = ctx.artifacts_dir().join("destroy-report.txt");
    if let Err(e) = std::fs::write(&txt_path, &txt) {
        ctx.log(format!("warning: could not write {}: {e}", txt_path.display()));
    }

    let resources: Vec<serde_json::Value> = remaining
        .iter()
        .map(|r| {
            serde_json::json!({
                "service": r.service,
                "resource_type": r.resource_type,
                "id": r.id,
                "billable": r.billable,
            })
        })
        .collect();
    let json = serde_json::json!({
        "infra_id": infra,
        "region": region,
        "generated": false,
        "remaining": resources,
    });
    let json_path = ctx.artifacts_dir().join("destroy-report.json");
    if let Err(e) = std::fs::write(&json_path, serde_json::to_string_pretty(&json).unwrap_or_default()) {
        ctx.log(format!("warning: could not write {}: {e}", json_path.display()));
    }
}

/// Actionable guidance when provisioning fails — the common culprits for an
/// AWS IPI install.
fn provision_failure_next_steps() -> Vec<String> {
    vec![
        "Check AWS service quotas (EC2 vCPUs, Elastic IPs, VPCs) in the target region."
            .to_string(),
        "Verify the IAM principal has the permissions openshift-install requires \
         (EC2, Route53, IAM, S3, ELB)."
            .to_string(),
        "Confirm the base domain is a Route53 public hosted zone in this account."
            .to_string(),
        "Review the install log at <artifacts>/cluster/.openshift_install.log."
            .to_string(),
    ]
}

/// The AWS cluster-spec fields, exposed so the API can render a provider-driven
/// spec form. Other clouds add their own `Provisioner` + spec fields the same way.
pub fn aws_spec_fields() -> Vec<InputField> {
    spec_fields()
}

/// The cluster-spec inputs with their v1 defaults for a watsonx.data-capable
/// cluster. `base_domain` has no default — it must be a real Route53 hosted zone
/// the user owns.
fn spec_fields() -> Vec<InputField> {
    vec![
        InputField {
            key: "cluster_name".to_string(),
            label: "Cluster / resource name (tags every cloud resource)".to_string(),
            secret: false,
            default: Some("wxd".to_string()),
        },
        InputField {
            key: "region".to_string(),
            label: "AWS region".to_string(),
            secret: false,
            default: Some("us-east-1".to_string()),
        },
        InputField {
            key: "base_domain".to_string(),
            label: "Base domain (Route53 hosted zone)".to_string(),
            secret: false,
            default: None,
        },
        InputField {
            key: "ocp_version".to_string(),
            label: "OpenShift version".to_string(),
            secret: false,
            default: Some("4.21".to_string()),
        },
        InputField {
            key: "create_base_domain_zone".to_string(),
            label: "Create the base-domain hosted zone if missing (auto-delegates for a subdomain of a zone you own)".to_string(),
            secret: false,
            default: Some("false".to_string()),
        },
        InputField {
            key: "control_plane_type".to_string(),
            label: "Control plane instance type".to_string(),
            secret: false,
            default: Some("m6i.2xlarge".to_string()),
        },
        InputField {
            key: "control_plane_count".to_string(),
            label: "Control plane node count".to_string(),
            secret: false,
            default: Some("3".to_string()),
        },
        InputField {
            key: "worker_type".to_string(),
            label: "Worker instance type".to_string(),
            secret: false,
            default: Some("m6i.4xlarge".to_string()),
        },
        InputField {
            key: "worker_count".to_string(),
            label: "Worker node count".to_string(),
            secret: false,
            default: Some("3".to_string()),
        },
        InputField {
            key: "resource_tags".to_string(),
            label: "Extra cloud tags (optional, key=value,key2=value2)".to_string(),
            secret: false,
            default: None,
        },
    ]
}

/// The required, non-secret input keys for the cluster spec. `cluster_name` is
/// required because it tags every cloud resource the installer creates.
const REQUIRED_INPUTS: [&str; 7] = [
    "cluster_name",
    "region",
    "base_domain",
    "control_plane_type",
    "control_plane_count",
    "worker_type",
    "worker_count",
];

/// Parse a `key=value,key2=value2` tag string into pairs, ignoring blanks.
pub fn parse_tags(raw: &str) -> Vec<(String, String)> {
    raw.split(',')
        .filter_map(|kv| {
            let kv = kv.trim();
            if kv.is_empty() {
                return None;
            }
            let (k, v) = kv.split_once('=')?;
            let (k, v) = (k.trim(), v.trim());
            if k.is_empty() || v.is_empty() {
                None
            } else {
                Some((k.to_string(), v.to_string()))
            }
        })
        .collect()
}

/// Render an `install-config.yaml` for AWS IPI from the collected inputs.
///
/// The result is deterministic given the same inputs. The pull secret is
/// embedded verbatim (it is required by `openshift-install`); callers must keep
/// the cluster dir out of any logs/artifacts that get shared.
#[allow(clippy::too_many_arguments)]
pub fn render_install_config(
    cluster_name: &str,
    base_domain: &str,
    region: &str,
    control_plane_type: &str,
    control_plane_count: &str,
    worker_type: &str,
    worker_count: &str,
    pull_secret: &str,
    ssh_key: Option<&str>,
    user_tags: &[(String, String)],
) -> String {
    let ssh_line = match ssh_key {
        Some(key) if !key.is_empty() => format!("sshKey: '{}'\n", key.trim()),
        _ => String::new(),
    };
    // Every resource openshift-install creates on AWS gets these tags
    // (`platform.aws.userTags`), keyed off the user-provided name plus any extras.
    let mut tag_lines = String::new();
    for (k, v) in user_tags {
        tag_lines.push_str(&format!("      {k}: '{v}'\n"));
    }
    let user_tags_block = if tag_lines.is_empty() {
        String::new()
    } else {
        format!("    userTags:\n{tag_lines}")
    };
    format!(
        "apiVersion: v1\n\
         baseDomain: {base_domain}\n\
         metadata:\n  \
         name: {cluster_name}\n\
         compute:\n\
         - name: worker\n  \
         replicas: {worker_count}\n  \
         platform:\n    \
         aws:\n      \
         type: {worker_type}\n\
         controlPlane:\n  \
         name: master\n  \
         replicas: {control_plane_count}\n  \
         platform:\n    \
         aws:\n      \
         type: {control_plane_type}\n\
         platform:\n  \
         aws:\n    \
         region: {region}\n\
         {user_tags_block}\
         pullSecret: '{pull_secret}'\n\
         {ssh_line}"
    )
}

/// Sanitize a user-supplied name into an RFC 1123 subdomain that
/// `openshift-install` accepts for `metadata.name`: lowercase, only
/// `[a-z0-9.-]`, starting and ending with an alphanumeric. Invalid characters
/// (e.g. `_`) become `-`. Falls back to `wxd` if nothing valid remains.
pub fn sanitize_cluster_name(raw: &str) -> String {
    let mapped: String = raw
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '-' })
        .collect();
    let trimmed = mapped.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    if trimmed.is_empty() {
        "wxd".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Build the AWS `userTags` for a run. AWS IPI reserves the `Name` and
/// `kubernetes.io/*` tag keys, so we tag with `cluster-name=<name>` (plus any
/// extra `resource_tags`); openshift-install already adds its own `Name` tags.
fn build_user_tags(cluster_name: &str, resource_tags: &str) -> Vec<(String, String)> {
    let mut tags = vec![("cluster-name".to_string(), cluster_name.to_string())];
    for (k, v) in parse_tags(resource_tags) {
        if k.eq_ignore_ascii_case("Name") || k.starts_with("kubernetes.io/") || k == "cluster-name" {
            continue; // reserved / already set
        }
        tags.push((k, v));
    }
    tags
}

/// The provisioning module: provider-agnostic steps that dispatch to the
/// `Provisioner` matching the run's `hyperscaler` input. New clouds plug in by
/// implementing `Provisioner` and registering in the `ProvisionerRegistry`.
#[derive(Clone)]
pub struct ProvisionModule {
    reg: std::sync::Arc<ProvisionerRegistry>,
}

impl Default for ProvisionModule {
    fn default() -> Self {
        Self::new()
    }
}

impl ProvisionModule {
    /// Module with the built-in providers (AWS today).
    pub fn new() -> Self {
        Self { reg: std::sync::Arc::new(ProvisionerRegistry::new()) }
    }

    /// Module with a custom provisioner registry (e.g. AWS + GCP + Azure).
    pub fn with_registry(reg: std::sync::Arc<ProvisionerRegistry>) -> Self {
        Self { reg }
    }
}

impl Module for ProvisionModule {
    fn id(&self) -> &str {
        "mod-provision"
    }

    fn title(&self) -> &str {
        "Provision OpenShift cluster"
    }

    fn steps(&self) -> Vec<Box<dyn Step>> {
        vec![
            Box::new(ClusterSpecStep { reg: self.reg.clone() }),
            Box::new(PreflightStep { reg: self.reg.clone() }),
            Box::new(EnsureDnsStep { reg: self.reg.clone() }),
            Box::new(WriteConfigStep { reg: self.reg.clone() }),
            Box::new(CreateClusterStep { reg: self.reg.clone() }),
        ]
    }
}

/// Step 1: collect/validate the cluster spec for the selected provider.
struct ClusterSpecStep {
    reg: std::sync::Arc<ProvisionerRegistry>,
}

#[async_trait]
impl Step for ClusterSpecStep {
    fn id(&self) -> &str {
        "cluster-spec"
    }

    fn title(&self) -> &str {
        "Define cluster spec"
    }

    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        let p = self.reg.get(&provider_id(ctx));
        let missing = p
            .required_inputs()
            .iter()
            .any(|key| ctx.input(key).map(str::is_empty).unwrap_or(true));

        if missing {
            return StepOutcome::NeedsInput {
                prompt: format!("Provide the {} cluster spec.", p.display_name()),
                fields: p.spec_fields(),
            };
        }

        // `cluster_name` is universal across clouds (OpenShift requires an
        // RFC 1123 name). Re-ask with a valid suggestion rather than silently
        // renaming or failing deep in `create cluster`.
        let name = ctx.input("cluster_name").unwrap_or("");
        if !is_valid_cluster_name(name) {
            return StepOutcome::NeedsInput {
                prompt: format!(
                    "\"{name}\" isn't a valid cluster name. Use only lowercase letters, \
                     numbers, '-' and '.', starting and ending with a letter or number \
                     (e.g. {}).",
                    sanitize_cluster_name(name)
                ),
                fields: vec![InputField {
                    key: "cluster_name".to_string(),
                    label: "Cluster / resource name (lowercase RFC 1123)".to_string(),
                    secret: false,
                    default: Some(sanitize_cluster_name(name)),
                }],
            };
        }

        ctx.log("cluster spec complete");
        StepOutcome::Completed
    }
}

/// Step 2: provider preflight (tooling + credentials).
struct PreflightStep {
    reg: std::sync::Arc<ProvisionerRegistry>,
}
#[async_trait]
impl Step for PreflightStep {
    fn id(&self) -> &str {
        "preflight-aws"
    }
    fn title(&self) -> &str {
        "Preflight"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        self.reg.get(&provider_id(ctx)).preflight(ctx).await
    }
}

/// Step 3: ensure the base-domain DNS zone.
struct EnsureDnsStep {
    reg: std::sync::Arc<ProvisionerRegistry>,
}
#[async_trait]
impl Step for EnsureDnsStep {
    fn id(&self) -> &str {
        "ensure-dns-zone"
    }
    fn title(&self) -> &str {
        "Ensure base-domain DNS zone"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        self.reg.get(&provider_id(ctx)).ensure_dns(ctx).await
    }
}

/// Step 4: write the provider's install config.
struct WriteConfigStep {
    reg: std::sync::Arc<ProvisionerRegistry>,
}
#[async_trait]
impl Step for WriteConfigStep {
    fn id(&self) -> &str {
        "write-install-config"
    }
    fn title(&self) -> &str {
        "Write install config"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        match self.reg.get(&provider_id(ctx)).write_install_config(ctx) {
            Ok(_) => {
                ctx.progress(100);
                StepOutcome::Completed
            }
            Err(outcome) => outcome,
        }
    }
}

/// Step 5: create the cluster. Regenerates the install config before each
/// attempt (until the installer has consumed it) so a retry uses fresh config.
struct CreateClusterStep {
    reg: std::sync::Arc<ProvisionerRegistry>,
}
#[cfg(test)]
impl CreateClusterStep {
    fn new() -> Self {
        Self { reg: std::sync::Arc::new(ProvisionerRegistry::new()) }
    }
}
#[async_trait]
impl Step for CreateClusterStep {
    fn id(&self) -> &str {
        "create-cluster"
    }
    fn title(&self) -> &str {
        "Create cluster"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        let p = self.reg.get(&provider_id(ctx));
        let complete = install_complete_marker(ctx).exists();
        let started = cluster_dir(ctx).join(".openshift_install_state.json").exists();
        // Only (re)write install-config for a genuinely fresh attempt — not when
        // resuming an in-progress install or when it already completed.
        if !complete && !started && !kubeconfig_path(ctx).exists() {
            if let Err(outcome) = p.write_install_config(ctx) {
                return outcome;
            }
        }
        p.create(ctx).await
    }
}

/// Whether `name` is a valid RFC 1123 subdomain for `metadata.name`.
fn is_valid_cluster_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 253 {
        return false;
    }
    let all_valid = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.');
    let alnum = |c: Option<char>| matches!(c, Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit());
    all_valid && alnum(name.chars().next()) && alnum(name.chars().last())
}

/// Run one preflight check, returning an error string on failure. Logs the
/// command line through `ctx` so the live log shows exactly what ran.
async fn preflight_check(
    ctx: &StepContext,
    program: &str,
    args: &[String],
    env: &[(String, String)],
    what: &str,
) -> Result<(), String> {
    match ctx.run_with_env(program, args, env).await {
        Ok(out) if out.success() => Ok(()),
        Ok(out) => Err(format!("{what} failed (exit {}): {}", out.status, out.stderr.trim())),
        Err(e) => Err(format!("{what}: could not run `{program}`: {e}")),
    }
}

/// AWS credentials/region passed to `openshift-install`/`aws`, sourced from run
/// secrets (entered in the UI). Empty when the user relies on `~/.aws` instead.
fn aws_env(ctx: &StepContext) -> Vec<(String, String)> {
    let mut env = Vec::new();
    for key in ["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN"] {
        if let Some(v) = ctx.secret(key).filter(|v| !v.is_empty()) {
            env.push((key.to_string(), v.to_string()));
        }
    }
    if let Some(r) = ctx.input("region").filter(|v| !v.is_empty()) {
        env.push(("AWS_DEFAULT_REGION".to_string(), r.to_string()));
    }
    env
}

/// AWS preflight: verify `openshift-install` + `aws` CLI + credentials.
async fn aws_preflight(ctx: &StepContext) -> StepOutcome {
    let env = aws_env(ctx);
    let checks = [
        ("openshift-install", vec!["version".to_string()], "openshift-install availability"),
        ("aws", vec!["--version".to_string()], "aws CLI availability"),
        (
            "aws",
            vec!["sts".to_string(), "get-caller-identity".to_string()],
            "AWS credentials (aws sts get-caller-identity)",
        ),
    ];
    for (program, args, what) in checks {
        ctx.log(format!("preflight: {what}"));
        if let Err(error) = preflight_check(ctx, program, &args, &env, what).await {
            return StepOutcome::Failed {
                error,
                next_steps: vec![
                    "Install openshift-install (the OpenShift IPI installer) and put it on PATH."
                        .to_string(),
                    "Install the AWS CLI v2 and put it on PATH.".to_string(),
                    "Configure AWS credentials (aws configure, or AWS_ACCESS_KEY_ID / \
                     AWS_SECRET_ACCESS_KEY) for an account with provisioning rights."
                        .to_string(),
                ],
            };
        }
    }
    ctx.log("preflight passed");
    ctx.progress(100);
    StepOutcome::Completed
}

/// Parse `aws route53 list-hosted-zones --output json` into the **public** zones
/// as `(name, id)` pairs (name has a trailing dot, e.g. `example.com.`).
fn parse_public_zones(json: &str) -> Vec<(String, String)> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("HostedZones").and_then(|h| h.as_array()).cloned())
        .map(|arr| {
            arr.iter()
                .filter(|z| {
                    z.get("Config")
                        .and_then(|c| c.get("PrivateZone"))
                        .and_then(|p| p.as_bool())
                        == Some(false)
                })
                .filter_map(|z| {
                    let name = z.get("Name").and_then(|n| n.as_str())?;
                    let id = z.get("Id").and_then(|i| i.as_str()).unwrap_or("");
                    Some((name.to_string(), id.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Name servers from `aws route53 create-hosted-zone --output json`.
fn parse_delegation_ns(json: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| {
            v.get("DelegationSet")
                .and_then(|d| d.get("NameServers"))
                .and_then(|n| n.as_array())
                .cloned()
        })
        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// The longest public zone that is a strict parent of `base` (e.g. parent
/// `ocpcpdtest.com.` for base `wxd.ocpcpdtest.com`), if any.
fn find_parent_zone(zones: &[(String, String)], base: &str) -> Option<(String, String)> {
    let base_dot = format!("{}.", base.trim_end_matches('.'));
    zones
        .iter()
        .filter(|(n, _)| n != &base_dot && base_dot.ends_with(&format!(".{n}")))
        .max_by_key(|(n, _)| n.len())
        .cloned()
}

/// A Route53 change-batch (JSON) that UPSERTs an NS record delegating `base` to
/// `ns` in the parent zone.
fn ns_change_batch(base: &str, ns: &[String]) -> String {
    let records: Vec<String> = ns.iter().map(|n| format!("{{\"Value\":\"{n}\"}}")).collect();
    format!(
        "{{\"Changes\":[{{\"Action\":\"UPSERT\",\"ResourceRecordSet\":\
         {{\"Name\":\"{base}\",\"Type\":\"NS\",\"TTL\":300,\"ResourceRecords\":[{}]}}}}]}}",
        records.join(",")
    )
}

/// Create the Route53 hosted zone for `base` and, when it is a subdomain of an
/// existing public zone, auto-delegate by UPSERTing the NS record set in the
/// parent. For an apex/unowned domain, surface the NS records for registrar
/// delegation.
async fn aws_create_zone(
    ctx: &StepContext,
    base: &str,
    zones: &[(String, String)],
    env: &[(String, String)],
) -> StepOutcome {
        ctx.log(format!("creating Route53 hosted zone for {base}"));
        let caller_ref = format!("wxd-{}-{base}", ctx.run_id);
        let create = ctx.run_with_env(
                "aws",
                &[
                    "route53".into(),
                    "create-hosted-zone".into(),
                    "--name".into(),
                    base.to_string(),
                    "--caller-reference".into(),
                    caller_ref,
                    "--output".into(),
                    "json".into(),
                ],
                env,
            )
            .await;
        let out = match create {
            Ok(o) if o.success() => o,
            Ok(o) => {
                return StepOutcome::Failed {
                    error: format!("create-hosted-zone failed (exit {}): {}", o.status, o.stderr.trim()),
                    next_steps: vec!["Check Route53 permissions (route53:CreateHostedZone), then retry.".into()],
                }
            }
            Err(e) => {
                return StepOutcome::Failed {
                    error: format!("could not run aws route53 create-hosted-zone: {e}"),
                    next_steps: vec!["Ensure the aws CLI is installed (Prerequisites), then retry.".into()],
                }
            }
        };
        let ns = parse_delegation_ns(&out.stdout);
        if ns.is_empty() {
            return StepOutcome::Failed {
                error: "created the zone but could not read its name servers".to_string(),
                next_steps: vec!["Inspect the new hosted zone in the Route53 console.".into()],
            };
        }

        match find_parent_zone(zones, base) {
            Some((parent_name, parent_id)) => {
                ctx.log(format!("delegating {base} under parent zone {parent_name}"));
                let batch = ns_change_batch(base, &ns);
                let path = ctx.artifacts_dir().join("route53-delegation.json");
                if let Err(e) = std::fs::write(&path, &batch) {
                    return StepOutcome::Failed {
                        error: format!("could not write delegation change-batch: {e}"),
                        next_steps: vec!["Check filesystem permissions for the artifacts directory.".into()],
                    };
                }
                let batch_arg = format!("file://{}", path.display());
                match ctx.run_with_env(
                        "aws",
                        &[
                            "route53".into(),
                            "change-resource-record-sets".into(),
                            "--hosted-zone-id".into(),
                            parent_id,
                            "--change-batch".into(),
                            batch_arg,
                            "--output".into(),
                            "json".into(),
                        ],
                        env,
                    )
                    .await
                {
                    Ok(o) if o.success() => {
                        ctx.log(format!("delegated {base} in {parent_name}; the zone is ready"));
                        ctx.progress(100);
                        StepOutcome::Completed
                    }
                    Ok(o) => StepOutcome::Failed {
                        error: format!("NS delegation failed (exit {}): {}", o.status, o.stderr.trim()),
                        next_steps: vec![
                            format!(
                                "Manually add an NS record for {base} in the {parent_name} zone with: {}",
                                ns.join(", ")
                            ),
                            "Then retry.".into(),
                        ],
                    },
                    Err(e) => StepOutcome::Failed {
                        error: format!("could not run change-resource-record-sets: {e}"),
                        next_steps: vec!["Retry once the aws CLI is available.".into()],
                    },
                }
            }
            None => StepOutcome::Failed {
                error: format!(
                    "created the hosted zone for '{base}', but it won't resolve until you delegate it at your domain registrar"
                ),
                next_steps: vec![
                    format!(
                        "At your domain registrar for '{base}', set these name server (NS) records: {}",
                        ns.join(", ")
                    ),
                    "After the registrar delegation propagates, retry. (Tip: a subdomain of a domain you already host in Route53 delegates automatically.)".into(),
                ],
            },
        }
    }

/// AWS: ensure the base domain is a usable public Route53 hosted zone — validate
/// it exists, or (when opted in) create it and auto-delegate for a subdomain.
async fn aws_ensure_dns(ctx: &StepContext) -> StepOutcome {
        let base = match ctx.input("base_domain").filter(|d| !d.is_empty()) {
            Some(d) => d.trim_end_matches('.').to_string(),
            None => {
                return StepOutcome::Failed {
                    error: "base_domain is required".to_string(),
                    next_steps: vec!["Re-run the cluster-spec step and supply a base domain.".into()],
                }
            }
        };
        let env = aws_env(ctx);
        ctx.log(format!("checking Route53 for a public hosted zone matching {base}"));
        let listing = ctx.run_with_env(
                "aws",
                &["route53".into(), "list-hosted-zones".into(), "--output".into(), "json".into()],
                &env,
            )
            .await;
        let zones = match listing {
            Ok(o) if o.success() => parse_public_zones(&o.stdout),
            // Couldn't list (e.g. limited IAM) — don't hard-block; let
            // openshift-install be the source of truth.
            Ok(o) => {
                ctx.log(format!(
                    "warning: could not list Route53 zones (exit {}): {} — skipping DNS check",
                    o.status,
                    o.stderr.trim()
                ));
                return StepOutcome::Completed;
            }
            Err(e) => {
                ctx.log(format!("warning: could not run aws route53: {e} — skipping DNS check"));
                return StepOutcome::Completed;
            }
        };

        let want = format!("{base}.");
        if zones.iter().any(|(n, _)| n == &want) {
            ctx.log(format!("base domain '{base}' is an existing public hosted zone"));
            ctx.progress(100);
            return StepOutcome::Completed;
        }

        let opt_in = matches!(ctx.input("create_base_domain_zone"), Some(v) if v.eq_ignore_ascii_case("true"));
        if !opt_in {
            let listing = if zones.is_empty() {
                "(no public hosted zones found in this account)".to_string()
            } else {
                zones.iter().map(|(n, _)| n.trim_end_matches('.').to_string()).collect::<Vec<_>>().join(", ")
            };
            return StepOutcome::Failed {
                error: format!("'{base}' is not a public Route53 hosted zone in this account"),
                next_steps: vec![
                    format!("Use one of your existing public hosted zones as the base domain: {listing}"),
                    "…or enable \"Create the base-domain hosted zone if missing\" in the cluster spec and retry.".into(),
                ],
            };
        }

        aws_create_zone(ctx, &base, &zones, &env).await
}

/// Render and write `install-config.yaml` into the cluster dir.

/// Render and write `install-config.yaml` from the run's inputs/secrets. Returns
/// the path on success, or a `StepOutcome` (NeedsInput for the pull secret, or
/// Failed) to surface to the user. Shared by the write step and create step so a
/// retry always regenerates a fresh, correct config (openshift-install validates
/// and consumes it on each `create cluster`).
fn write_install_config(ctx: &StepContext) -> Result<std::path::PathBuf, StepOutcome> {
    let region = ctx.input("region").unwrap_or("us-east-1");
    let base_domain = match ctx.input("base_domain") {
        Some(d) if !d.is_empty() => d,
        _ => {
            return Err(StepOutcome::Failed {
                error: "base_domain is required to render install-config.yaml".to_string(),
                next_steps: vec![
                    "Re-run the cluster-spec step and supply a Route53 base domain.".to_string(),
                ],
            });
        }
    };
    let control_plane_type = ctx.input("control_plane_type").unwrap_or("m6i.2xlarge");
    let control_plane_count = ctx.input("control_plane_count").unwrap_or("3");
    let worker_type = ctx.input("worker_type").unwrap_or("m6i.4xlarge");
    let worker_count = ctx.input("worker_count").unwrap_or("3");
    let cluster_name = ctx.input("cluster_name").unwrap_or("wxd");
    let pull_secret = resolve_pull_secret(ctx)?;
    let ssh_key = ctx.input("ssh_key");
    let resource_tags = ctx.input("resource_tags").unwrap_or("");
    let user_tags = build_user_tags(cluster_name, resource_tags);
    ctx.log(format!(
        "tagging all cloud resources with: {}",
        user_tags
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ")
    ));

    let config = render_install_config(
        cluster_name,
        base_domain,
        region,
        control_plane_type,
        control_plane_count,
        worker_type,
        worker_count,
        &pull_secret,
        ssh_key,
        &user_tags,
    );

    let dir = cluster_dir(ctx);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Err(StepOutcome::Failed {
            error: format!("could not create cluster dir {}: {e}", dir.display()),
            next_steps: vec!["Check filesystem permissions for the artifacts directory.".to_string()],
        });
    }
    let path = dir.join("install-config.yaml");
    if let Err(e) = std::fs::write(&path, config) {
        return Err(StepOutcome::Failed {
            error: format!("could not write {}: {e}", path.display()),
            next_steps: vec!["Check filesystem permissions for the artifacts directory.".to_string()],
        });
    }
    ctx.log(format!("wrote install-config.yaml to {}", path.display()));
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use sw_core::{CommandRunner, EventBus, MockCommandRunner, MockResponse};

    /// Build a StepContext with the given inputs/secrets and a temp artifacts dir.
    fn ctx_with(
        runner: Arc<dyn CommandRunner>,
        inputs: BTreeMap<String, String>,
        secrets: BTreeMap<String, String>,
        artifacts_dir: std::path::PathBuf,
    ) -> StepContext {
        StepContext::with_artifacts(
            "test-run".to_string(),
            "mod-provision/test".to_string(),
            runner,
            EventBus::new(),
            inputs,
            secrets,
            artifacts_dir,
        )
    }

    /// A unique temp dir for an artifacts root.
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("sw-mod-provision-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn full_spec_inputs() -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert("cluster_name".into(), "wxd-test".into());
        m.insert("region".into(), "us-west-2".into());
        m.insert("base_domain".into(), "example.com".into());
        m.insert("control_plane_type".into(), "m6i.2xlarge".into());
        m.insert("control_plane_count".into(), "3".into());
        m.insert("worker_type".into(), "m6i.4xlarge".into());
        m.insert("worker_count".into(), "3".into());
        m.insert("resource_tags".into(), "owner=qa,project=wxd".into());
        m
    }

    /// A cluster-spec step backed by the default (AWS) provisioner registry.
    fn cluster_spec() -> ClusterSpecStep {
        ClusterSpecStep { reg: std::sync::Arc::new(ProvisionerRegistry::new()) }
    }

    #[tokio::test]
    async fn cluster_spec_needs_input_when_empty() {
        let ctx = ctx_with(
            Arc::new(MockCommandRunner::new(vec![])),
            BTreeMap::new(),
            BTreeMap::new(),
            temp_dir("spec-empty"),
        );
        let outcome = cluster_spec().run(&ctx).await;
        match outcome {
            StepOutcome::NeedsInput { fields, .. } => {
                assert_eq!(fields.len(), 10, "should request all spec fields");
                let keys: Vec<&str> = fields.iter().map(|f| f.key.as_str()).collect();
                assert!(keys.contains(&"cluster_name"));
                assert!(keys.contains(&"region"));
                assert!(keys.contains(&"base_domain"));
                assert!(keys.contains(&"ocp_version"));
                assert!(keys.contains(&"worker_count"));
                assert!(keys.contains(&"resource_tags"));
            }
            other => panic!("expected NeedsInput, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cluster_spec_completes_when_all_present() {
        let ctx = ctx_with(
            Arc::new(MockCommandRunner::new(vec![])),
            full_spec_inputs(),
            BTreeMap::new(),
            temp_dir("spec-full"),
        );
        assert_eq!(cluster_spec().run(&ctx).await, StepOutcome::Completed);
    }

    #[test]
    fn ocp_channel_maps_versions_to_mirror_dirs() {
        assert_eq!(ocp_channel("4.21"), "stable-4.21"); // minor → latest patch
        assert_eq!(ocp_channel("4.21.5"), "4.21.5"); // exact
        assert_eq!(ocp_channel("stable-4.20"), "stable-4.20"); // explicit channel kept
        assert_eq!(ocp_channel("latest-4.22"), "latest-4.22");
        assert_eq!(ocp_channel(""), "stable"); // fallback
    }

    #[tokio::test]
    async fn pull_secret_can_come_from_a_file_path() {
        let dir = temp_dir("ps-file");
        // Write a pull-secret file and point the input at it (no pasted secret).
        let ps_file = dir.join("my-pull-secret.json");
        std::fs::write(&ps_file, "{\"auths\":{\"x\":{}}}\n").unwrap();
        let mut inputs = full_spec_inputs();
        inputs.insert("pull_secret_path".into(), ps_file.to_string_lossy().into_owned());

        let ctx = ctx_with(Arc::new(MockCommandRunner::new(vec![])), inputs, BTreeMap::new(), dir.clone());
        assert!(write_install_config(&ctx).is_ok(), "file-based pull secret should resolve");
        let written =
            std::fs::read_to_string(dir.join("cluster").join("install-config.yaml")).unwrap();
        assert!(written.contains("\"auths\":{\"x\":{}}"), "file pull secret not embedded: {written}");
    }

    #[tokio::test]
    async fn missing_pull_secret_prompts_for_paste_or_path() {
        let dir = temp_dir("ps-none");
        let ctx = ctx_with(Arc::new(MockCommandRunner::new(vec![])), full_spec_inputs(), BTreeMap::new(), dir);
        match write_install_config(&ctx) {
            Err(StepOutcome::NeedsInput { fields, .. }) => {
                let keys: Vec<_> = fields.iter().map(|f| f.key.as_str()).collect();
                assert!(keys.contains(&"pull_secret") && keys.contains(&"pull_secret_path"), "got {keys:?}");
            }
            other => panic!("expected NeedsInput offering both, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn write_install_config_writes_region_and_types() {
        let dir = temp_dir("write-config");
        let mut secrets = BTreeMap::new();
        secrets.insert("pull_secret".into(), "{\"auths\":{}}".into());
        let ctx = ctx_with(
            Arc::new(MockCommandRunner::new(vec![])),
            full_spec_inputs(),
            secrets,
            dir.clone(),
        );

        assert!(write_install_config(&ctx).is_ok());

        let written =
            std::fs::read_to_string(dir.join("cluster").join("install-config.yaml")).unwrap();
        assert!(written.contains("region: us-west-2"), "region missing: {written}");
        assert!(written.contains("type: m6i.2xlarge"), "control plane type missing");
        assert!(written.contains("type: m6i.4xlarge"), "worker type missing");
        assert!(written.contains("baseDomain: example.com"));
        assert!(written.contains("pullSecret"));
        // Every resource is tagged with the user-provided name + extra tags.
        // AWS reserves the `Name` key, so we use `cluster-name`.
        assert!(written.contains("userTags:"), "userTags block missing: {written}");
        assert!(written.contains("cluster-name: 'wxd-test'"), "cluster-name tag missing: {written}");
        assert!(!written.contains("\n      Name:"), "reserved Name tag must not appear: {written}");
        assert!(written.contains("owner: 'qa'"), "extra tag missing: {written}");
    }

    #[test]
    fn parse_tags_handles_blanks_and_pairs() {
        assert_eq!(
            parse_tags("owner=qa, project=wxd , =bad, nokeyval, "),
            vec![("owner".to_string(), "qa".to_string()), ("project".to_string(), "wxd".to_string())]
        );
    }

    #[test]
    fn build_user_tags_uses_non_reserved_key_and_drops_reserved() {
        let tags = build_user_tags("my-cluster", "owner=qa,Name=nope,kubernetes.io/x=y");
        assert_eq!(tags[0], ("cluster-name".to_string(), "my-cluster".to_string()));
        assert!(tags.contains(&("owner".to_string(), "qa".to_string())));
        // Reserved keys are excluded.
        assert!(!tags.iter().any(|(k, _)| k.eq_ignore_ascii_case("Name")));
        assert!(!tags.iter().any(|(k, _)| k.starts_with("kubernetes.io/")));
    }

    #[test]
    fn parse_public_zones_filters_private() {
        let json = r#"{"HostedZones":[
            {"Name":"ocpcpdtest.com.","Id":"/hostedzone/Z1","Config":{"PrivateZone":false}},
            {"Name":"internal.example.","Id":"/hostedzone/Z2","Config":{"PrivateZone":true}},
            {"Name":"ibm-cpd-partnerships.com.","Id":"/hostedzone/Z3","Config":{"PrivateZone":false}}
        ]}"#;
        let zones = parse_public_zones(json);
        assert_eq!(
            zones,
            vec![
                ("ocpcpdtest.com.".to_string(), "/hostedzone/Z1".to_string()),
                ("ibm-cpd-partnerships.com.".to_string(), "/hostedzone/Z3".to_string()),
            ]
        );
    }

    #[test]
    fn find_parent_zone_picks_longest_match() {
        let zones = vec![
            ("ocpcpdtest.com.".to_string(), "/hostedzone/Z1".to_string()),
            ("example.com.".to_string(), "/hostedzone/Z2".to_string()),
        ];
        let p = find_parent_zone(&zones, "wxd.ocpcpdtest.com").unwrap();
        assert_eq!(p.0, "ocpcpdtest.com.");
        // exact match is not a "parent"
        assert!(find_parent_zone(&zones, "example.com").is_none());
        // unrelated apex has no parent
        assert!(find_parent_zone(&zones, "swwxdinstallpractice.com").is_none());
    }

    #[test]
    fn parse_delegation_ns_reads_name_servers() {
        let json = r#"{"DelegationSet":{"NameServers":["ns-1.awsdns-01.com","ns-2.awsdns-02.net"]}}"#;
        assert_eq!(parse_delegation_ns(json), vec!["ns-1.awsdns-01.com", "ns-2.awsdns-02.net"]);
    }

    fn r53_list(zones_json: &str) -> MockResponse {
        MockResponse::ok("route53 list-hosted-zones", zones_json)
    }

    #[tokio::test]
    async fn ensure_dns_passes_when_zone_exists() {
        let dir = temp_dir("dns-exists");
        let runner = Arc::new(MockCommandRunner::new(vec![r53_list(
            r#"{"HostedZones":[{"Name":"example.com.","Id":"/hostedzone/Z1","Config":{"PrivateZone":false}}]}"#,
        )]));
        let ctx = ctx_with(runner, full_spec_inputs(), BTreeMap::new(), dir);
        assert_eq!(aws_ensure_dns(&ctx).await, StepOutcome::Completed);
    }

    #[tokio::test]
    async fn ensure_dns_fails_without_optin() {
        let dir = temp_dir("dns-no-optin");
        let runner = Arc::new(MockCommandRunner::new(vec![r53_list(
            r#"{"HostedZones":[{"Name":"ocpcpdtest.com.","Id":"/hostedzone/Z1","Config":{"PrivateZone":false}}]}"#,
        )]));
        // base_domain=example.com not present, opt-in not set.
        let ctx = ctx_with(runner, full_spec_inputs(), BTreeMap::new(), dir);
        match aws_ensure_dns(&ctx).await {
            StepOutcome::Failed { next_steps, .. } => {
                assert!(next_steps.iter().any(|s| s.contains("ocpcpdtest.com")));
                assert!(next_steps.iter().any(|s| s.contains("Create the base-domain")));
            }
            o => panic!("expected Failed, got {o:?}"),
        }
    }

    #[tokio::test]
    async fn ensure_dns_creates_and_delegates_subdomain() {
        let dir = temp_dir("dns-create");
        let mut inputs = full_spec_inputs();
        inputs.insert("base_domain".into(), "wxd.ocpcpdtest.com".into());
        inputs.insert("create_base_domain_zone".into(), "true".into());
        let runner = Arc::new(MockCommandRunner::new(vec![
            r53_list(r#"{"HostedZones":[{"Name":"ocpcpdtest.com.","Id":"/hostedzone/ZPARENT","Config":{"PrivateZone":false}}]}"#),
            MockResponse::ok("create-hosted-zone", r#"{"DelegationSet":{"NameServers":["ns-1.awsdns-01.com","ns-2.awsdns-02.net"]}}"#),
            MockResponse::ok("change-resource-record-sets", "{}"),
        ]));
        let ctx = ctx_with(runner.clone(), inputs, BTreeMap::new(), dir);
        assert_eq!(aws_ensure_dns(&ctx).await, StepOutcome::Completed);
        let calls = runner.calls();
        assert!(calls.iter().any(|c| c.contains("create-hosted-zone")), "{calls:?}");
        assert!(calls.iter().any(|c| c.contains("change-resource-record-sets")), "{calls:?}");
    }

    #[tokio::test]
    async fn ensure_dns_apex_without_parent_asks_for_registrar_delegation() {
        let dir = temp_dir("dns-apex");
        let mut inputs = full_spec_inputs();
        inputs.insert("base_domain".into(), "swwxdinstallpractice.com".into());
        inputs.insert("create_base_domain_zone".into(), "true".into());
        let runner = Arc::new(MockCommandRunner::new(vec![
            r53_list(r#"{"HostedZones":[{"Name":"ocpcpdtest.com.","Id":"/hostedzone/Z1","Config":{"PrivateZone":false}}]}"#),
            MockResponse::ok("create-hosted-zone", r#"{"DelegationSet":{"NameServers":["ns-9.awsdns-09.org"]}}"#),
        ]));
        let ctx = ctx_with(runner, inputs, BTreeMap::new(), dir);
        match aws_ensure_dns(&ctx).await {
            StepOutcome::Failed { next_steps, .. } => {
                assert!(next_steps.iter().any(|s| s.contains("registrar")));
                assert!(next_steps.iter().any(|s| s.contains("ns-9.awsdns-09.org")));
            }
            o => panic!("expected Failed asking for registrar delegation, got {o:?}"),
        }
    }

    #[test]
    fn cluster_name_validation() {
        assert!(is_valid_cluster_name("wxd"));
        assert!(is_valid_cluster_name("sw-wxd-install-prac-auto1"));
        assert!(!is_valid_cluster_name("sw_wxd_install_prac_auto1")); // underscores
        assert!(!is_valid_cluster_name("WXD")); // uppercase
        assert!(!is_valid_cluster_name("-wxd")); // leading dash
        assert!(!is_valid_cluster_name(""));
        assert_eq!(sanitize_cluster_name("sw_wxd_install_prac_auto1"), "sw-wxd-install-prac-auto1");
    }

    #[tokio::test]
    async fn cluster_spec_reprompts_on_invalid_name() {
        let mut inputs = full_spec_inputs();
        inputs.insert("cluster_name".into(), "Bad_Name".into());
        let ctx = ctx_with(
            Arc::new(MockCommandRunner::new(vec![])),
            inputs,
            BTreeMap::new(),
            temp_dir("spec-badname"),
        );
        match cluster_spec().run(&ctx).await {
            StepOutcome::NeedsInput { fields, .. } => {
                assert_eq!(fields[0].key, "cluster_name");
                assert_eq!(fields[0].default.as_deref(), Some("bad-name"));
            }
            o => panic!("expected NeedsInput re-prompt, got {o:?}"),
        }
    }

    #[tokio::test]
    async fn create_cluster_success_path() {
        let dir = temp_dir("create-ok");
        let runner = Arc::new(MockCommandRunner::new(vec![MockResponse::ok(
            "create cluster",
            "INFO Install complete!",
        )]));
        let mut secrets = BTreeMap::new();
        secrets.insert("pull_secret".to_string(), "{\"auths\":{}}".to_string());
        let ctx = ctx_with(runner.clone(), full_spec_inputs(), secrets, dir);
        assert_eq!(CreateClusterStep::new().run(&ctx).await, StepOutcome::Completed);
        // Confirm the work went through the runner.
        assert!(
            runner.calls().iter().any(|c| c.contains("openshift-install create cluster")),
            "expected openshift-install invocation, got {:?}",
            runner.calls()
        );
    }

    #[tokio::test]
    async fn create_cluster_skips_only_when_completion_marker_exists() {
        let dir = temp_dir("create-skip");
        // The completion marker — not a mere kubeconfig — is the idempotency
        // signal (a kubeconfig appears during bootstrap, before completion).
        let cluster = dir.join("cluster");
        std::fs::create_dir_all(&cluster).unwrap();
        std::fs::write(cluster.join(".wxd_install_complete"), "ok\n").unwrap();

        let runner = Arc::new(MockCommandRunner::new(vec![]));
        let ctx = ctx_with(
            runner.clone(),
            full_spec_inputs(),
            BTreeMap::new(),
            dir,
        );
        assert_eq!(CreateClusterStep::new().run(&ctx).await, StepOutcome::Completed);
        // Idempotent skip: the installer must NOT have been invoked.
        assert!(
            runner.calls().is_empty(),
            "expected no commands on skip path, got {:?}",
            runner.calls()
        );
    }

    #[tokio::test]
    async fn create_cluster_resumes_when_started_but_not_complete() {
        let dir = temp_dir("create-resume");
        // Simulate an interrupted install: state + kubeconfig exist, but no
        // completion marker. Must resume via wait-for, not re-run create.
        let cluster = dir.join("cluster");
        let auth = cluster.join("auth");
        std::fs::create_dir_all(&auth).unwrap();
        std::fs::write(cluster.join(".openshift_install_state.json"), "{}").unwrap();
        std::fs::write(auth.join("kubeconfig"), "apiVersion: v1").unwrap();

        let runner = Arc::new(MockCommandRunner::new(vec![]));
        let ctx = ctx_with(runner.clone(), full_spec_inputs(), BTreeMap::new(), dir);
        assert_eq!(CreateClusterStep::new().run(&ctx).await, StepOutcome::Completed);
        let calls = runner.calls();
        assert!(
            calls.iter().any(|c| c.contains("wait-for install-complete")),
            "expected wait-for resume, got {calls:?}"
        );
        assert!(
            !calls.iter().any(|c| c.contains("create cluster")),
            "must not re-run create cluster on resume, got {calls:?}"
        );
    }

    #[tokio::test]
    async fn create_cluster_failure_path() {
        let dir = temp_dir("create-fail");
        let runner = Arc::new(MockCommandRunner::new(vec![MockResponse::fail(
            "create cluster",
            1,
            "quota exceeded",
        )]));
        let mut secrets = BTreeMap::new();
        secrets.insert("pull_secret".to_string(), "{\"auths\":{}}".to_string());
        let ctx = ctx_with(runner, full_spec_inputs(), secrets, dir);
        match CreateClusterStep::new().run(&ctx).await {
            StepOutcome::Failed { error, next_steps } => {
                assert!(error.contains("quota exceeded"), "error: {error}");
                assert!(!next_steps.is_empty(), "next_steps must guide the user");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn preflight_fails_when_sts_fails() {
        let dir = temp_dir("preflight-fail");
        let runner = Arc::new(MockCommandRunner::new(vec![
            MockResponse::ok("openshift-install version", "4.16.0"),
            MockResponse::ok("aws --version", "aws-cli/2.0"),
            MockResponse::fail("sts get-caller-identity", 255, "ExpiredToken"),
        ]));
        let ctx = ctx_with(runner, full_spec_inputs(), BTreeMap::new(), dir);
        match aws_preflight(&ctx).await {
            StepOutcome::Failed { error, next_steps } => {
                assert!(error.contains("ExpiredToken"), "error: {error}");
                assert!(!next_steps.is_empty());
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn preflight_passes_when_all_ok() {
        let dir = temp_dir("preflight-ok");
        // base_domain in full_spec_inputs() is example.com — the Route53 list
        // must report it as a public zone for preflight to pass.
        let runner = Arc::new(MockCommandRunner::new(vec![MockResponse::ok(
            "route53 list-hosted-zones",
            r#"{"HostedZones":[{"Name":"example.com.","Config":{"PrivateZone":false}}]}"#,
        )]));
        let ctx = ctx_with(runner, full_spec_inputs(), BTreeMap::new(), dir);
        assert_eq!(aws_preflight(&ctx).await, StepOutcome::Completed);
    }

    #[test]
    fn module_exposes_four_ordered_steps() {
        let m = ProvisionModule::new();
        assert_eq!(m.id(), "mod-provision");
        let ids: Vec<String> = m.steps().iter().map(|s| s.id().to_string()).collect();
        assert_eq!(
            ids,
            vec![
                "cluster-spec",
                "preflight-aws",
                "ensure-dns-zone",
                "write-install-config",
                "create-cluster"
            ]
        );
    }

    #[test]
    fn provisioner_id_is_aws() {
        assert_eq!(AwsProvisioner::new().id(), "aws");
    }

    #[test]
    fn parse_arn_handles_both_shapes() {
        // Slash form: arn:aws:ec2:<region>:<acct>:natgateway/<id>
        let (s, t, id) = parse_arn("arn:aws:ec2:us-east-1:123456789012:natgateway/nat-0abc");
        assert_eq!((s.as_str(), t.as_str(), id.as_str()), ("ec2", "natgateway", "nat-0abc"));

        // Slash form: instance
        let (s, t, id) = parse_arn("arn:aws:ec2:us-east-1:123456789012:instance/i-0123456789abcdef0");
        assert_eq!((s.as_str(), t.as_str(), id.as_str()), ("ec2", "instance", "i-0123456789abcdef0"));

        // Slash form: security-group
        let (s, t, id) = parse_arn("arn:aws:ec2:us-east-1:123456789012:security-group/sg-0abc");
        assert_eq!((s.as_str(), t.as_str(), id.as_str()), ("ec2", "security-group", "sg-0abc"));

        // Colon form: elasticfilesystem:...:file-system:<id>
        let (s, t, id) = parse_arn("arn:aws:elasticfilesystem:us-east-1:123456789012:file-system/fs-0abc");
        assert_eq!((s.as_str(), t.as_str(), id.as_str()), ("elasticfilesystem", "file-system", "fs-0abc"));

        // RDS uses the colon separator: arn:aws:rds:<region>:<acct>:db:<id>
        let (s, t, id) = parse_arn("arn:aws:rds:us-east-1:123456789012:db:mydb");
        assert_eq!((s.as_str(), t.as_str(), id.as_str()), ("rds", "db", "mydb"));

        // No separator (e.g. S3 bucket) — whole resource is the id.
        let (s, t, id) = parse_arn("arn:aws:s3:::my-bucket");
        assert_eq!((s.as_str(), t.as_str(), id.as_str()), ("s3", "", "my-bucket"));
    }

    #[test]
    fn billable_classifier_flags_cost_bearing_types() {
        for t in ["instance", "natgateway", "elastic-ip", "eip", "address", "load-balancer", "elb", "elbv2", "volume", "file-system", "efs", "db", "rds", "fsx"] {
            assert!(is_billable(t), "{t} should be billable");
        }
        for t in ["security-group", "subnet", "vpc", "route-table", "internet-gateway", ""] {
            assert!(!is_billable(t), "{t} should NOT be billable");
        }
    }

    #[test]
    fn parses_resource_arns_from_tagging_api() {
        let json = r#"{"ResourceTagMappingList":[
            {"ResourceARN":"arn:aws:ec2:us-east-1:1:natgateway/nat-1"},
            {"ResourceARN":"arn:aws:ec2:us-east-1:1:security-group/sg-1"}
        ]}"#;
        assert_eq!(
            parse_resource_arns(json),
            vec![
                "arn:aws:ec2:us-east-1:1:natgateway/nat-1".to_string(),
                "arn:aws:ec2:us-east-1:1:security-group/sg-1".to_string(),
            ]
        );
    }

    fn write_metadata(dir: &std::path::Path, infra: &str) {
        let cluster = dir.join("cluster");
        std::fs::create_dir_all(&cluster).unwrap();
        std::fs::write(cluster.join("metadata.json"), format!("{{\"infraID\":\"{infra}\"}}")).unwrap();
    }

    #[tokio::test]
    async fn destroy_emits_report_flagging_billable_leftovers() {
        let dir = temp_dir("destroy-report");
        write_metadata(&dir, "cl-abc12");
        // EFS teardown: no filesystem found (skip). Then openshift-install destroy
        // succeeds. Then get-resources returns one natgateway + one security-group
        // for the first tag key, none for the second.
        let runner = Arc::new(MockCommandRunner::new(vec![
            MockResponse::ok("efs describe-file-systems", "{\"FileSystems\":[]}"),
            MockResponse::ok("destroy cluster", "INFO destroy complete"),
            MockResponse::ok(
                "get-resources",
                r#"{"ResourceTagMappingList":[
                    {"ResourceARN":"arn:aws:ec2:us-east-1:1:natgateway/nat-1"},
                    {"ResourceARN":"arn:aws:ec2:us-east-1:1:security-group/sg-1"}
                ]}"#,
            ),
            // Second tag key — nothing.
            MockResponse::ok("get-resources", r#"{"ResourceTagMappingList":[]}"#),
        ]));
        let ctx = ctx_with(runner.clone(), full_spec_inputs(), BTreeMap::new(), dir.clone());

        assert_eq!(AwsProvisioner::new().destroy(&ctx).await, StepOutcome::Completed);

        // The report artifact exists and flags the natgateway as billable.
        let report = std::fs::read_to_string(dir.join("destroy-report.txt")).unwrap();
        assert!(report.contains("REMAINING [BILLABLE] ec2/natgateway nat-1"), "report: {report}");
        assert!(report.contains("REMAINING ec2/security-group sg-1"), "report: {report}");
        assert!(report.contains("2 resource(s) still tagged"), "report: {report}");
        assert!(report.contains("1 billable"), "report: {report}");

        // The structured JSON report is also written.
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("destroy-report.json")).unwrap()).unwrap();
        assert_eq!(json["infra_id"], "cl-abc12");
        assert_eq!(json["remaining"].as_array().unwrap().len(), 2);

        // EFS teardown ran before the cluster destroy.
        let calls = runner.calls();
        let efs_pos = calls.iter().position(|c| c.contains("efs describe-file-systems")).unwrap();
        let destroy_pos = calls.iter().position(|c| c.contains("destroy cluster")).unwrap();
        assert!(efs_pos < destroy_pos, "EFS teardown must precede cluster destroy: {calls:?}");
    }

    #[tokio::test]
    async fn destroy_failure_still_emits_report() {
        let dir = temp_dir("destroy-fail-report");
        write_metadata(&dir, "cl-xyz99");
        let runner = Arc::new(MockCommandRunner::new(vec![
            MockResponse::ok("efs describe-file-systems", "{\"FileSystems\":[]}"),
            MockResponse::fail("destroy cluster", 1, "dependency violation"),
            MockResponse::ok("get-resources", r#"{"ResourceTagMappingList":[]}"#),
            MockResponse::ok("get-resources", r#"{"ResourceTagMappingList":[]}"#),
        ]));
        let ctx = ctx_with(runner, full_spec_inputs(), BTreeMap::new(), dir.clone());
        match AwsProvisioner::new().destroy(&ctx).await {
            StepOutcome::Failed { error, .. } => assert!(error.contains("dependency violation"), "{error}"),
            o => panic!("expected Failed, got {o:?}"),
        }
        // Report is still written even on destroy failure.
        let report = std::fs::read_to_string(dir.join("destroy-report.txt")).unwrap();
        assert!(report.contains("all tagged resources deleted"), "report: {report}");
    }

    #[tokio::test]
    async fn destroy_tears_down_efs_with_mount_targets() {
        let dir = temp_dir("destroy-efs");
        write_metadata(&dir, "cl-efs01");
        let runner = Arc::new(MockCommandRunner::new(vec![
            // describe-file-systems → one filesystem
            MockResponse::ok("efs describe-file-systems", "{\"FileSystems\":[{\"FileSystemId\":\"fs-9\"}]}"),
            // describe-mount-targets → one mount target
            MockResponse::ok("efs describe-mount-targets", "{\"MountTargets\":[{\"MountTargetId\":\"fsmt-1\"}]}"),
            // delete-mount-target
            MockResponse::ok("efs delete-mount-target", "{}"),
            // re-poll describe-mount-targets → now empty
            MockResponse::ok("efs describe-mount-targets", "{\"MountTargets\":[]}"),
            // delete-file-system
            MockResponse::ok("efs delete-file-system", "{}"),
            // cluster destroy
            MockResponse::ok("destroy cluster", "INFO destroy complete"),
            // report
            MockResponse::ok("get-resources", r#"{"ResourceTagMappingList":[]}"#),
            MockResponse::ok("get-resources", r#"{"ResourceTagMappingList":[]}"#),
        ]));
        let ctx = ctx_with(runner.clone(), full_spec_inputs(), BTreeMap::new(), dir);
        assert_eq!(AwsProvisioner::new().destroy(&ctx).await, StepOutcome::Completed);
        let calls = runner.calls();
        assert!(calls.iter().any(|c| c.contains("efs delete-mount-target --mount-target-id fsmt-1")), "{calls:?}");
        assert!(calls.iter().any(|c| c.contains("efs delete-file-system --file-system-id fs-9")), "{calls:?}");
    }
}
