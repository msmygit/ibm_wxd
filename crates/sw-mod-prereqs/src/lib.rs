//! `sw-mod-prereqs` — auto-installs the external CLIs the installer drives.
//!
//! For each required tool it first checks whether the tool already runs (via the
//! `CommandRunner` seam, so `~/.wxd/bin` is on PATH); if not, it downloads and
//! installs it into `~/.wxd/bin` and verifies it. Idempotent: a present tool is a
//! no-op. Everything runs through `ctx.runner()` (shelling `sh -c "<script>"`),
//! so it stays hermetically testable with a mock runner.

use async_trait::async_trait;
use serde::Serialize;
use sw_core::{CommandRunner, Module, Step, StepContext, StepOutcome};

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
    // The cpd-cli asset token is arch-aware (macOS Apple Silicon → `arm64`, not
    // the Intel `darwin` build). Fall back to `linux` for the rare unsupported
    // arch so the fresh-install grep at least targets something.
    let cpd_token =
        cpd_asset_token(std::env::consts::OS, std::env::consts::ARCH).unwrap_or("linux");
    match std::env::consts::OS {
        "macos" => Platform {
            dl_os: "darwin",
            helm_arch: if arm { "arm64" } else { "amd64" },
            mirror_arch: if arm { "arm64" } else { "x86_64" },
            oc_file: if arm {
                "openshift-client-mac-arm64.tar.gz"
            } else {
                "openshift-client-mac.tar.gz"
            },
            ois_file: if arm {
                "openshift-install-mac-arm64.tar.gz"
            } else {
                "openshift-install-mac.tar.gz"
            },
            cpd_token,
        },
        _ => Platform {
            dl_os: "linux",
            helm_arch: if arm { "arm64" } else { "amd64" },
            mirror_arch: if arm { "arm64" } else { "x86_64" },
            oc_file: if arm {
                "openshift-client-linux-arm64.tar.gz"
            } else {
                "openshift-client-linux.tar.gz"
            },
            ois_file: if arm {
                "openshift-install-linux-arm64.tar.gz"
            } else {
                "openshift-install-linux.tar.gz"
            },
            cpd_token,
        },
    }
}

const MIRROR: &str = "https://mirror.openshift.com/pub/openshift-v4";

