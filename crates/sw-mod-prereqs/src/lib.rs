//! `sw-mod-prereqs` — auto-installs the external CLIs the installer drives.
//!
//! For each required tool it first checks whether the tool already runs (via the
//! `CommandRunner` seam, so `~/.wxd/bin` is on PATH); if not, it downloads and
//! installs it into `~/.wxd/bin` and verifies it. Idempotent: a present tool is a
//! no-op. Everything runs through `ctx.runner()` (shelling `sh -c "<script>"`),
//! so it stays hermetically testable with a mock runner.

use async_trait::async_trait;
use sw_core::{Module, Step, StepContext, StepOutcome};

/// Per-platform download tokens derived from the build target.
struct Platform {
    /// helm/get.helm.sh os token: `darwin` | `linux`.
    dl_os: &'static str,
    /// helm arch token: `amd64` | `arm64`.
    helm_arch: &'static str,
    /// OpenShift mirror arch directory: `x86_64` | `arm64`.
    mirror_arch: &'static str,
    /// OpenShift client tarball name.
    oc_file: &'static str,
    /// OpenShift installer tarball name.
    ois_file: &'static str,
    /// cpd-cli asset grep token: `darwin` | `linux`.
    cpd_token: &'static str,
}

fn platform() -> Platform {
    let arm = std::env::consts::ARCH == "aarch64";
    match std::env::consts::OS {
        "macos" => Platform {
            dl_os: "darwin",
            helm_arch: if arm { "arm64" } else { "amd64" },
            mirror_arch: if arm { "arm64" } else { "x86_64" },
            oc_file: if arm { "openshift-client-mac-arm64.tar.gz" } else { "openshift-client-mac.tar.gz" },
            ois_file: if arm { "openshift-install-mac-arm64.tar.gz" } else { "openshift-install-mac.tar.gz" },
            cpd_token: "darwin",
        },
        _ => Platform {
            dl_os: "linux",
            helm_arch: if arm { "arm64" } else { "amd64" },
            mirror_arch: if arm { "arm64" } else { "x86_64" },
            oc_file: if arm { "openshift-client-linux-arm64.tar.gz" } else { "openshift-client-linux.tar.gz" },
            ois_file: if arm { "openshift-install-linux-arm64.tar.gz" } else { "openshift-install-linux.tar.gz" },
            cpd_token: "linux",
        },
    }
}

const MIRROR: &str = "https://mirror.openshift.com/pub/openshift-v4";

fn helm_script(p: &Platform) -> String {
    format!(
        "set -e; BIN=\"$HOME/.wxd/bin\"; mkdir -p \"$BIN\"; \
         V=$(curl -fsSL https://api.github.com/repos/helm/helm/releases/latest | grep '\"tag_name\"' | head -1 | sed -E 's/.*\"v?([0-9.]+)\".*/\\1/'); \
         curl -fsSL \"https://get.helm.sh/helm-v${{V}}-{os}-{arch}.tar.gz\" -o /tmp/wxd-helm.tgz; \
         tar xzf /tmp/wxd-helm.tgz -C /tmp; mv /tmp/{os}-{arch}/helm \"$BIN/helm\"; chmod +x \"$BIN/helm\"",
        os = p.dl_os,
        arch = p.helm_arch
    )
}

fn oc_script(p: &Platform) -> String {
    format!(
        "set -e; BIN=\"$HOME/.wxd/bin\"; mkdir -p \"$BIN\"; \
         curl -fsSL \"{mirror}/{arch}/clients/ocp/stable/{file}\" -o /tmp/wxd-oc.tgz; \
         tar xzf /tmp/wxd-oc.tgz -C \"$BIN\" oc kubectl; chmod +x \"$BIN/oc\"",
        mirror = MIRROR,
        arch = p.mirror_arch,
        file = p.oc_file
    )
}

fn ois_script(p: &Platform) -> String {
    format!(
        "set -e; BIN=\"$HOME/.wxd/bin\"; mkdir -p \"$BIN\"; \
         curl -fsSL \"{mirror}/{arch}/clients/ocp/stable/{file}\" -o /tmp/wxd-ois.tgz; \
         tar xzf /tmp/wxd-ois.tgz -C \"$BIN\" openshift-install; chmod +x \"$BIN/openshift-install\"",
        mirror = MIRROR,
        arch = p.mirror_arch,
        file = p.ois_file
    )
}

