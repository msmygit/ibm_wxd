# AGENTS.md — working on this repo as an AI agent

Practical onboarding for an AI coding agent extending or maintaining the **IBM
Software Hub + watsonx.data Easy Installer**. Read this, then
[`docs/architecture.md`](docs/architecture.md) for the design and diagrams.

---

## What this is (in one paragraph)

A Rust **Cargo workspace** that takes a user from nothing → a running
**watsonx.data** on **IBM Software Hub 5.4.x**: provision OpenShift (AWS IPI) →
RWX storage (EFS) → Software Hub control plane → services, driven by a **no-build
web UI**. A service-agnostic orchestrator (`sw-core`) flattens pluggable
**modules** (`sw-mod-*`) into an ordered list of **steps** and drives them,
persisting state after every step so runs are resumable. Every external command
goes through one `CommandRunner` seam. `sw-*` = generic; `wxd-*` = watsonx.data-specific.

---

## Environment (this workstation)

- **Rust isn't on the default PATH.** Prefix commands:
  `export PATH="/usr/local/opt/rust/bin:$HOME/.cargo/bin:$PATH"`.
- **The root `Cargo.toml` is a virtual manifest** (no root package). Always
  target a crate: `cargo run -p sw-api --bin wxd`, `cargo test --workspace`.
  `cargo run -- ...` / `cargo install --path .` at the root **do not work**.
- **GitHub:** use the `gh` CLI (the GitHub MCP is not authenticated). Repo is
  `msmygit/ibm_wxd`; `gh` is authed there.
- **Host Node is broken** (dyld mismatch) — no `node`/`npx`/Playwright. The UI is
  no-build static files; validate JS by reading, and the server by `curl`ing the
  endpoints, not with a browser harness.
- **Run state** lives under `~/.wxd/runs/<run-id>/` (`state.json`, `events.log`,
  `secrets.json` @ `0600`, `artifacts/`). Auto-installed CLIs go to `~/.wxd/bin`.

## Build / test / run

```bash
export PATH="/usr/local/opt/rust/bin:$HOME/.cargo/bin:$PATH"
cargo build --workspace
cargo test  --workspace           # must stay green, 0 warnings
cargo run -p sw-api --bin wxd      # web server on 127.0.0.1:4178 (WXD_PORT to change)
```

All tests are **hermetic** (mock `CommandRunner`, no cloud spend). Keep them that way.

---

## Non-negotiable conventions

1. **Never call `std::process` in a module.** Use `ctx.run` / `ctx.run_with_env` /
   `ctx.run_in_cluster` / `ctx.run_in_cluster_pty_env`. This is the seam that
   makes everything testable. New crates that shell out must go through it.
2. **Steps must be idempotent** (check-then-act). Retry/resume re-runs the current
   step — it must be safe to run twice.
3. **Fill `StepOutcome::Failed { next_steps }` with exact, copy-pasteable fixes.**
   The UI shows them verbatim; a user with no agent watching relies on them.
4. **Secret hygiene:** secrets go through `ctx.secret()` / the `0600` secret
   store, never into `state.json` or plain logs. Mask them in any command echo
   (`run_in_cluster_pty_masking`). The server binds `127.0.0.1` only.
5. **Tests + zero warnings** before a PR: `cargo test --workspace` and
   `cargo build --workspace 2>&1 | grep -c warning` → `0`.
6. **Git flow:** branch off `main` → commit → push → open PR with `gh`. Do **not**
   self-merge unless the user explicitly authorizes it (the harness blocks
   agent self-approval). Keep PRs focused; one concern per branch.
7. **Match surrounding style** — comment density, naming, error/next-step phrasing.

---

## The three extension seams (this is why the tool exists)

See [`docs/architecture.md`](docs/architecture.md) §12 for full signatures.

### Add a step / module
Implement `sw_core::Step` (group in a `Module`), register the module in the
mode's `ModuleRegistry` in `crates/sw-api/src/lib.rs` (`default_registry()` /
`existing_registry()`). Step ids surface as `"<module_id>/<step_id>"`.

### Add a cloud provider
Implement `sw_mod_provision::Provisioner` (`id`, `spec_fields`, `required_inputs`,
`preflight`, `ensure_dns`, `write_install_config`, `create`, `status`, `destroy`)
and register it in `ProvisionerRegistry::with(...)`. Dispatch is by the
`hyperscaler` input; the UI cluster-spec form is generated from `spec_fields()`.
**Also add a matching RWX storage module** — `sw-mod-storage` is AWS-EFS-specific.

