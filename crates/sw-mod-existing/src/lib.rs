//! `sw-mod-existing` — the "use my existing cluster" path.
//!
//! Instead of provisioning, this module adopts a cluster the user already has by
//! publishing their kubeconfig to the run's standard location
//! (`ctx.kubeconfig_path()`), so the downstream Software Hub and service modules
//! target it via `ctx.run_in_cluster(...)` exactly as they would a freshly
//! provisioned cluster. No cloud resources are created, so there is nothing to tag.

use async_trait::async_trait;
use sw_core::{InputField, Module, Step, StepContext, StepOutcome};

/// Expand a leading `~` to `$HOME` so users can type `~/.kube/config`.
fn expand_home(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return std::path::Path::new(&home).join(rest);
        }
    }
    std::path::PathBuf::from(path)
}

/// Write `contents` to the run's kubeconfig path with `0600` perms.
fn write_kubeconfig(ctx: &StepContext, contents: &str) -> std::io::Result<()> {
    let dst = ctx.kubeconfig_path();
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&dst, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Step 1: obtain the kubeconfig for the existing cluster and publish it.
struct ProvideKubeconfig;

#[async_trait]
impl Step for ProvideKubeconfig {
    fn id(&self) -> &str {
        "provide-kubeconfig"
    }
    fn title(&self) -> &str {
        "Provide cluster kubeconfig"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        // Idempotent: already published (resume/retry).
        if ctx.kubeconfig_path().exists() {
            ctx.log("kubeconfig already present for this run");
            ctx.progress(100);
            return StepOutcome::Completed;
        }

        // Prefer a path on the local machine; fall back to pasted content.
        if let Some(path) = ctx.input("kubeconfig_source_path").filter(|p| !p.is_empty()) {
            let src = expand_home(path);
            match std::fs::read_to_string(&src) {
                Ok(contents) => {
                    if let Err(e) = write_kubeconfig(ctx, &contents) {
                        return StepOutcome::Failed {
                            error: format!("could not write kubeconfig: {e}"),
                            next_steps: vec!["Check filesystem permissions, then retry.".into()],
                        };
                    }
                    ctx.log(format!("adopted kubeconfig from {}", src.display()));
                    ctx.progress(100);
                    return StepOutcome::Completed;
                }
                Err(e) => {
                    return StepOutcome::Failed {
                        error: format!("could not read kubeconfig at {}: {e}", src.display()),
                        next_steps: vec![
                            "Confirm the path is correct and readable on the machine running wxd, then retry."
                                .into(),
                        ],
                    };
                }
            }
        }

        if let Some(contents) = ctx.secret("kubeconfig").filter(|c| !c.is_empty()) {
            if let Err(e) = write_kubeconfig(ctx, contents) {
                return StepOutcome::Failed {
                    error: format!("could not write kubeconfig: {e}"),
                    next_steps: vec!["Check filesystem permissions, then retry.".into()],
                };
            }
            ctx.log("adopted pasted kubeconfig");
            ctx.progress(100);
            return StepOutcome::Completed;
        }

        // Nothing supplied yet — ask.
        StepOutcome::NeedsInput {
            prompt: "Point to your existing OpenShift cluster's kubeconfig. Provide a file \
                     path on this machine (recommended), or paste the kubeconfig contents."
                .into(),
            fields: vec![
                InputField {
                    key: "kubeconfig_source_path".into(),
                    label: "Path to kubeconfig file".into(),
                    secret: false,
                    default: Some("~/.kube/config".into()),
                },
                InputField {
                    key: "kubeconfig".into(),
                    label: "…or paste kubeconfig contents".into(),
                    secret: true,
                    default: None,
                },
            ],
        }
    }
}

/// Step 2: verify we can actually reach the cluster with that kubeconfig.
struct VerifyAccess;

#[async_trait]
impl Step for VerifyAccess {
    fn id(&self) -> &str {
        "verify-access"
    }
    fn title(&self) -> &str {
        "Verify cluster access"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        ctx.log("checking cluster access (oc get nodes)");
        match ctx
            .run_in_cluster("oc", &["get".into(), "nodes".into(), "--no-headers".into()])
            .await
        {
            Ok(o) if o.success() => {
                let nodes = o.stdout.lines().filter(|l| !l.trim().is_empty()).count();
                ctx.log(format!("cluster reachable: {nodes} node(s)"));
                ctx.progress(100);
                StepOutcome::Completed
            }
            Ok(o) => StepOutcome::Failed {
                error: format!("cluster not reachable (exit {}): {}", o.status, o.stderr.trim()),
                next_steps: vec![
                    "Confirm the kubeconfig is valid and the API server is reachable.".into(),
                    "If the token expired, refresh it (e.g. `oc login`) and re-provide the kubeconfig.".into(),
                ],
            },
            Err(e) => StepOutcome::Failed {
                error: format!("could not run oc: {e}"),
                next_steps: vec!["Install `oc` and ensure it is on your PATH, then retry.".into()],
            },
        }
    }
}

/// The existing-cluster module.
pub struct ExistingClusterModule;

impl Module for ExistingClusterModule {
    fn id(&self) -> &str {
        "mod-existing"
    }
    fn title(&self) -> &str {
        "Use existing cluster"
    }
    fn steps(&self) -> Vec<Box<dyn Step>> {
        vec![Box::new(ProvideKubeconfig), Box::new(VerifyAccess)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use sw_core::{EventBus, MockCommandRunner, MockResponse};

    fn temp_artifacts() -> std::path::PathBuf {
        static N: AtomicU64 = AtomicU64::new(1);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!("wxd-existing-{}", std::process::id()))
            .join(format!("a{n}"))
    }

    fn ctx(
        runner: MockCommandRunner,
        inputs: &[(&str, &str)],
        secrets: &[(&str, &str)],
        artifacts: std::path::PathBuf,
    ) -> StepContext {
        let inputs: BTreeMap<String, String> =
            inputs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let secrets: BTreeMap<String, String> =
            secrets.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        std::fs::create_dir_all(&artifacts).unwrap();
        StepContext::with_artifacts(
            "run".into(),
            "mod-existing/x".into(),
            Arc::new(runner),
            EventBus::new(),
            inputs,
            secrets,
            artifacts,
        )
    }

    #[test]
    fn module_has_two_steps() {
        let ids: Vec<_> = ExistingClusterModule.steps().iter().map(|s| s.id().to_string()).collect();
        assert_eq!(ids, vec!["provide-kubeconfig", "verify-access"]);
    }

    #[tokio::test]
    async fn asks_for_kubeconfig_when_none_given() {
        let art = temp_artifacts();
        let c = ctx(MockCommandRunner::new(vec![]), &[], &[], art.clone());
        match ProvideKubeconfig.run(&c).await {
            StepOutcome::NeedsInput { fields, .. } => {
                assert_eq!(fields[0].key, "kubeconfig_source_path");
                assert_eq!(fields[1].key, "kubeconfig");
                assert!(fields[1].secret);
            }
            o => panic!("expected NeedsInput, got {o:?}"),
        }
        std::fs::remove_dir_all(art).ok();
    }

    #[tokio::test]
    async fn adopts_kubeconfig_from_path() {
        let art = temp_artifacts();
        // Write a source kubeconfig to copy from.
        let src = art.join("source-kubeconfig");
        std::fs::create_dir_all(&art).unwrap();
        std::fs::write(&src, "apiVersion: v1\nkind: Config\n").unwrap();
        let c = ctx(
            MockCommandRunner::new(vec![]),
            &[("kubeconfig_source_path", src.to_str().unwrap())],
            &[],
            art.clone(),
        );
        assert_eq!(ProvideKubeconfig.run(&c).await, StepOutcome::Completed);
        assert!(c.kubeconfig_path().exists());
        std::fs::remove_dir_all(art).ok();
    }

    #[tokio::test]
    async fn adopts_pasted_kubeconfig_contents() {
        let art = temp_artifacts();
        let c = ctx(
            MockCommandRunner::new(vec![]),
            &[],
            &[("kubeconfig", "apiVersion: v1\nkind: Config\n")],
            art.clone(),
        );
        assert_eq!(ProvideKubeconfig.run(&c).await, StepOutcome::Completed);
        let written = std::fs::read_to_string(c.kubeconfig_path()).unwrap();
        assert!(written.contains("kind: Config"));
        std::fs::remove_dir_all(art).ok();
    }

    #[tokio::test]
    async fn verify_access_reports_node_count() {
        let art = temp_artifacts();
        let runner = MockCommandRunner::new(vec![MockResponse::ok(
            "get nodes",
            "node-1 Ready\nnode-2 Ready\n",
        )]);
        let c = ctx(runner, &[], &[], art.clone());
        assert_eq!(VerifyAccess.run(&c).await, StepOutcome::Completed);
        std::fs::remove_dir_all(art).ok();
    }

    #[tokio::test]
    async fn verify_access_fails_when_unreachable() {
        let art = temp_artifacts();
        let runner = MockCommandRunner::new(vec![MockResponse::fail(
            "get nodes",
            1,
            "Unable to connect to the server",
        )]);
        let c = ctx(runner, &[], &[], art.clone());
        match VerifyAccess.run(&c).await {
            StepOutcome::Failed { next_steps, .. } => assert!(!next_steps.is_empty()),
            o => panic!("expected Failed, got {o:?}"),
        }
        std::fs::remove_dir_all(art).ok();
    }
}
