//! `sw-mod-storage` — provisions an RWX **file** storage class for Software Hub
//! and watsonx.data on a freshly-provisioned AWS cluster.
//!
//! A bare OpenShift IPI cluster on AWS only has EBS (`gp3-csi`, block/RWO). The
//! Software Hub control plane and watsonx.data also need a ReadWriteMany (file)
//! class, so this module stands up **AWS EFS**:
//!   1. `ensure-efs`      — create an EFS filesystem + per-subnet mount targets,
//!                          allow NFS in the node security group.
//!   2. `install-efs-csi` — install the AWS EFS CSI Driver Operator.
//!   3. `efs-storage-class` — create the `efs-sc` StorageClass.
//!
//! Everything goes through the `CommandRunner` seam (`aws` + `oc`), so it stays
//! hermetically testable. AWS-only today; for other clouds (or an existing
//! cluster that already has RWX storage) the steps skip cleanly.

use async_trait::async_trait;
use sw_core::{Module, Step, StepContext, StepOutcome};

/// The provider this run targets (defaults to AWS).
fn provider_id(ctx: &StepContext) -> String {
    ctx.input("hyperscaler").unwrap_or("aws").to_string()
}

/// True when EFS provisioning applies (AWS new-cluster provisioning).
fn applies(ctx: &StepContext) -> bool {
    provider_id(ctx) == "aws"
}

/// AWS region/credentials passed to the `aws` CLI, sourced from run secrets
/// (entered in the UI) and the `region` input. Empty when relying on `~/.aws`.
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

/// The cluster infra ID (e.g. `swwxd-w4lcm`), read from the provisioner's
/// `metadata.json`. It tags every cluster resource and disambiguates the VPC.
fn infra_id(ctx: &StepContext) -> Option<String> {
    let path = ctx.artifacts_dir().join("cluster").join("metadata.json");
    let body = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    v.get("infraID").and_then(|x| x.as_str()).map(String::from)
}

/// The cluster ownership tag key for the given infra ID.
fn cluster_tag(infra: &str) -> String {
    format!("kubernetes.io/cluster/{infra}")
}

/// The EFS filesystem id chosen for this cluster, persisted to an artifact so
/// later steps (and retries) reuse it.
fn fs_id_path(ctx: &StepContext) -> std::path::PathBuf {
    ctx.artifacts_dir().join("efs-fs-id.txt")
}

fn read_fs_id(ctx: &StepContext) -> Option<String> {
    std::fs::read_to_string(fs_id_path(ctx)).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

// ---- AWS JSON parsing (pure; unit-tested) ---------------------------------

/// First filesystem id in `aws efs describe-file-systems` / `create-file-system`
/// JSON, if any (handles both the `FileSystems[]` list and a bare object).
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

/// `(vpc_id, [private subnet ids])` from `aws ec2 describe-subnets` JSON. A
/// subnet is treated as private when it does not auto-assign public IPs.
fn parse_private_subnets(json: &str) -> (Option<String>, Vec<String>) {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return (None, Vec::new()),
    };
    let mut vpc = None;
    let mut subnets = Vec::new();
    if let Some(arr) = v.get("Subnets").and_then(|s| s.as_array()) {
        for s in arr {
            let public = s.get("MapPublicIpOnLaunch").and_then(|b| b.as_bool()).unwrap_or(false);
            // Some installs also tag subnet roles; prefer the public-IP signal,
            // but treat an explicit `*private*` Name tag as private too.
            let name_private = s
                .get("Tags")
                .and_then(|t| t.as_array())
                .map(|tags| {
                    tags.iter().any(|t| {
                        t.get("Key").and_then(|k| k.as_str()) == Some("Name")
                            && t.get("Value").and_then(|x| x.as_str()).is_some_and(|n| n.contains("private"))
                    })
                })
                .unwrap_or(false);
            if !public || name_private {
                if let Some(id) = s.get("SubnetId").and_then(|x| x.as_str()) {
                    subnets.push(id.to_string());
                }
            }
            if vpc.is_none() {
                vpc = s.get("VpcId").and_then(|x| x.as_str()).map(String::from);
            }
        }
    }
    (vpc, subnets)
}

/// The node/worker security group id from `aws ec2 describe-security-groups`
/// JSON (prefers a group whose name contains `node`, else `worker`, else first).
fn parse_node_sg(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let groups = v.get("SecurityGroups").and_then(|g| g.as_array())?;
    let pick = |needle: &str| {
        groups.iter().find(|g| {
            g.get("GroupName").and_then(|n| n.as_str()).is_some_and(|n| n.contains(needle))
        })
    };
    let g = pick("node").or_else(|| pick("worker")).or_else(|| groups.first())?;
    g.get("GroupId").and_then(|x| x.as_str()).map(String::from)
}

