# Design — watsonx.data Easy Installer (end-to-end)

**Date:** 2026-06-28
**Status:** Approved (pending written-spec review)
**Tracking issue:** [#1](https://github.com/msmygit/ibm_wxd/issues/1)
**Supersedes scope of:** first increment `wxd-config` (folded in as `mod-config`)

## 1. Goal

A guided, modular installer that takes a user from nothing to a running IBM
watsonx.data service, end to end:

> choose hyperscaler → spec the cluster (control plane + workers) → create a
> self-managed OpenShift cluster on AWS → install IBM Software Hub 5.4.0 → select
> and install services (watsonx.data by default; other entitled IBM services
> optional) — with a simple web UI showing live status, a progress tracker, clear
> next steps, captured errors, and pause/resume.

Extensible by design: new clouds and new services are added behind traits without
changing the core.

## 2. Scope

### In scope (v1)
- Hyperscaler selection with **AWS enabled** (IBM Cloud / Azure / GCP stubbed as
  "coming soon" behind the same interface).
- Self-managed OpenShift on AWS via **`openshift-install` (IPI)** — user specifies
  control-plane and worker machine specs.
- Config generation (`cpd_vars.sh`) via the existing `wxd-config`.
- IBM Software Hub 5.4.0 install (operators → control plane → readiness).
- Service selection + install: **watsonx.data preselected by default**, plus other
  IBM services the user is entitled to.
- Local **web server + no-build web UI** (light/dark), OpenAPI 3.1.0 REST + SSE.
- **Pause / resume / retry** with persisted run state.
- Hermetic tests (mocked external commands).

### Out of scope (v1)
- Carbon design system / any Node/npm build toolchain (deliberately avoided).
- Tauri / native desktop packaging (possible later increment).
- Non-AWS provisioners as working implementations (interface only).
- Day-2 ops (upgrades, scaling, backup/restore beyond what `cpd_vars.sh` records).
- Automated real-cloud e2e in CI (real provisioning costs money; opt-in manual only).

## 3. Architecture

A **Cargo workspace** plus a static web UI, shipped as one binary `wxd` that runs a
local web server (bind `127.0.0.1`, session token) and serves the UI.

```
wxd (binary)
 ├─ api ............ axum web server: OpenAPI 3.1.0 REST + SSE; serves static UI
 ├─ core .......... orchestrator: Module/Step framework, Run state machine,
 │                  pause/resume, event bus, CommandRunner abstraction
 ├─ modules:
 │    mod-config ...... wraps existing wxd-config (cpd_vars.sh generation)
 │    mod-provision ... Provisioner trait; AwsProvisioner (openshift-install IPI)
 │    mod-softwarehub . IBM Software Hub install (operators → control plane → ready)
 │    mod-services .... ServiceCatalog + ServiceInstaller; watsonx.data default
 └─ ui/ ........... static index.html + styles.css + app.js (NO build step)
```

Rationale for app shell (decided): local web server + browser UI (not CLI-first,
not Tauri) gives real pause/resume controls, live push updates, remote/headless
use, multi-client reuse, and the best substrate for parallel module development.

## 4. The spine (module framework)

- **`Module` trait** — declares ordered **idempotent, resumable `Step`s**. Each step
  emits status/log/progress events through a channel → SSE → UI. A step returns one
  of `Completed | NeedsInput | Failed { error, next_steps } | Paused`.
- **`Provisioner` trait** — cluster lifecycle (`plan`, `create`, `status`,
  `destroy`). `AwsProvisioner` (openshift-install IPI) is the first impl; IBM
  Cloud/Azure/GCP are added as new impls with no core change.
- **`ServiceInstaller` trait + `ServiceCatalog`** — watsonx.data is the default
  catalog entry; other IBM services listed and gated by entitlement. New service =
  catalog entry + installer impl.
- **`CommandRunner` trait** — the single seam for every external command
  (`openshift-install`, `oc`, `cpd-cli`, `helm`, `aws`). Real impl shells out; mock
  impl powers hermetic tests. No module calls `std::process` directly.

Each module is a separate crate depending only on `core`'s traits → independently
buildable/testable → suitable for parallel subagent development.

## 5. Run model + pause/resume

- A **Run** is an ordered sequence of phases/steps across modules, persisted to
  `~/.wxd/runs/<run-id>/`:
  - `state.json` — per-step status, inputs (secrets redacted/handled separately),
    checkpoints, timestamps.
  - `events.log` — append-only event stream (also replayed to late SSE subscribers).
  - `artifacts/` — kubeconfig, generated `cpd_vars.sh`, install logs.
- **Pause** — stop at the next step boundary (steps are not interrupted mid-call).
- **Resume** — continue from the last completed checkpoint; survives process restart.
- **Retry** — re-run a failed step; safe because steps are idempotent (check-then-act).
- **Secrets** — never written to `state.json` or logs in plaintext; held in a
  separate secret store file with `0600` perms (or OS keychain later), masked in all
  UI/log output (reuses `wxd-config`'s masking discipline).

## 6. End-to-end wizard (UX phases)

Each phase streams live status, shows a progress tracker, captures errors with
actionable next steps, and is pausable/resumable.

1. **Choose hyperscaler** — AWS (enabled); IBM Cloud/Azure/GCP "coming soon".
2. **Cluster infra spec** — region, base domain (Route53 hosted zone), control-plane
   count + instance type, worker count + instance type. Sensible defaults provided.
3. **Credentials** — AWS credentials, Red Hat pull secret, IBM entitlement key
   (secret-masked inputs).
4. **Provision cluster** — generate `install-config.yaml` → `openshift-install create
   cluster`; live progress; outputs kubeconfig + kubeadmin password to `artifacts/`.
5. **Generate config** — `mod-config` produces `cpd_vars.sh` for the new cluster.
6. **Install IBM Software Hub** — operators → control plane → readiness (5.4.0,
   `PATCH_ID=latest`).
7. **Select services** — **watsonx.data preselected**; other entitled IBM services
   shown as checkboxes (entitlement-aware).
8. **Install services** — apply selected services; live readiness checks.
9. **Done** — summary: console URLs, credentials location, teardown (destroy) option.

A "use my existing cluster" shortcut (skip phases 1–4, jump to config/install) is a
candidate add — see Open Questions.

## 7. API — OpenAPI 3.1.0 (contract-first)

REST resources (all under `/api`):
- `POST /runs`, `GET /runs`, `GET /runs/{id}` — create/list/inspect runs.
- `POST /runs/{id}/pause`, `/resume`, `/retry` — control plane.
- `POST /runs/{id}/inputs` — submit answers when a step is `NeedsInput`.
- `GET /runs/{id}/events` — **SSE** stream of step status / logs / progress %.
- `GET /catalog/hyperscalers`, `GET /catalog/services` — capability + entitlement.
- `GET /modules` — registered modules and their step graphs.

The OpenAPI 3.1.0 document is the frozen contract: it generates the UI's request
types and documents the seam. SSE event payloads are JSON schemas referenced from
the spec (described as an event-typed stream, since SSE is plain HTTP).

## 8. UI (no-build, light/dark)

- Static `index.html` + `styles.css` + `app.js` served by axum. **No Node, no npm,
  no bundler.**
- Light/dark via CSS custom properties + `prefers-color-scheme` + a persisted toggle.
- `fetch()` for REST; browser-native `EventSource` for the SSE event stream.
- Screens map to the 9 wizard phases + a run dashboard (progress tracker, per-step
  status, live logs, error panels with next-step actions, pause/resume/retry buttons).
- Clean, crisp, actionable; accessible (semantic HTML, keyboard-navigable).

## 9. External tooling & prerequisites

Modules shell out (via `CommandRunner`) to: `openshift-install`, `oc`, `cpd-cli`,
`helm` (v3.18+), `aws`. A **preflight step** checks for required tools and
credentials and gives actionable install guidance if missing (it does not silently
proceed). `wxd` itself needs only the Rust toolchain to build.

## 10. Testing

- **Hermetic by default** — mock `CommandRunner` returns canned tool output; no AWS
  spend, no real cluster, no `oc`/`cpd-cli` needed. Unit + integration tests cover
  step logic, state transitions, pause/resume/retry, error capture, API contract.
- **Real e2e** — an explicit, opt-in manual procedure (provisions a *paid* AWS
  cluster). Never in automated CI.

## 11. Extensibility

- **New cloud** → implement `Provisioner`; register in the hyperscaler catalog.
- **New service** → add a `ServiceCatalog` entry + `ServiceInstaller` impl.
- **New step/module** → implement `Module`; the orchestrator and UI render it
  generically from `/modules` + the event stream.

## 12. Build plan (parallel subagents)

- **Phase A — spine (sequential, first):** `core` (Module/Step/Run/state/event bus/
  CommandRunner) + the **OpenAPI 3.1.0 contract** + the static UI shell + axum `api`
  skeleton. Freezes every contract the rest builds against.
- **Phase B — modules (parallel subagents):** `mod-provision` (AWS IPI),
  `mod-softwarehub`, `mod-services`, and the UI wizard screens — each built
  independently against the frozen contract + mock `CommandRunner`, in isolated
  worktrees to avoid collisions.
- **Phase C — integration:** wire modules into the orchestrator, end-to-end on the
  UI against mocks; then the opt-in real e2e.

## 13. Risks & mitigations

- **Host Node broken** (`libllhttp` dyld error) → eliminated by the no-build UI
  (no Node toolchain in the project at all).
- **Real provisioning costs money / has AWS prereqs** (IAM, quotas, Route53 base
  domain) → hermetic tests by default; preflight checks; clear teardown path.
- **Entitlement detection** for the service catalog is uncertain → v1 may present a
  known IBM-service catalog and validate entitlement at install time rather than
  pre-filtering (see Open Questions).
- **openshift-install version drift** → pin/declare a supported version range; the
  preflight step verifies it.

## 14. Open questions (non-blocking; resolve during planning)

1. **Existing-cluster shortcut** — offer "I already have a cluster" to skip
   provisioning (phases 1–4)? Answer: yes; cheap and useful.
2. **Entitlement detection** — derive entitled services from the entitlement key, or
   present a static catalog and validate at install? Answer: static + validate-at-install.
3. **Default machine specs** — concrete defaults for control-plane/worker counts and
   instance types for a watsonx.data-capable cluster.
4. **Secret storage** — file with `0600` for v1 vs OS keychain. Answer: Yes.