fn cpd_script(p: &Platform) -> String {
    format!(
        "set -e; BIN=\"$HOME/.wxd/bin\"; mkdir -p \"$BIN\"; \
         U=$(curl -fsSL https://api.github.com/repos/IBM/cpd-cli/releases/latest | grep -o '\"browser_download_url\": *\"[^\"]*\"' | grep -i {tok} | grep -i EE | head -1 | sed -E 's/.*\"(http[^\"]*)\".*/\\1/'); \
         test -n \"$U\"; curl -fsSL \"$U\" -o /tmp/wxd-cpd.tgz; \
         rm -rf /tmp/wxd-cpdx; mkdir -p /tmp/wxd-cpdx; tar xzf /tmp/wxd-cpd.tgz -C /tmp/wxd-cpdx; \
         D=$(find /tmp/wxd-cpdx -maxdepth 1 -type d -name 'cpd-cli-*' | head -1); cp -R \"$D\"/* \"$BIN\"/; chmod +x \"$BIN/cpd-cli\"",
        tok = p.cpd_token
    )
}

/// One installable tool.
struct ToolStep {
    id: &'static str,
    title: &'static str,
    /// Presence probe: program + args (exit 0 means "already installed").
    probe: (&'static str, Vec<String>),
    /// `sh -c` install script, or `None` for check-only tools (e.g. `aws`).
    install: Option<String>,
    /// Manual guidance if it can't be auto-installed.
    manual: Vec<String>,
}

impl ToolStep {
    async fn present(&self, ctx: &StepContext) -> bool {
        matches!(ctx.runner().run(self.probe.0, &self.probe.1).await, Ok(o) if o.success())
    }
}

#[async_trait]
impl Step for ToolStep {
    fn id(&self) -> &str {
        self.id
    }
    fn title(&self) -> &str {
        self.title
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        if self.present(ctx).await {
            ctx.log(format!("{} already installed", self.probe.0));
            ctx.progress(100);
            return StepOutcome::Completed;
        }
        let Some(script) = &self.install else {
            return StepOutcome::Failed {
                error: format!("{} is not installed and cannot be auto-installed", self.probe.0),
                next_steps: self.manual.clone(),
            };
        };
        ctx.log(format!("installing {} into ~/.wxd/bin …", self.probe.0));
        match ctx.runner().run("sh", &["-c".to_string(), script.clone()]).await {
            Ok(o) if o.success() => {
                if self.present(ctx).await {
                    ctx.log(format!("{} installed", self.probe.0));
                    ctx.progress(100);
                    StepOutcome::Completed
                } else {
                    StepOutcome::Failed {
                        error: format!("{} installed but did not verify", self.probe.0),
                        next_steps: self.manual.clone(),
                    }
                }
            }
            Ok(o) => StepOutcome::Failed {
                error: format!("installing {} failed (exit {}): {}", self.probe.0, o.status, o.stderr.trim()),
                next_steps: self.manual.clone(),
            },
            Err(e) => StepOutcome::Failed {
                error: format!("could not run installer for {}: {e}", self.probe.0),
                next_steps: self.manual.clone(),
            },
        }
    }
}

/// The prerequisites module: installs every external CLI the run needs.
pub struct PrereqsModule;

impl Module for PrereqsModule {
    fn id(&self) -> &str {
        "mod-prereqs"
    }
    fn title(&self) -> &str {
        "Install prerequisites"
    }
    fn steps(&self) -> Vec<Box<dyn Step>> {
        let p = platform();
        vec![
            Box::new(ToolStep {
                id: "oc",
                title: "OpenShift CLI (oc)",
                probe: ("oc", vec!["version".into(), "--client".into()]),
                install: Some(oc_script(&p)),
                manual: vec!["Download `oc` from console.redhat.com/openshift/downloads.".into()],
            }),
            Box::new(ToolStep {
                id: "helm",
                title: "Helm",
                probe: ("helm", vec!["version".into(), "--short".into()]),
                install: Some(helm_script(&p)),
                manual: vec!["Install Helm 3.18+ from helm.sh.".into()],
            }),
            Box::new(ToolStep {
                id: "openshift-install",
                title: "OpenShift installer",
                probe: ("openshift-install", vec!["version".into()]),
                install: Some(ois_script(&p)),
                manual: vec!["Download `openshift-install` from console.redhat.com/openshift/install/aws/installer-provisioned.".into()],
            }),
            Box::new(ToolStep {
                id: "cpd-cli",
                title: "Cloud Pak for Data CLI (cpd-cli)",
                probe: ("cpd-cli", vec!["version".into()]),
                install: Some(cpd_script(&p)),
                manual: vec!["Download cpd-cli from github.com/IBM/cpd-cli/releases (match the 5.4.x release).".into()],
            }),
            Box::new(ToolStep {
                id: "aws",
                title: "AWS CLI",
                probe: ("aws", vec!["--version".into()]),
                install: None, // macOS/Linux installs need sudo; check-only with guidance.
                manual: vec!["Install the AWS CLI v2 from aws.amazon.com/cli (needs admin rights).".into()],
            }),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use sw_core::{EventBus, MockCommandRunner, MockResponse};

    fn ctx(runner: MockCommandRunner) -> StepContext {
        StepContext::with_artifacts(
            "r".into(),
            "mod-prereqs/x".into(),
            Arc::new(runner),
            EventBus::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            std::env::temp_dir(),
        )
    }

    fn helm_step() -> ToolStep {
        let p = platform();
        ToolStep {
            id: "helm",
            title: "Helm",
            probe: ("helm", vec!["version".into(), "--short".into()]),
            install: Some(helm_script(&p)),
            manual: vec!["manual".into()],
        }
    }

    #[test]
    fn module_lists_all_tools() {
        let ids: Vec<_> = PrereqsModule.steps().iter().map(|s| s.id().to_string()).collect();
        assert_eq!(ids, vec!["oc", "helm", "openshift-install", "cpd-cli", "aws"]);
    }

    #[test]
    fn scripts_are_platform_specialized() {
        let p = platform();
        let oc = oc_script(&p);
        assert!(oc.contains(p.oc_file));
        assert!(helm_script(&p).contains(p.helm_arch));
        assert!(cpd_script(&p).contains(p.cpd_token));
    }

    #[tokio::test]
    async fn present_tool_is_a_noop() {
        // Default mock returns success for everything → probe succeeds.
        let c = ctx(MockCommandRunner::new(vec![]));
        assert_eq!(helm_step().run(&c).await, StepOutcome::Completed);
    }

    #[tokio::test]
    async fn missing_tool_is_installed_then_verified() {
        // First probe fails (absent); install (sh -c) succeeds; re-probe defaults to success.
        let runner = MockCommandRunner::new(vec![
            MockResponse::fail("helm version", 1, "not found"),
            MockResponse::ok("sh -c", "installed"),
        ]);
        let c = ctx(runner);
        assert_eq!(helm_step().run(&c).await, StepOutcome::Completed);
    }

    #[tokio::test]
    async fn install_failure_reports_manual_steps() {
        let runner = MockCommandRunner::new(vec![
            MockResponse::fail("helm version", 1, "not found"),
            MockResponse::fail("sh -c", 1, "network error"),
        ]);
        let c = ctx(runner);
        match helm_step().run(&c).await {
            StepOutcome::Failed { next_steps, .. } => assert!(!next_steps.is_empty()),
            o => panic!("expected Failed, got {o:?}"),
        }
    }

    #[tokio::test]
    async fn check_only_tool_fails_with_guidance_when_absent() {
        let p = platform();
        let aws = ToolStep {
            id: "aws",
            title: "AWS CLI",
            probe: ("aws", vec!["--version".into()]),
            install: None,
            manual: vec!["install aws".into()],
        };
        let _ = &p;
        let runner = MockCommandRunner::new(vec![MockResponse::fail("aws --version", 127, "missing")]);
        match aws.run(&ctx(runner)).await {
            StepOutcome::Failed { next_steps, .. } => assert_eq!(next_steps, vec!["install aws".to_string()]),
            o => panic!("expected Failed, got {o:?}"),
        }
    }
}