fn helm_script(p: &Platform) -> String {
    // IBM Software Hub supports Helm 3.19/3.20 (Helm is only used to debug
    // cpd-cli commands). Pin to the latest supported 3.20/3.19 patch — NOT the
    // absolute latest, which is now Helm 4.x.
    format!(
        "set -e; BIN=\"$HOME/.wxd/bin\"; mkdir -p \"$BIN\"; \
         V=$(curl -fsSL 'https://api.github.com/repos/helm/helm/releases?per_page=100' | grep '\"tag_name\"' | sed -E 's/.*\"v?([0-9.]+)\".*/\\1/' | grep -E '^3\\.(20|19)\\.' | sort -V | tail -1); \
         test -n \"$V\"; \
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

// ---- cpd-cli ↔ Software Hub version compatibility -------------------------
//
// The chosen Software Hub release (VERSION input) dictates which cpd-cli is
// required — cpd-cli's own `manage`/olm-utils contract is version-specific, so an
// old cpd-cli against a new release fails cryptically later. Rather than trust a
// hardcoded map, we read the compatibility straight from the IBM/cpd-cli GitHub
// releases: each release NAME encodes it (e.g. "v14.4.0 … CPD 5.4.0",
// "v14.3.1.7 … CPD 5.3.1 - Patch 7"). If the installed cpd-cli doesn't target the
// chosen release, we download the matching one for THIS machine's OS+arch.

const CPD_RELEASES_API: &str = "https://api.github.com/repos/IBM/cpd-cli/releases?per_page=100";
const CPD_RELEASES_URL: &str = "https://github.com/IBM/cpd-cli/releases";

/// The cpd-cli release-asset token for an OS/arch, or None if IBM publishes no
/// build for it. IBM's asset naming (per the release assets):
///   - macOS Intel (x86_64)        → `darwin`
///   - macOS Apple Silicon (arm64) → `arm64`   ← this is a macOS build, not Linux
///   - Linux x86_64                → `linux`
///   - Linux ppc64le / s390x       → `ppc64le` / `s390x` (IBM Power / Z)
/// There is no Linux-arm64 cpd-cli build.
fn cpd_asset_token(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("arm64"),
        ("macos", _) => Some("darwin"),
        ("linux", "x86_64") => Some("linux"),
        ("linux", "powerpc64" | "powerpc64le") => Some("ppc64le"),
        ("linux", "s390x") => Some("s390x"),
        _ => None,
    }
}

/// The value after a labeled line in `cpd-cli version` output (tab-indented),
/// e.g. `parse_version_field(out, "SWH Release Version:")`.
fn parse_version_field(version_output: &str, label: &str) -> Option<String> {
    version_output.lines().find_map(|l| {
        let l = l.trim();
        l.strip_prefix(label).map(|v| v.trim().to_string())
    })
}

/// Extract `(cpd_version, patch)` from a cpd-cli release name like
/// "v14.4.0 Cloud Pak for Data command line interface CPD 5.4.0" or
/// "… CPD 5.3.1 - Patch 7". Returns None if it doesn't carry a `CPD X.Y…` token.
fn parse_release_cpd(name: &str) -> Option<(String, Option<u32>)> {
    let rest = &name[name.find("CPD ")? + 4..];
    let version = rest.split_whitespace().next()?.trim().to_string();
    if !version.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    let patch = name
        .find("Patch ")
        .and_then(|p| name[p + 6..].split_whitespace().next())
        .and_then(|n| n.trim().parse::<u32>().ok());
    Some((version, patch))
}

/// The chosen cpd-cli release: its tag (the cpd-cli version) + the download URL
/// for this machine's asset.
struct CpdTarget {
    tag: String,
    url: String,
}

/// From the GitHub `/releases` JSON array, pick the cpd-cli release whose CPD
/// version == `version` and whose patch matches `patch_id` (an exact patch when
/// numeric, else the highest available), and return its tag + the `token`+EE
/// asset download URL.
fn select_cpd_target(
    releases_json: &str,
    version: &str,
    patch_id: &str,
    token: &str,
) -> Result<CpdTarget, String> {
    let rels: serde_json::Value = serde_json::from_str(releases_json)
        .map_err(|e| format!("could not parse cpd-cli releases: {e}"))?;
    let arr = rels
        .as_array()
        .ok_or("unexpected cpd-cli releases payload")?;

    let want_patch: Option<u32> = match patch_id.trim() {
        "" | "latest" | "0" => None,
        p => p.parse::<u32>().ok(),
    };

    let candidates: Vec<(Option<u32>, &serde_json::Value)> = arr
        .iter()
        .filter_map(|r| {
            let name = r.get("name").and_then(|n| n.as_str()).unwrap_or("");
            parse_release_cpd(name)
                .and_then(|(cpd_ver, patch)| (cpd_ver == version).then_some((patch, r)))
        })
        .collect();

    if candidates.is_empty() {
        return Err(format!(
            "no cpd-cli release found for Software Hub {version}"
        ));
    }

    let chosen = match want_patch {
        Some(p) => candidates
            .iter()
            .find(|(patch, _)| *patch == Some(p))
            .map(|(_, r)| *r)
            .ok_or_else(|| format!("no cpd-cli release for Software Hub {version} patch {p}"))?,
        None => candidates
            .iter()
            .max_by_key(|(patch, _)| patch.unwrap_or(0))
            .map(|(_, r)| *r)
            .expect("candidates is non-empty"),
    };

    let tag = chosen
        .get("tag_name")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let needle = format!("-{token}-EE-");
    let url = chosen
        .get("assets")
        .and_then(|a| a.as_array())
        .and_then(|assets| {
            assets.iter().find_map(|a| {
                let n = a.get("name").and_then(|n| n.as_str()).unwrap_or("");
                (n.contains(&needle) && n.ends_with(".tgz"))
                    .then(|| {
                        a.get("browser_download_url")
                            .and_then(|u| u.as_str())
                            .map(String::from)
                    })
                    .flatten()
            })
        })
        .ok_or_else(|| format!("release {tag} has no cpd-cli '{token}' EE asset"))?;

    Ok(CpdTarget { tag, url })
}

/// Ensures the installed cpd-cli targets the chosen Software Hub release; if not,
/// downloads the matching one (for this OS+arch) into `~/.wxd/bin`. Idempotent.
struct CpdCliCompatStep;

#[async_trait]
impl Step for CpdCliCompatStep {
    fn id(&self) -> &str {
        "cpd-cli-compat"
    }
    fn title(&self) -> &str {
        "Match cpd-cli to the Software Hub release"
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        let target_version = ctx.input("VERSION").unwrap_or("5.4.0").to_string();
        let patch_id = ctx.input("PATCH_ID").unwrap_or("latest").to_string();
        let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
        let token = match cpd_asset_token(os, arch) {
            Some(t) => t,
            None => {
                return StepOutcome::Failed {
                    error: format!("IBM does not publish a cpd-cli build for {os}/{arch}"),
                    next_steps: vec![format!(
                        "Run the installer on a supported workstation (macOS Intel/Apple Silicon, or Linux x86_64/ppc64le/s390x). Releases: {CPD_RELEASES_URL}"
                    )],
                }
            }
        };

        // What's installed now? (Empty if cpd-cli is missing/errored.)
        let installed = match ctx.runner().run("cpd-cli", &["version".into()]).await {
            Ok(o) if o.success() => o.stdout,
            _ => String::new(),
        };
        let installed_swh = parse_version_field(&installed, "SWH Release Version:");
        let installed_ver = parse_version_field(&installed, "Version:");

        if installed_swh.as_deref() == Some(target_version.as_str()) {
            ctx.log(format!(
                "cpd-cli {} already targets Software Hub {target_version}",
                installed_ver.as_deref().unwrap_or("(installed)")
            ));
            ctx.progress(100);
            return StepOutcome::Completed;
        }

        ctx.log(format!(
            "installed cpd-cli ({}) does not target Software Hub {target_version}; finding the matching release for {os}/{arch}",
            installed_swh.as_deref().or(installed_ver.as_deref()).unwrap_or("none/unknown"),
        ));

        // Read the cpd-cli ↔ Software Hub compatibility from the GitHub releases.
        let json = match ctx.runner().run("curl", &["-fsSL".into(), CPD_RELEASES_API.into()]).await {
            Ok(o) if o.success() && !o.stdout.trim().is_empty() => o.stdout,
            _ => {
                return StepOutcome::Failed {
                    error: format!("could not reach GitHub to find the cpd-cli matching Software Hub {target_version}"),
                    next_steps: vec![
                        "Check network access to api.github.com, then retry.".into(),
                        format!("Or install it manually: download the `cpd-cli-{token}-EE` asset for Software Hub {target_version} from {CPD_RELEASES_URL} into ~/.wxd/bin, then retry."),
                    ],
                }
            }
        };

        let target = match select_cpd_target(&json, &target_version, &patch_id, token) {
            Ok(t) => t,
            Err(e) => {
                return StepOutcome::Failed {
                    error: e,
                    next_steps: vec![format!("Confirm the Software Hub version/patch is valid — releases: {CPD_RELEASES_URL}")],
                }
            }
        };

        ctx.log(format!(
            "updating cpd-cli → {} for Software Hub {target_version} ({os}/{arch}, asset `cpd-cli-{token}-EE`)",
            target.tag
        ));

        // Download the matching asset + install into ~/.wxd/bin.
        let script = format!(
            "set -e; BIN=\"$HOME/.wxd/bin\"; mkdir -p \"$BIN\"; \
             curl -fsSL \"{url}\" -o /tmp/wxd-cpd.tgz; \
             rm -rf /tmp/wxd-cpdx; mkdir -p /tmp/wxd-cpdx; tar xzf /tmp/wxd-cpd.tgz -C /tmp/wxd-cpdx; \
             D=$(find /tmp/wxd-cpdx -maxdepth 1 -type d -name 'cpd-cli-*' | head -1); cp -R \"$D\"/* \"$BIN\"/; chmod +x \"$BIN/cpd-cli\"",
            url = target.url
        );
        match ctx.runner().run("sh", &["-c".into(), script]).await {
            Ok(o) if o.success() => {}
            Ok(o) => {
                return StepOutcome::Failed {
                    error: format!(
                        "installing cpd-cli {} failed (exit {}): {}",
                        target.tag,
                        o.status,
                        o.stderr.trim()
                    ),
                    next_steps: vec![format!(
                        "Download {} into ~/.wxd/bin manually, then retry.",
                        target.url
                    )],
                }
            }
            Err(e) => {
                return StepOutcome::Failed {
                    error: format!("could not run the cpd-cli installer: {e}"),
                    next_steps: vec!["Ensure `curl` and `tar` are available, then retry.".into()],
                }
            }
        }

        // Verify the new cpd-cli targets the chosen release.
        let after = ctx
            .runner()
            .run("cpd-cli", &["version".into()])
            .await
            .ok()
            .filter(|o| o.success())
            .map(|o| o.stdout)
            .unwrap_or_default();
        match parse_version_field(&after, "SWH Release Version:") {
            Some(swh) if swh == target_version => {
                ctx.log(format!("cpd-cli now targets Software Hub {target_version} ({})", target.tag));
                ctx.progress(100);
                StepOutcome::Completed
            }
            other => StepOutcome::Failed {
                error: format!(
                    "installed cpd-cli {} but it reports Software Hub '{}' (expected {target_version})",
                    target.tag,
                    other.unwrap_or_default()
                ),
                next_steps: vec![format!("Verify the cpd-cli install, or fetch {} manually from {CPD_RELEASES_URL}.", target.tag)],
            },
        }
    }
}

/// One installable tool: how to detect it and (optionally) how to install it.
#[derive(Clone)]
struct ToolSpec {
    id: &'static str,
    title: &'static str,
    /// Presence probe: program + args (exit 0 means "already installed").
    probe: (&'static str, Vec<String>),
    /// `sh -c` install script, or `None` for check-only tools (e.g. `aws`).
    install: Option<String>,
    /// Manual guidance if it can't be auto-installed.
    manual: Vec<String>,
}

/// The prerequisite tools for the current platform, in run order.
fn specs() -> Vec<ToolSpec> {
    let p = platform();
    vec![
        ToolSpec {
            id: "oc",
            title: "OpenShift CLI (oc)",
            probe: ("oc", vec!["version".into(), "--client".into()]),
            install: Some(oc_script(&p)),
            manual: vec!["Download `oc` from console.redhat.com/openshift/downloads.".into()],
        },
        ToolSpec {
            id: "helm",
            title: "Helm",
            probe: ("helm", vec!["version".into(), "--short".into()]),
            install: Some(helm_script(&p)),
            manual: vec!["Install Helm 3.18+ from helm.sh.".into()],
        },
        ToolSpec {
            id: "openshift-install",
            title: "OpenShift installer",
            probe: ("openshift-install", vec!["version".into()]),
            install: Some(ois_script(&p)),
            manual: vec!["Download `openshift-install` from console.redhat.com/openshift/install/aws/installer-provisioned.".into()],
        },
        ToolSpec {
            id: "cpd-cli",
            title: "Cloud Pak for Data CLI (cpd-cli)",
            probe: ("cpd-cli", vec!["version".into()]),
            install: Some(cpd_script(&p)),
            manual: vec!["Download cpd-cli from github.com/IBM/cpd-cli/releases (match the 5.4.x release).".into()],
        },
        ToolSpec {
            id: "aws",
            title: "AWS CLI",
            probe: ("aws", vec!["--version".into()]),
            install: None, // installing AWS CLI needs admin rights; check-only.
            manual: vec!["Install the AWS CLI v2 from aws.amazon.com/cli (needs admin rights).".into()],
        },
        ToolSpec {
            id: "container-runtime",
            title: "Container runtime (Docker/Podman)",
            // `docker info` (or `podman info`) only succeeds when the daemon is
            // actually RUNNING — `--version` would pass even with the daemon down.
            // `cpd-cli manage` runs the olm-utils image locally, so this must be up
            // before the Software Hub phase.
            probe: (
                "sh",
                vec![
                    "-c".into(),
                    "docker info >/dev/null 2>&1 || podman info >/dev/null 2>&1".into(),
                ],
            ),
            install: None, // a daemon can't be auto-installed/started; guide the user.
            manual: vec![
                "cpd-cli needs a local container engine to run olm-utils during the Software Hub install.".into(),
                "Install Docker Desktop (or Colima/Podman) and START it, then Re-check. Verify with `docker info`.".into(),
            ],
        },
    ]
}

/// Reported status of one prerequisite tool (for the UI's prerequisites panel).
#[derive(Debug, Clone, Serialize)]
pub struct ToolStatus {
    pub id: String,
    pub title: String,
    pub present: bool,
    /// Whether the installer can auto-install it (false for check-only tools).
    pub installable: bool,
    /// Version string / short detail when present.
    pub detail: String,
}

async fn probe(runner: &dyn CommandRunner, spec: &ToolSpec) -> (bool, String) {
    match runner.run(spec.probe.0, &spec.probe.1).await {
        Ok(o) if o.success() => {
            let detail = o
                .stdout
                .lines()
                .chain(o.stderr.lines())
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
                .to_string();
            (true, detail)
        }
        _ => (false, String::new()),
    }
}

/// Check every prerequisite tool's presence (no installation).
pub async fn check_all(runner: &dyn CommandRunner) -> Vec<ToolStatus> {
    let mut out = Vec::new();
    for spec in specs() {
        let (present, detail) = probe(runner, &spec).await;
        out.push(ToolStatus {
            id: spec.id.into(),
            title: spec.title.into(),
            present,
            installable: spec.install.is_some(),
            detail,
        });
    }
    out
}

/// Install one tool by id (installs unconditionally, then verifies). The caller
/// decides whether it was missing first.
pub async fn install_one(runner: &dyn CommandRunner, id: &str) -> Result<(), String> {
    let Some(spec) = specs().into_iter().find(|s| s.id == id) else {
        return Err(format!("unknown tool: {id}"));
    };
    let Some(script) = spec.install.clone() else {
        return Err(format!(
            "{} cannot be auto-installed. {}",
            spec.id,
            spec.manual.join(" ")
        ));
    };
    match runner.run("sh", &["-c".to_string(), script]).await {
        Ok(o) if o.success() => {
            if probe(runner, &spec).await.0 {
                Ok(())
            } else {
                Err(format!("{} installed but did not verify", spec.id))
            }
        }
        Ok(o) => Err(format!(
            "installing {} failed (exit {}): {}",
            spec.id,
            o.status,
            o.stderr.trim()
        )),
        Err(e) => Err(format!("could not run installer for {}: {e}", spec.id)),
    }
}

/// Install every missing, installable tool; return the resulting status list.
pub async fn install_missing(runner: &dyn CommandRunner) -> Vec<ToolStatus> {
    for spec in specs() {
        if spec.install.is_some() && !probe(runner, &spec).await.0 {
            let _ = install_one(runner, spec.id).await;
        }
    }
    check_all(runner).await
}

/// A prerequisite step wrapping one spec (used inside an install run).
struct ToolStep {
    spec: ToolSpec,
}

#[async_trait]
impl Step for ToolStep {
    fn id(&self) -> &str {
        self.spec.id
    }
    fn title(&self) -> &str {
        self.spec.title
    }
    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        if probe(ctx.runner(), &self.spec).await.0 {
            ctx.log(format!("{} already installed", self.spec.id));
            ctx.progress(100);
            return StepOutcome::Completed;
        }
        if self.spec.install.is_none() {
            return StepOutcome::Failed {
                error: format!(
                    "{} is not installed and cannot be auto-installed",
                    self.spec.id
                ),
                next_steps: self.spec.manual.clone(),
            };
        }
        ctx.log(format!("installing {} into ~/.wxd/bin …", self.spec.id));
        match install_one(ctx.runner(), self.spec.id).await {
            Ok(()) => {
                ctx.log(format!("{} installed", self.spec.id));
                ctx.progress(100);
                StepOutcome::Completed
            }
            Err(e) => StepOutcome::Failed {
                error: e,
                next_steps: self.spec.manual.clone(),
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
        let mut steps: Vec<Box<dyn Step>> = specs()
            .into_iter()
            .map(|spec| Box::new(ToolStep { spec }) as Box<dyn Step>)
            .collect();
        // After the tool steps (cpd-cli is present by now), ensure it matches the
        // chosen Software Hub release — auto-updating it for this OS/arch if not.
        steps.push(Box::new(CpdCliCompatStep));
        steps
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
            spec: ToolSpec {
                id: "helm",
                title: "Helm",
                probe: ("helm", vec!["version".into(), "--short".into()]),
                install: Some(helm_script(&p)),
                manual: vec!["manual".into()],
            },
        }
    }

    #[test]
    fn module_lists_all_tools_then_cpd_compat() {
        let ids: Vec<_> = PrereqsModule
            .steps()
            .iter()
            .map(|s| s.id().to_string())
            .collect();
        assert_eq!(
            ids,
            vec![
                "oc",
                "helm",
                "openshift-install",
                "cpd-cli",
                "aws",
                "container-runtime",
                "cpd-cli-compat"
            ]
        );
    }

    // A releases payload shaped like the real GitHub API.
    const RELEASES: &str = r#"[
        {"tag_name":"v14.4.0","name":"v14.4.0 Cloud Pak for Data command line interface CPD 5.4.0","assets":[
            {"name":"cpd-cli-darwin-EE-14.4.0.tgz","browser_download_url":"https://ex/darwin-1440"},
            {"name":"cpd-cli-arm64-EE-14.4.0.tgz","browser_download_url":"https://ex/arm64-1440"},
            {"name":"cpd-cli-linux-EE-14.4.0.tgz","browser_download_url":"https://ex/linux-1440"},
            {"name":"cpd-cli-ppc64le-EE-14.4.0.tgz","browser_download_url":"https://ex/ppc-1440"},
            {"name":"cpd-cli-s390x-EE-14.4.0.tgz","browser_download_url":"https://ex/s390-1440"}
        ]},
        {"tag_name":"v14.3.1.7","name":"v14.3.1.7 Cloud Pak for Data command line interface CPD 5.3.1 - Patch 7","assets":[
            {"name":"cpd-cli-darwin-EE-14.3.1.tgz","browser_download_url":"https://ex/darwin-1317"}
        ]},
        {"tag_name":"v14.3.1","name":"v14.3.1 Cloud Pak for Data command line interface CPD 5.3.1","assets":[
            {"name":"cpd-cli-darwin-EE-14.3.1.tgz","browser_download_url":"https://ex/darwin-1310"}
        ]}
    ]"#;

    #[test]
    fn asset_token_maps_os_and_arch() {
        // macOS: Intel → darwin, Apple Silicon → arm64 (a macOS build, not Linux).
        assert_eq!(cpd_asset_token("macos", "x86_64"), Some("darwin"));
        assert_eq!(cpd_asset_token("macos", "aarch64"), Some("arm64"));
        assert_eq!(cpd_asset_token("linux", "x86_64"), Some("linux"));
        assert_eq!(cpd_asset_token("linux", "powerpc64le"), Some("ppc64le"));
        assert_eq!(cpd_asset_token("linux", "s390x"), Some("s390x"));
        // No cpd-cli build exists for Linux arm64.
        assert_eq!(cpd_asset_token("linux", "aarch64"), None);
    }

    #[test]
    fn parse_release_name_extracts_version_and_patch() {
        assert_eq!(
            parse_release_cpd("v14.4.0 … CPD 5.4.0"),
            Some(("5.4.0".into(), None))
        );
        assert_eq!(
            parse_release_cpd("v14.3.1.7 … CPD 5.3.1 - Patch 7"),
            Some(("5.3.1".into(), Some(7)))
        );
        assert_eq!(parse_release_cpd("no cpd token here"), None);
    }

    #[test]
    fn parse_version_fields_from_cpd_output() {
        let out = "cpd-cli\n\tVersion: 14.4.0\n\tBuild Date: x\n\tSWH Release Version: 5.4.0\n";
        assert_eq!(
            parse_version_field(out, "Version:").as_deref(),
            Some("14.4.0")
        );
        assert_eq!(
            parse_version_field(out, "SWH Release Version:").as_deref(),
            Some("5.4.0")
        );
    }

    #[test]
    fn select_target_by_version_arch_and_patch() {
        // 5.4.0 for each arch resolves to the right asset URL.
        assert_eq!(
            select_cpd_target(RELEASES, "5.4.0", "latest", "darwin")
                .unwrap()
                .url,
            "https://ex/darwin-1440"
        );
        assert_eq!(
            select_cpd_target(RELEASES, "5.4.0", "latest", "arm64")
                .unwrap()
                .url,
            "https://ex/arm64-1440"
        );
        let t = select_cpd_target(RELEASES, "5.4.0", "latest", "linux").unwrap();
        assert_eq!(
            (t.tag.as_str(), t.url.as_str()),
            ("v14.4.0", "https://ex/linux-1440")
        );

        // 5.3.1 "latest" → highest patch (7); explicit patch 7 → same.
        assert_eq!(
            select_cpd_target(RELEASES, "5.3.1", "latest", "darwin")
                .unwrap()
                .tag,
            "v14.3.1.7"
        );
        assert_eq!(
            select_cpd_target(RELEASES, "5.3.1", "7", "darwin")
                .unwrap()
                .tag,
            "v14.3.1.7"
        );

        // Unknown patch / version → error (not a wrong pick).
        assert!(select_cpd_target(RELEASES, "5.3.1", "3", "darwin").is_err());
        assert!(select_cpd_target(RELEASES, "9.9.9", "latest", "darwin").is_err());
    }

    fn ctx_in(runner: Arc<MockCommandRunner>, inputs: &[(&str, &str)]) -> StepContext {
        let map: BTreeMap<String, String> = inputs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        StepContext::with_artifacts(
            "r".into(),
            "mod-prereqs/cpd-cli-compat".into(),
            runner as Arc<dyn CommandRunner>,
            EventBus::new(),
            map,
            BTreeMap::new(),
            std::env::temp_dir(),
        )
    }

    #[tokio::test]
    async fn compat_auto_updates_when_installed_targets_wrong_release() {
        let host_token = cpd_asset_token(std::env::consts::OS, std::env::consts::ARCH)
            .expect("test host has a cpd-cli build");
        let runner = Arc::new(MockCommandRunner::new(vec![
            // installed cpd-cli targets 5.3.1 (mismatch with chosen 5.4.0)
            MockResponse::ok(
                "cpd-cli version",
                "cpd-cli\n\tVersion: 14.3.1\n\tSWH Release Version: 5.3.1\n",
            ),
            MockResponse::ok("api.github.com", RELEASES),
            MockResponse::ok("wxd-cpdx", ""), // the download/install sh script
            // after install, cpd-cli targets 5.4.0
            MockResponse::ok(
                "cpd-cli version",
                "cpd-cli\n\tVersion: 14.4.0\n\tSWH Release Version: 5.4.0\n",
            ),
        ]));
        let ctx = ctx_in(
            runner.clone(),
            &[("VERSION", "5.4.0"), ("PATCH_ID", "latest")],
        );
        assert_eq!(CpdCliCompatStep.run(&ctx).await, StepOutcome::Completed);
        // The install pulled THIS host's asset URL.
        let expected_url = select_cpd_target(RELEASES, "5.4.0", "latest", host_token)
            .unwrap()
            .url;
        assert!(
            runner.calls().iter().any(|c| c.contains(&expected_url)),
            "should download {expected_url}"
        );
    }

    #[tokio::test]
    async fn compat_is_noop_when_already_matching() {
        let runner = Arc::new(MockCommandRunner::new(vec![MockResponse::ok(
            "cpd-cli version",
            "cpd-cli\n\tVersion: 14.4.0\n\tSWH Release Version: 5.4.0\n",
        )]));
        let ctx = ctx_in(runner.clone(), &[("VERSION", "5.4.0")]);
        assert_eq!(CpdCliCompatStep.run(&ctx).await, StepOutcome::Completed);
        // No network / install when already correct.
        assert!(!runner.calls().iter().any(|c| c.contains("api.github.com")));
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
        let aws = ToolStep {
            spec: ToolSpec {
                id: "aws",
                title: "AWS CLI",
                probe: ("aws", vec!["--version".into()]),
                install: None,
                manual: vec!["install aws".into()],
            },
        };
        let runner =
            MockCommandRunner::new(vec![MockResponse::fail("aws --version", 127, "missing")]);
        match aws.run(&ctx(runner)).await {
            StepOutcome::Failed { next_steps, .. } => {
                assert_eq!(next_steps, vec!["install aws".to_string()])
            }
            o => panic!("expected Failed, got {o:?}"),
        }
    }

    #[tokio::test]
    async fn check_all_reports_status_for_every_tool() {
        // Default mock → all probes succeed → all present.
        let runner = MockCommandRunner::new(vec![]);
        let statuses = check_all(&runner).await;
        let ids: Vec<_> = statuses.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "oc",
                "helm",
                "openshift-install",
                "cpd-cli",
                "aws",
                "container-runtime"
            ]
        );
        assert!(statuses.iter().all(|s| s.present));
        // aws is the only check-only (non-installable) tool.
        assert!(!statuses.iter().find(|s| s.id == "aws").unwrap().installable);
    }
}