### Add a service
- **Catalog-driven (preferred):** add it to `catalog::services()` in
  `crates/sw-api/src/catalog.rs` with its `cpd-cli` component token. The generic
  `ComponentsModule` installs it (cluster-scoped resources → `install-components`
  → verify) with no new code path.
- **Bespoke:** implement `sw_mod_services::ServiceInstaller` (see
  `wxd-svc-watsonxdata`) and compose into a `ServicesModule`.

### Add a run mode
New `ModuleRegistry` inserted into `registries()` keyed by a mode name; the UI
picks it up via `/catalog/modes`.

---

## Hard-won operational knowledge (cpd-cli / OpenShift)

These bit us on real runs and are now encoded in the modules — don't regress them:

- **`cpd-cli manage` needs `DOCKER_HOST`.** Its Go docker client reads
  `DOCKER_HOST`, not the docker CLI context. Auto-detected via
  `sw_core::detect_docker_host()` and injected into `cpd_env` / `services_cpd_env`.
- **`cpd-cli manage` needs a PTY.** Use `ctx.run_in_cluster_pty_env` (wraps with
  `script`), not a plain run.
- **`OLM_UTILS_IMAGE` must be set explicitly.** `VERSION` alone does NOT switch
  the olm-utils image cpd-cli uses (it reuses a baked-in default → wrong release).
  Default: `icr.io/cpopen/cpd/olm-utils-v4:${VERSION}`.
- **Services need cluster-scoped resources first.** Before `install-components`
  for a service, run `case-download --cluster_resources=true` and `oc apply
  --server-side --force-conflicts` the aggregated `cluster_scoped_resources.yaml`
  (covers the service **and** its dependency CASEs — EDB Postgres, opensearch,
  etc.). Missing CRDs → operators CrashLoop → `install-components` deadlocks.
- **`wait-ready` gates on the `Ibmcpd` CR** (`controlPlaneStatus=Completed` +
  non-empty `currentVersion`), not just ZenService — retrying `install-platform`
  re-triggers a reconcile that transiently blanks the version.
- **EFS on CAPI clusters (OCP 4.19+):** node SG is tagged
  `sigs.k8s.io/cluster-api-provider-aws/...`, NOT the in-tree
  `kubernetes.io/cluster/<infra>` tag (that lands on ELB SGs). Select the node SG
  by name `<infra>-node`. The EFS CSI operator Subscription needs an
  `OperatorGroup` in `openshift-cluster-csi-drivers` or OLM never installs it.
- **macOS negative-DNS cache** breaks the openshift-install API wait even when
  Route53 is correct. The tool surfaces the exact `/etc/hosts` pin +
  `dscacheutil -flushcache; killall -HUP mDNSResponder` command as a next-step.
- **`openshift-install destroy cluster` deletes `metadata.json`** — the provision
  module backs it up / restores it.
- **Version scheme:** Software Hub **5.4.0** maps internally to `Ibmcpd`
  `currentVersion` **5.10.0** and ZenService **6.10.1**. Don't compare release
  strings to CR versions directly.

---

## Where things are

| Topic | File |
|---|---|
| Drive loop / state machine | `crates/sw-core/src/orchestrator.rs` |
| Traits + command seam (`StepContext`) | `crates/sw-core/src/module.rs` |
| Types (`RunState`, `StepOutcome`, …) | `crates/sw-core/src/model.rs` |
| Persistence / resume | `crates/sw-core/src/store.rs` |
| Module wiring per mode | `crates/sw-api/src/lib.rs` |
| REST + SSE + access panel | `crates/sw-api/src/routes.rs` |
| UI (no build) | `crates/sw-api/ui/{index.html,app.js,styles.css}` |
| Catalog (hyperscalers, services) | `crates/sw-api/src/catalog.rs` |
| Cloud seam | `crates/sw-mod-provision/src/lib.rs` |
| Service seam | `crates/sw-mod-services/src/lib.rs`, `crates/wxd-svc-watsonxdata` |
| Architecture (diagrams) | `docs/architecture.md` |
| Roadmap | `docs/roadmap.md` |
| Operator walkthrough | `docs/running-the-installer.md` |
