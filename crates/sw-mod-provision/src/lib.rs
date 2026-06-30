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
    CommandRunner, InputField, Module, Step, StepContext, StepOutcome,
};

/// The cloud-agnostic provisioning seam.
///
/// An implementation knows how to materialize, inspect, and tear down an
/// OpenShift cluster on a specific cloud. All work goes through the
/// [`StepContext`]'s command runner so it stays testable; implementations must
/// never call `std::process` directly.
#[async_trait]
pub trait Provisioner: Send + Sync {
    /// Stable identifier for this provisioner (e.g. `"aws"`).
    fn id(&self) -> &str;

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

    async fn create(&self, ctx: &StepContext) -> StepOutcome {
        let dir = cluster_dir(ctx);

        // Idempotency: a kubeconfig means the cluster is already provisioned.
        if kubeconfig_path(ctx).exists() {
            ctx.log("cluster already provisioned");
            publish_kubeconfig(ctx);
            ctx.progress(100);
            return StepOutcome::Completed;
        }

        ctx.log("provisioning OpenShift cluster via openshift-install (AWS IPI)");
        ctx.progress(10);

        let dir_str = dir.to_string_lossy().into_owned();
        let args = vec![
            "create".to_string(),
            "cluster".to_string(),
            "--dir".to_string(),
            dir_str,
        ];

        match ctx.runner().run_with_env("openshift-install", &args, &aws_env(ctx)).await {
            Ok(out) if out.success() => {
                ctx.log("cluster provisioned");
                publish_kubeconfig(ctx);
                ctx.progress(100);
                StepOutcome::Completed
            }
            Ok(out) => StepOutcome::Failed {
                error: format!(
                    "openshift-install create cluster failed (exit {}): {}",
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
        if kubeconfig_path(ctx).exists() {
            StepOutcome::Completed
        } else {
            StepOutcome::Failed {
                error: "no kubeconfig found; cluster does not appear provisioned"
                    .to_string(),
                next_steps: vec![
                    "Run the create-cluster step to provision the cluster.".to_string(),
                ],
            }
        }
    }

    async fn destroy(&self, ctx: &StepContext) -> StepOutcome {
        let dir = cluster_dir(ctx);
        let dir_str = dir.to_string_lossy().into_owned();
        ctx.log("destroying OpenShift cluster via openshift-install");
        let args = vec![
            "destroy".to_string(),
            "cluster".to_string(),
            "--dir".to_string(),
            dir_str,
        ];
        match ctx.runner().run_with_env("openshift-install", &args, &aws_env(ctx)).await {
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
        }
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

/// The provisioning module: contributes the ordered steps that take a run from
/// "no cluster" to "OpenShift cluster ready".
#[derive(Debug, Default, Clone)]
pub struct ProvisionModule;

impl ProvisionModule {
    pub fn new() -> Self {
        Self
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
            Box::new(ClusterSpecStep),
            Box::new(PreflightAwsStep),
            Box::new(WriteInstallConfigStep),
            Box::new(CreateClusterStep::new()),
        ]
    }
}

/// Step 1: collect (or confirm) the cluster spec inputs.
struct ClusterSpecStep;

#[async_trait]
impl Step for ClusterSpecStep {
    fn id(&self) -> &str {
        "cluster-spec"
    }

    fn title(&self) -> &str {
        "Define cluster spec"
    }

    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        let missing = REQUIRED_INPUTS
            .iter()
            .any(|key| ctx.input(key).map(str::is_empty).unwrap_or(true));

        if missing {
            return StepOutcome::NeedsInput {
                prompt: "Provide the OpenShift cluster spec for the watsonx.data \
                         installation."
                    .to_string(),
                fields: spec_fields(),
            };
        }

        // Validate the cluster name up front (openshift-install requires an
        // RFC 1123 name for metadata.name). Re-ask with a valid suggestion rather
        // than silently renaming or failing deep in `create cluster`.
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

/// Step 2: verify the local tooling and AWS credentials are usable.
struct PreflightAwsStep;

impl PreflightAwsStep {
    /// Run one preflight check, returning an error string on failure.
    async fn check(
        runner: &dyn CommandRunner,
        program: &str,
        args: &[String],
        env: &[(String, String)],
        what: &str,
    ) -> Result<(), String> {
        match runner.run_with_env(program, args, env).await {
            Ok(out) if out.success() => Ok(()),
            Ok(out) => Err(format!(
                "{what} failed (exit {}): {}",
                out.status,
                out.stderr.trim()
            )),
            Err(e) => Err(format!("{what}: could not run `{program}`: {e}")),
        }
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

#[async_trait]
impl Step for PreflightAwsStep {
    fn id(&self) -> &str {
        "preflight-aws"
    }

    fn title(&self) -> &str {
        "Preflight AWS"
    }

    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        let runner = ctx.runner();
        let env = aws_env(ctx);

        let checks = [
            (
                "openshift-install",
                vec!["version".to_string()],
                "openshift-install availability",
            ),
            ("aws", vec!["--version".to_string()], "aws CLI availability"),
            (
                "aws",
                vec!["sts".to_string(), "get-caller-identity".to_string()],
                "AWS credentials (aws sts get-caller-identity)",
            ),
        ];

        for (program, args, what) in checks {
            ctx.log(format!("preflight: {what}"));
            if let Err(error) = Self::check(runner, program, &args, &env, what).await {
                return StepOutcome::Failed {
                    error,
                    next_steps: vec![
                        "Install openshift-install (the OpenShift IPI installer) and \
                         put it on PATH."
                            .to_string(),
                        "Install the AWS CLI v2 and put it on PATH.".to_string(),
                        "Configure AWS credentials (aws configure, or AWS_ACCESS_KEY_ID / \
                         AWS_SECRET_ACCESS_KEY) for an account with provisioning rights."
                            .to_string(),
                    ],
                };
            }
        }

        // Validate the base domain is an existing *public* Route53 hosted zone —
        // openshift-install needs one and otherwise fails ~30s in. Catching it
        // here lists the user's actual zones so they can pick a valid one.
        if let Some(base) = ctx.input("base_domain").filter(|d| !d.is_empty()) {
            let base = base.trim_end_matches('.').to_string();
            ctx.log(format!("preflight: Route53 public hosted zone for {base}"));
            let args = vec!["route53".into(), "list-hosted-zones".into(), "--output".into(), "json".into()];
            match runner.run_with_env("aws", &args, &env).await {
                Ok(o) if o.success() => {
                    let zones = parse_public_zones(&o.stdout);
                    let want = format!("{base}.");
                    if !zones.iter().any(|z| z == &want) {
                        let available: Vec<String> =
                            zones.iter().map(|z| z.trim_end_matches('.').to_string()).collect();
                        let listing = if available.is_empty() {
                            "(no public hosted zones found in this account)".to_string()
                        } else {
                            available.join(", ")
                        };
                        return StepOutcome::Failed {
                            error: format!(
                                "'{base}' is not a public Route53 hosted zone in this account"
                            ),
                            next_steps: vec![
                                format!("Use one of your existing public hosted zones as the base domain: {listing}"),
                                format!("…or create a public Route53 hosted zone for '{base}' and delegate the domain to its name servers, then retry."),
                            ],
                        };
                    }
                    ctx.log(format!("base domain '{base}' resolves to a public hosted zone"));
                }
                // Couldn't list zones (e.g. limited IAM) — warn, don't hard-block.
                Ok(o) => ctx.log(format!(
                    "warning: could not verify Route53 zones (exit {}): {}",
                    o.status,
                    o.stderr.trim()
                )),
                Err(e) => ctx.log(format!("warning: could not run aws route53: {e}")),
            }
        }

        ctx.log("preflight passed");
        ctx.progress(100);
        StepOutcome::Completed
    }
}

/// Extract the names of **public** hosted zones from `aws route53
/// list-hosted-zones --output json` output (trailing-dot names, e.g.
/// `example.com.`).
fn parse_public_zones(json: &str) -> Vec<String> {
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
                .filter_map(|z| z.get("Name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Step 3: render and write `install-config.yaml` into the cluster dir.
struct WriteInstallConfigStep;

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
    let pull_secret = match ctx.secret("pull_secret") {
        Some(s) if !s.is_empty() => s,
        _ => {
            return Err(StepOutcome::NeedsInput {
                prompt: "Paste your Red Hat pull secret (from \
                         console.redhat.com/openshift/install/pull-secret). \
                         Optionally add an SSH public key for node debugging."
                    .to_string(),
                fields: vec![
                    InputField {
                        key: "pull_secret".to_string(),
                        label: "Red Hat pull secret (JSON)".to_string(),
                        secret: true,
                        default: None,
                    },
                    InputField {
                        key: "ssh_key".to_string(),
                        label: "SSH public key (optional)".to_string(),
                        secret: false,
                        default: None,
                    },
                ],
            });
        }
    };
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
        pull_secret,
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

#[async_trait]
impl Step for WriteInstallConfigStep {
    fn id(&self) -> &str {
        "write-install-config"
    }

    fn title(&self) -> &str {
        "Write install-config.yaml"
    }

    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        match write_install_config(ctx) {
            Ok(_) => {
                ctx.progress(100);
                StepOutcome::Completed
            }
            Err(outcome) => outcome,
        }
    }
}

/// Step 4: provision the cluster (delegates to a [`Provisioner`]). Idempotent.
struct CreateClusterStep {
    provisioner: AwsProvisioner,
}

impl CreateClusterStep {
    fn new() -> Self {
        Self {
            provisioner: AwsProvisioner::new(),
        }
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
        // Regenerate install-config.yaml from current inputs before the installer
        // has consumed it (first attempt, or a retry after a config-validation
        // failure). Once openshift-install has started (state file present) we
        // leave the dir alone so `create cluster` resumes correctly.
        let started = cluster_dir(ctx)
            .join(".openshift_install_state.json")
            .exists();
        if !kubeconfig_path(ctx).exists() && !started {
            if let Err(outcome) = write_install_config(ctx) {
                return outcome;
            }
        }
        self.provisioner.create(ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use sw_core::{EventBus, MockCommandRunner, MockResponse};

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

    #[tokio::test]
    async fn cluster_spec_needs_input_when_empty() {
        let ctx = ctx_with(
            Arc::new(MockCommandRunner::new(vec![])),
            BTreeMap::new(),
            BTreeMap::new(),
            temp_dir("spec-empty"),
        );
        let outcome = ClusterSpecStep.run(&ctx).await;
        match outcome {
            StepOutcome::NeedsInput { fields, .. } => {
                assert_eq!(fields.len(), 8, "should request all spec fields");
                let keys: Vec<&str> = fields.iter().map(|f| f.key.as_str()).collect();
                assert!(keys.contains(&"cluster_name"));
                assert!(keys.contains(&"region"));
                assert!(keys.contains(&"base_domain"));
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
        assert_eq!(ClusterSpecStep.run(&ctx).await, StepOutcome::Completed);
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

        assert_eq!(
            WriteInstallConfigStep.run(&ctx).await,
            StepOutcome::Completed
        );

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
            {"Name":"ocpcpdtest.com.","Config":{"PrivateZone":false}},
            {"Name":"internal.example.","Config":{"PrivateZone":true}},
            {"Name":"ibm-cpd-partnerships.com.","Config":{"PrivateZone":false}}
        ]}"#;
        let zones = parse_public_zones(json);
        assert_eq!(zones, vec!["ocpcpdtest.com.".to_string(), "ibm-cpd-partnerships.com.".to_string()]);
    }

    #[tokio::test]
    async fn preflight_fails_when_base_domain_not_a_public_zone() {
        let dir = temp_dir("preflight-zone");
        let runner = Arc::new(MockCommandRunner::new(vec![
            MockResponse::ok("openshift-install version", "4.22"),
            MockResponse::ok("aws --version", "aws-cli/2"),
            MockResponse::ok("sts get-caller-identity", "{}"),
            MockResponse::ok(
                "route53 list-hosted-zones",
                r#"{"HostedZones":[{"Name":"ocpcpdtest.com.","Config":{"PrivateZone":false}}]}"#,
            ),
        ]));
        // full_spec_inputs uses base_domain=example.com, which isn't in the list.
        let ctx = ctx_with(runner, full_spec_inputs(), BTreeMap::new(), dir);
        match PreflightAwsStep.run(&ctx).await {
            StepOutcome::Failed { error, next_steps } => {
                assert!(error.contains("public Route53 hosted zone"), "{error}");
                assert!(next_steps.iter().any(|s| s.contains("ocpcpdtest.com")));
            }
            o => panic!("expected Failed for bad base domain, got {o:?}"),
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
        match ClusterSpecStep.run(&ctx).await {
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
    async fn create_cluster_skips_when_kubeconfig_exists() {
        let dir = temp_dir("create-skip");
        // Pre-create the kubeconfig to simulate an already-provisioned cluster.
        let auth = dir.join("cluster").join("auth");
        std::fs::create_dir_all(&auth).unwrap();
        std::fs::write(auth.join("kubeconfig"), "apiVersion: v1").unwrap();

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
        match PreflightAwsStep.run(&ctx).await {
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
        assert_eq!(PreflightAwsStep.run(&ctx).await, StepOutcome::Completed);
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
                "write-install-config",
                "create-cluster"
            ]
        );
    }

    #[test]
    fn provisioner_id_is_aws() {
        assert_eq!(AwsProvisioner::new().id(), "aws");
    }
}
