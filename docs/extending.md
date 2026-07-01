# Extending the installer — worked examples

Copy-pasteable recipes for the three extension seams described in
[`architecture.md`](architecture.md) §12. Each example uses the **real trait
signatures** and shows exactly where to register it. Nothing here requires
touching `sw-core` (the orchestrator).

Signatures to keep handy (`sw-core`):

```rust
pub enum StepOutcome {
    Completed,
    NeedsInput { prompt: String, fields: Vec<InputField> },
    Failed { error: String, next_steps: Vec<String> },
}
pub struct InputField { pub key: String, pub label: String, pub secret: bool, pub default: Option<String> }
```

`StepContext` gives a step its only handle to the outside world:
`ctx.input(k)` / `ctx.secret(k)`, `ctx.log(..)` / `ctx.progress(pct)`,
`ctx.artifacts_dir()`, and the command seam `ctx.run(..)` / `ctx.run_with_env(..)`
/ `ctx.run_in_cluster(..)` / `ctx.run_in_cluster_pty_env(..)`. **Never call
`std::process` directly** — that's what keeps steps testable with
`MockCommandRunner`.

---

## 1. Add a step + module

Goal: after the install, run a quick health check that watsonx.data answers on
its route. One `Step`, wrapped in a `Module`, registered into the graph.

```rust
// crates/sw-mod-smoketest/src/lib.rs  (a new crate, or drop into an existing module crate)
use async_trait::async_trait;
use sw_core::{Module, Step, StepContext, StepOutcome};

/// A post-install smoke test module.
pub struct SmokeTestModule;

impl Module for SmokeTestModule {
    fn id(&self) -> &str { "mod-smoketest" }
    fn title(&self) -> &str { "Smoke test" }
    fn steps(&self) -> Vec<Box<dyn Step>> {
        vec![Box::new(CheckZenRoute)]
    }
}

struct CheckZenRoute;

#[async_trait]
impl Step for CheckZenRoute {
    fn id(&self) -> &str { "check-zen-route" }
    fn title(&self) -> &str { "Check the Software Hub route responds" }

    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        let ns = ctx.input("PROJECT_CPD_INST_OPERANDS").unwrap_or("cpd-instance");
        ctx.log("resolving the Software Hub route");

        // All external calls go through the seam; run_in_cluster sets KUBECONFIG.
        let host = match ctx
            .run_in_cluster("oc", &[
                "get".into(), "zenservice".into(), "lite-cr".into(),
                "-n".into(), ns.to_string(),
                "-o".into(), "jsonpath={.status.url}".into(),
            ])
            .await
        {
            Ok(o) if o.success() && !o.stdout.trim().is_empty() => o.stdout.trim().to_string(),
            _ => return StepOutcome::Failed {
                error: "Software Hub route not found".into(),
                next_steps: vec!["Confirm the ZenService is Completed, then Retry.".into()],
            },
        };

        // Idempotent + side-effect-free: safe to retry.
        ctx.progress(100);
        ctx.log(format!("Software Hub reachable at https://{host}"));
        StepOutcome::Completed
    }
}
```

Register it (append to the mode graph) in `crates/sw-api/src/lib.rs`:

```rust
pub fn default_registry() -> ModuleRegistry {
    ModuleRegistry::new()
        .with(Box::new(sw_mod_prereqs::PrereqsModule))
        .with(Box::new(sw_mod_provision::ProvisionModule::new()))
        .with(Box::new(sw_mod_storage::StorageModule))
        .with(Box::new(sw_mod_softwarehub::SoftwareHubModule))
        .with(Box::new(services_module()))
        .with(Box::new(sw_mod_smoketest::SmokeTestModule)) // ← new, runs last
}
```

The UI shows `mod-smoketest/check-zen-route` in the progress list automatically.

**Test it hermetically** (no cluster):

```rust
#[tokio::test]
async fn smoke_passes_when_route_present() {
    use sw_core::{MockCommandRunner, MockResponse};
    let runner = std::sync::Arc::new(MockCommandRunner::new(vec![
        MockResponse::ok("zenservice", "cpd-cpd-instance.apps.example.com"),
    ]));
    let ctx = /* build a StepContext with this runner (see existing module tests) */;
    assert_eq!(CheckZenRoute.run(&ctx).await, StepOutcome::Completed);
}
```