/// Subnet ids that already have a mount target, from `aws efs
/// describe-mount-targets` JSON.
fn parse_mount_target_subnets(json: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("MountTargets").and_then(|m| m.as_array()).cloned())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("SubnetId").and_then(|x| x.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

// ---- steps ----------------------------------------------------------------

/// Provision the EFS filesystem, mount targets, and NFS access.
struct EnsureEfs;

#[async_trait]
impl Step for EnsureEfs {
    fn id(&self) -> &str {
        "ensure-efs"
    }
    fn title(&self) -> &str {
        "Provision EFS (RWX storage)"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        if !applies(ctx) {
            ctx.log("EFS storage applies to AWS provisioning; skipping");
            ctx.progress(100);
            return StepOutcome::Completed;
        }
        let Some(infra) = infra_id(ctx) else {
            return StepOutcome::Failed {
                error: "could not read cluster infra ID (metadata.json) for EFS provisioning".into(),
                next_steps: vec!["Ensure the cluster was provisioned in this run, then retry.".into()],
            };
        };
        let env = aws_env(ctx);
        let token = format!("{infra}-efs");

        // 1. Filesystem (idempotent via creation token).
        let fs_id = match read_fs_id(ctx) {
            Some(id) => {
                ctx.log(format!("reusing EFS filesystem {id}"));
                id
            }
            None => {
                let existing = ctx
                    .run_with_env(
                        "aws",
                        &["efs".into(), "describe-file-systems".into(), "--creation-token".into(), token.clone(), "--output".into(), "json".into()],
                        &env,
                    )
                    .await;
                let found = existing.ok().filter(|o| o.success()).and_then(|o| parse_fs_id(&o.stdout));
                let id = match found {
                    Some(id) => id,
                    None => {
                        ctx.log("creating EFS filesystem");
                        let created = ctx
                            .run_with_env(
                                "aws",
                                &[
                                    "efs".into(), "create-file-system".into(),
                                    "--creation-token".into(), token.clone(),
                                    "--encrypted".into(),
                                    "--performance-mode".into(), "generalPurpose".into(),
                                    "--tags".into(),
                                    format!("Key=Name,Value={token}"),
                                    format!("Key={},Value=owned", cluster_tag(&infra)),
                                    "--output".into(), "json".into(),
                                ],
                                &env,
                            )
                            .await;
                        match created {
                            Ok(o) if o.success() => match parse_fs_id(&o.stdout) {
                                Some(id) => id,
                                None => return fail("created EFS but could not read its id", "Inspect EFS in the AWS console, then retry."),
                            },
                            Ok(o) => return fail(&format!("create-file-system failed (exit {}): {}", o.status, o.stderr.trim()), "Check EFS service quotas/permissions (elasticfilesystem:*), then retry."),
                            Err(e) => return fail(&format!("could not run aws efs create-file-system: {e}"), "Ensure the aws CLI is installed, then retry."),
                        }
                    }
                };
                let _ = std::fs::write(fs_id_path(ctx), format!("{id}\n"));
                id
            }
        };

        // 2. Network: VPC, private subnets, node SG.
        let subnets_out = ctx
            .run_with_env(
                "aws",
                &[
                    "ec2".into(), "describe-subnets".into(),
                    "--filters".into(), format!("Name=tag:{},Values=owned", cluster_tag(&infra)),
                    "--output".into(), "json".into(),
                ],
                &env,
            )
            .await;
        let (vpc, subnets) = match subnets_out {
            Ok(o) if o.success() => parse_private_subnets(&o.stdout),
            Ok(o) => return fail(&format!("describe-subnets failed (exit {}): {}", o.status, o.stderr.trim()), "Check EC2 read permissions, then retry."),
            Err(e) => return fail(&format!("could not run aws ec2 describe-subnets: {e}"), "Ensure the aws CLI is installed, then retry."),
        };
        if subnets.is_empty() {
            return fail("found no private subnets tagged for this cluster", "Confirm the cluster provisioned its VPC/subnets, then retry.");
        }
        let Some(vpc) = vpc else {
            return fail("could not determine the cluster VPC", "Confirm the cluster VPC exists, then retry.");
        };

        let sg_out = ctx
            .run_with_env(
                "aws",
                &[
                    "ec2".into(), "describe-security-groups".into(),
                    "--filters".into(), format!("Name=vpc-id,Values={vpc}"),
                    format!("Name=tag:{},Values=owned", cluster_tag(&infra)),
                    "--output".into(), "json".into(),
                ],
                &env,
            )
            .await;
        let sg = match sg_out {
            Ok(o) if o.success() => parse_node_sg(&o.stdout),
            Ok(o) => return fail(&format!("describe-security-groups failed (exit {}): {}", o.status, o.stderr.trim()), "Check EC2 read permissions, then retry."),
            Err(e) => return fail(&format!("could not run aws ec2 describe-security-groups: {e}"), "Ensure the aws CLI is installed, then retry."),
        };
        let Some(sg) = sg else {
            return fail("could not find the cluster node security group", "Confirm the cluster security groups exist, then retry.");
        };

        // 3. Allow NFS (2049) within the node SG (idempotent — ignore duplicates).
        let _ = ctx
            .run_with_env(
                "aws",
                &[
                    "ec2".into(), "authorize-security-group-ingress".into(),
                    "--group-id".into(), sg.clone(),
                    "--protocol".into(), "tcp".into(),
                    "--port".into(), "2049".into(),
                    "--source-group".into(), sg.clone(),
                    "--output".into(), "json".into(),
                ],
                &env,
            )
            .await;

        // 4. Mount targets per private subnet (skip subnets already covered).
        let existing_mt = ctx
            .run_with_env(
                "aws",
                &["efs".into(), "describe-mount-targets".into(), "--file-system-id".into(), fs_id.clone(), "--output".into(), "json".into()],
                &env,
            )
            .await;
        let covered = existing_mt.ok().filter(|o| o.success()).map(|o| parse_mount_target_subnets(&o.stdout)).unwrap_or_default();
        for subnet in &subnets {
            if covered.contains(subnet) {
                continue;
            }
            ctx.log(format!("creating EFS mount target in {subnet}"));
            // Ignore MountTargetConflict (created concurrently / already exists).
            let _ = ctx
                .run_with_env(
                    "aws",
                    &[
                        "efs".into(), "create-mount-target".into(),
                        "--file-system-id".into(), fs_id.clone(),
                        "--subnet-id".into(), subnet.clone(),
                        "--security-groups".into(), sg.clone(),
                        "--output".into(), "json".into(),
                    ],
                    &env,
                )
                .await;
        }

        ctx.log(format!("EFS {fs_id} ready with {} mount target(s)", subnets.len()));
        ctx.progress(100);
        StepOutcome::Completed
    }
}

/// Install the AWS EFS CSI Driver Operator + ClusterCSIDriver.
struct InstallEfsCsi;

#[async_trait]
impl Step for InstallEfsCsi {
    fn id(&self) -> &str {
        "install-efs-csi"
    }
    fn title(&self) -> &str {
        "Install AWS EFS CSI driver"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        if !applies(ctx) {
            ctx.progress(100);
            return StepOutcome::Completed;
        }
        // Idempotency: ClusterCSIDriver already present?
        if let Ok(o) = ctx
            .run_in_cluster("oc", &["get".into(), "clustercsidriver".into(), "efs.csi.aws.com".into()])
            .await
        {
            if o.success() {
                ctx.log("EFS CSI driver already installed; skipping");
                ctx.progress(100);
                return StepOutcome::Completed;
            }
        }
        let manifest = ctx.artifacts_dir().join("efs-csi-operator.yaml");
        if let Err(e) = std::fs::write(&manifest, EFS_CSI_OPERATOR_YAML) {
            return fail(&format!("could not write CSI operator manifest: {e}"), "Check filesystem permissions for the artifacts dir, then retry.");
        }
        ctx.log("installing the AWS EFS CSI Driver Operator");
        match ctx
            .run_in_cluster("oc", &["apply".into(), "-f".into(), manifest.to_string_lossy().into_owned()])
            .await
        {
            Ok(o) if o.success() => {
                ctx.progress(100);
                StepOutcome::Completed
            }
            Ok(o) => fail(&format!("oc apply (EFS CSI operator) failed (exit {}): {}", o.status, o.stderr.trim()), "Confirm the redhat-operators catalog source is available, then retry."),
            Err(e) => fail(&format!("could not run oc: {e}"), "Ensure `oc` has an active cluster session, then retry."),
        }
    }
}

/// Create the `efs-sc` StorageClass bound to the provisioned filesystem.
struct EfsStorageClass;

#[async_trait]
impl Step for EfsStorageClass {
    fn id(&self) -> &str {
        "efs-storage-class"
    }
    fn title(&self) -> &str {
        "Create efs-sc storage class"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        if !applies(ctx) {
            ctx.progress(100);
            return StepOutcome::Completed;
        }
        let Some(fs_id) = read_fs_id(ctx) else {
            return fail("EFS filesystem id not found; run the ensure-efs step first", "Retry the ensure-efs step, then this one.");
        };
        let manifest = ctx.artifacts_dir().join("efs-sc.yaml");
        if let Err(e) = std::fs::write(&manifest, efs_storage_class_yaml(&fs_id)) {
            return fail(&format!("could not write storage-class manifest: {e}"), "Check filesystem permissions for the artifacts dir, then retry.");
        }
        ctx.log(format!("creating StorageClass efs-sc (fileSystemId={fs_id})"));
        match ctx
            .run_in_cluster("oc", &["apply".into(), "-f".into(), manifest.to_string_lossy().into_owned()])
            .await
        {
            Ok(o) if o.success() => {
                ctx.log("efs-sc storage class ready (RWX)");
                ctx.progress(100);
                StepOutcome::Completed
            }
            Ok(o) => fail(&format!("oc apply (efs-sc) failed (exit {}): {}", o.status, o.stderr.trim()), "Confirm the EFS CSI driver is installed, then retry."),
            Err(e) => fail(&format!("could not run oc: {e}"), "Ensure `oc` has an active cluster session, then retry."),
        }
    }
}

/// Static manifest for the AWS EFS CSI Driver Operator subscription + driver.
const EFS_CSI_OPERATOR_YAML: &str = "\
apiVersion: operators.coreos.com/v1alpha1
kind: Subscription
metadata:
  name: aws-efs-csi-driver-operator
  namespace: openshift-cluster-csi-drivers
spec:
  channel: stable
  installPlanApproval: Automatic
  name: aws-efs-csi-driver-operator
  source: redhat-operators
  sourceNamespace: openshift-marketplace
---
apiVersion: operator.openshift.io/v1
kind: ClusterCSIDriver
metadata:
  name: efs.csi.aws.com
spec:
  managementState: Managed
";

/// The `efs-sc` StorageClass manifest for the given filesystem id.
fn efs_storage_class_yaml(fs_id: &str) -> String {
    format!(
        "\
apiVersion: storage.k8s.io/v1
kind: StorageClass
metadata:
  name: efs-sc
provisioner: efs.csi.aws.com
parameters:
  provisioningMode: efs-ap
  fileSystemId: {fs_id}
  directoryPerms: \"700\"
  gidRangeStart: \"1000\"
  gidRangeEnd: \"2000\"
  basePath: \"/dynamic\"
reclaimPolicy: Delete
volumeBindingMode: Immediate
"
    )
}

/// Helper to build a Failed outcome with one next-step.
fn fail(error: &str, next: &str) -> StepOutcome {
    StepOutcome::Failed {
        error: error.to_string(),
        next_steps: vec![next.to_string()],
    }
}

/// The storage module: provisions RWX (EFS) storage for the cluster.
pub struct StorageModule;

impl Module for StorageModule {
    fn id(&self) -> &str {
        "mod-storage"
    }
    fn title(&self) -> &str {
        "Provision cluster storage (RWX)"
    }
    fn steps(&self) -> Vec<Box<dyn Step>> {
        vec![Box::new(EnsureEfs), Box::new(InstallEfsCsi), Box::new(EfsStorageClass)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use sw_core::{EventBus, MockCommandRunner, MockResponse};

    fn ctx_with(runner: MockCommandRunner, inputs: &[(&str, &str)], dir: std::path::PathBuf) -> StepContext {
        let inputs: BTreeMap<String, String> =
            inputs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        StepContext::with_artifacts(
            "run".into(),
            "mod-storage/x".into(),
            Arc::new(runner),
            EventBus::new(),
            inputs,
            BTreeMap::new(),
            dir,
        )
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wxd-storage-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_metadata(dir: &std::path::Path, infra: &str) {
        let cluster = dir.join("cluster");
        std::fs::create_dir_all(&cluster).unwrap();
        std::fs::write(cluster.join("metadata.json"), format!("{{\"infraID\":\"{infra}\"}}")).unwrap();
    }

    #[test]
    fn module_exposes_three_steps_in_order() {
        let ids: Vec<_> = StorageModule.steps().iter().map(|s| s.id().to_string()).collect();
        assert_eq!(ids, vec!["ensure-efs", "install-efs-csi", "efs-storage-class"]);
    }

    #[test]
    fn parses_fs_id_from_list_and_object() {
        assert_eq!(parse_fs_id("{\"FileSystemId\":\"fs-1\"}").as_deref(), Some("fs-1"));
        assert_eq!(parse_fs_id("{\"FileSystems\":[{\"FileSystemId\":\"fs-2\"}]}").as_deref(), Some("fs-2"));
        assert_eq!(parse_fs_id("{\"FileSystems\":[]}"), None);
    }

    #[test]
    fn parses_private_subnets_by_public_ip_flag() {
        let json = "{\"Subnets\":[\
            {\"SubnetId\":\"subnet-pub\",\"VpcId\":\"vpc-1\",\"MapPublicIpOnLaunch\":true},\
            {\"SubnetId\":\"subnet-priv\",\"VpcId\":\"vpc-1\",\"MapPublicIpOnLaunch\":false}]}";
        let (vpc, subnets) = parse_private_subnets(json);
        assert_eq!(vpc.as_deref(), Some("vpc-1"));
        assert_eq!(subnets, vec!["subnet-priv".to_string()]);
    }

    #[test]
    fn picks_node_security_group() {
        let json = "{\"SecurityGroups\":[\
            {\"GroupId\":\"sg-master\",\"GroupName\":\"x-controlplane\"},\
            {\"GroupId\":\"sg-node\",\"GroupName\":\"x-node\"}]}";
        assert_eq!(parse_node_sg(json).as_deref(), Some("sg-node"));
    }

    #[test]
    fn parses_covered_mount_target_subnets() {
        let json = "{\"MountTargets\":[{\"SubnetId\":\"subnet-a\"},{\"SubnetId\":\"subnet-b\"}]}";
        assert_eq!(parse_mount_target_subnets(json), vec!["subnet-a", "subnet-b"]);
    }

    #[tokio::test]
    async fn ensure_efs_skips_for_non_aws() {
        let dir = tmp("skip");
        let ctx = ctx_with(MockCommandRunner::new(vec![]), &[("hyperscaler", "gcp")], dir);
        assert_eq!(EnsureEfs.run(&ctx).await, StepOutcome::Completed);
    }

    #[tokio::test]
    async fn ensure_efs_fails_without_metadata() {
        let dir = tmp("nometa");
        let ctx = ctx_with(MockCommandRunner::new(vec![]), &[("hyperscaler", "aws")], dir);
        match EnsureEfs.run(&ctx).await {
            StepOutcome::Failed { .. } => {}
            o => panic!("expected Failed, got {o:?}"),
        }
    }

    #[tokio::test]
    async fn ensure_efs_creates_fs_and_mount_targets() {
        let dir = tmp("create");
        write_metadata(&dir, "cl-abc12");
        let runner = MockCommandRunner::new(vec![
            // describe-file-systems → none yet
            MockResponse::ok("efs describe-file-systems", "{\"FileSystems\":[]}"),
            // create-file-system → new id
            MockResponse::ok("efs create-file-system", "{\"FileSystemId\":\"fs-new\"}"),
            // describe-subnets → one private subnet
            MockResponse::ok("describe-subnets", "{\"Subnets\":[{\"SubnetId\":\"subnet-p\",\"VpcId\":\"vpc-1\",\"MapPublicIpOnLaunch\":false}]}"),
            // describe-security-groups → node sg
            MockResponse::ok("describe-security-groups", "{\"SecurityGroups\":[{\"GroupId\":\"sg-node\",\"GroupName\":\"cl-node\"}]}"),
            // authorize ingress (any)
            MockResponse::ok("authorize-security-group-ingress", "{}"),
            // describe-mount-targets → none covered
            MockResponse::ok("efs describe-mount-targets", "{\"MountTargets\":[]}"),
            // create-mount-target
            MockResponse::ok("efs create-mount-target", "{}"),
        ]);
        let ctx = ctx_with(runner, &[("hyperscaler", "aws"), ("region", "us-east-2")], dir.clone());
        assert_eq!(EnsureEfs.run(&ctx).await, StepOutcome::Completed);
        // The chosen filesystem id is persisted for later steps.
        assert_eq!(read_fs_id(&ctx).as_deref(), Some("fs-new"));
    }

    #[tokio::test]
    async fn storage_class_needs_fs_id() {
        let dir = tmp("sc-nofs");
        let ctx = ctx_with(MockCommandRunner::new(vec![]), &[("hyperscaler", "aws")], dir);
        match EfsStorageClass.run(&ctx).await {
            StepOutcome::Failed { .. } => {}
            o => panic!("expected Failed, got {o:?}"),
        }
    }

    #[test]
    fn storage_class_yaml_embeds_fs_id() {
        let y = efs_storage_class_yaml("fs-xyz");
        assert!(y.contains("fileSystemId: fs-xyz"));
        assert!(y.contains("provisioner: efs.csi.aws.com"));
        assert!(y.contains("name: efs-sc"));
    }
}