---

## 2. Add a cloud provider

Goal: an Azure provisioner. Implement the `Provisioner` trait and register it —
the step graph (`cluster-spec → preflight → ensure-dns → write-install-config →
create-cluster`), the UI cluster-spec form, and dispatch (by the `hyperscaler`
input) are all generic.

```rust
// crates/sw-mod-provision/src/azure.rs  (or a sibling crate)
use async_trait::async_trait;
use std::path::PathBuf;
use sw_core::{InputField, StepContext, StepOutcome};
use crate::Provisioner;

pub struct AzureProvisioner;

impl AzureProvisioner {
    pub fn new() -> Self { Self }
}

#[async_trait]
impl Provisioner for AzureProvisioner {
    fn id(&self) -> &str { "azure" }
    fn display_name(&self) -> &str { "Microsoft Azure" }

    /// Drives the UI cluster-spec form. Prefill sensible defaults.
    fn spec_fields(&self) -> Vec<InputField> {
        vec![
            InputField { key: "cluster_name".into(), label: "Cluster / resource name".into(), secret: false, default: Some("wxd".into()) },
            InputField { key: "region".into(), label: "Azure region".into(), secret: false, default: Some("eastus".into()) },
            InputField { key: "base_domain".into(), label: "Base domain (DNS zone)".into(), secret: false, default: None },
            InputField { key: "control_plane_type".into(), label: "Control plane VM size".into(), secret: false, default: Some("Standard_D8s_v5".into()) },
            InputField { key: "worker_type".into(), label: "Worker VM size".into(), secret: false, default: Some("Standard_D16s_v5".into()) },
            InputField { key: "worker_count".into(), label: "Worker node count".into(), secret: false, default: Some("3".into()) },
        ]
    }

    fn required_inputs(&self) -> Vec<&'static str> {
        vec!["cluster_name", "region", "base_domain", "worker_count"]
    }

    async fn preflight(&self, ctx: &StepContext) -> StepOutcome {
        // Verify `az`/creds, quotas, that the base-domain zone exists, etc.
        ctx.log("azure preflight");
        StepOutcome::Completed
    }

    async fn ensure_dns(&self, _ctx: &StepContext) -> StepOutcome {
        // Ensure the Azure DNS zone / delegation is in place.
        StepOutcome::Completed
    }

    fn write_install_config(&self, ctx: &StepContext) -> Result<PathBuf, StepOutcome> {
        // Render an Azure install-config.yaml into artifacts/cluster/, return its path.
        let dir = ctx.artifacts_dir().join("cluster");
        std::fs::create_dir_all(&dir).map_err(|e| StepOutcome::Failed {
            error: format!("could not create cluster dir: {e}"),
            next_steps: vec!["Check filesystem permissions, then retry.".into()],
        })?;
        // ... write install-config.yaml (platform: azure: ...) ...
        Ok(dir.join("install-config.yaml"))
    }

    async fn create(&self, ctx: &StepContext) -> StepOutcome {
        // `openshift-install create cluster --dir <artifacts/cluster>` (stream progress).
        ctx.log("creating the Azure OpenShift cluster");
        StepOutcome::Completed
    }

    async fn status(&self, _ctx: &StepContext) -> StepOutcome { StepOutcome::Completed }

    async fn destroy(&self, ctx: &StepContext) -> StepOutcome {
        // `openshift-install destroy cluster --dir <artifacts/cluster>` + provider cleanup.
        ctx.log("destroying the Azure cluster");
        StepOutcome::Completed
    }
}
```

Register it — the `ProvisionModule` dispatches on the `hyperscaler` input:

```rust
// wherever the ProvisionerRegistry is built (crates/sw-mod-provision):
ProvisionerRegistry::new()                     // AWS is built in
    .with(std::sync::Arc::new(AzureProvisioner::new()))
```

…and enable the provider chip in the UI catalog
(`crates/sw-api/src/catalog.rs`, set `enabled: true` for `azure`).

> ⚠️ **Also add RWX storage.** `sw-mod-storage` provisions **AWS EFS**
> specifically. A new cloud needs an equivalent module (Azure Files, GCP
> Filestore) that creates a ReadWriteMany storage class, or a storage-provider
> abstraction. `sw-mod-softwarehub` and `sw-mod-services` are already
> cloud-agnostic — they only use `oc`/`cpd-cli` through the seam.

---

## 3. Add a service

### 3a. Catalog-driven (the common case — no new code path)

Most entitled services install through the generic `ComponentsModule`, which
already handles cluster-scoped resources → `install-components` → verify. Just
add a catalog entry in `crates/sw-api/src/catalog.rs`:

```rust
pub fn services() -> Vec<Service> {
    // (display name, cpd-cli component token, default_selected)
    let rows = [
        ("watsonx.data", "watsonx_data", true),
        ("watsonx.ai",   "watsonx_ai",   false),
        ("Your Service", "your_component", false),  // ← new: becomes a UI checkbox
        // ...
    ];
    // ...
}
```

The service appears in the UI multi-select and installs via the same
`cpd-cli manage install-components --components your_component ...` flow. **No
Rust install logic needed** — the component token is the contract.

### 3b. Bespoke installer (custom install/verify logic)

When a service needs logic beyond `install-components`, implement
`ServiceInstaller` (see `crates/wxd-svc-watsonxdata` as the reference):

```rust
use async_trait::async_trait;
use sw_core::{StepContext, StepOutcome};
use sw_mod_services::ServiceInstaller;

pub struct MyServiceInstaller;

#[async_trait]
impl ServiceInstaller for MyServiceInstaller {
    fn service_id(&self) -> &str { "my-service" }     // used in step ids + UI
    fn display_name(&self) -> &str { "My Service" }
    fn component(&self) -> &str { "my_component" }      // cpd-cli COMPONENTS token

    async fn install(&self, ctx: &StepContext) -> StepOutcome {
        // Idempotent install via ctx.run_in_cluster_pty_env("cpd-cli", ...) + any
        // service-specific CRs. Return Failed { error, next_steps } on problems.
        ctx.log("installing My Service");
        StepOutcome::Completed
    }

    async fn verify(&self, ctx: &StepContext) -> StepOutcome {
        // Check the service CR reports Completed. Safe to call repeatedly.
        StepOutcome::Completed
    }
}
```

Compose it into a `ServicesModule` (each installer contributes `install-<id>` +
`verify-<id>` steps):

```rust
sw_mod_services::ServicesModule::new(vec![
    std::sync::Arc::new(MyServiceInstaller),
])
```

---

## 4. Add a run mode (new step graph)

Build a `ModuleRegistry` and insert it keyed by a mode name in `registries()`
(`crates/sw-api/src/lib.rs`). The UI offers it via `/catalog/modes`:

```rust
pub fn registries() -> BTreeMap<String, Arc<ModuleRegistry>> {
    let mut m = BTreeMap::new();
    m.insert("provision".into(), Arc::new(default_registry()));
    m.insert("existing".into(),  Arc::new(existing_registry()));
    m.insert("upgrade".into(),   Arc::new(upgrade_registry())); // ← new mode
    m
}
```

A run records its `mode`, so resume/retry always rebuild the same graph.

---

## Checklist before you PR

- [ ] All external calls go through `ctx.run*` (no `std::process`).
- [ ] Steps are idempotent (retry-safe) and set `progress`/`log`.
- [ ] `Failed` outcomes carry exact, copy-pasteable `next_steps`.
- [ ] Secrets via `ctx.secret()` / the secret store; masked in any echo.
- [ ] Hermetic tests with `MockCommandRunner`; `cargo test --workspace` green, 0 warnings.
- [ ] New module/provider/service registered in `crates/sw-api/src/lib.rs`
      (or the relevant registry) and, if user-visible, in `catalog.rs`.

See [`AGENTS.md`](../AGENTS.md) for conventions and the hard-won
cpd-cli/OpenShift operational notes.
