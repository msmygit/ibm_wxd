# watsonx.data Easy Installer

A guided, modular installer that takes you from **nothing** to a running **IBM
watsonx.data** service, end to end: create a self-managed OpenShift cluster on a
cloud provider (AWS today; IBM Cloud / Azure / GCP behind the same interface),
stand up **IBM Software Hub / Cloud Pak for Data 5.4.x** (latest: 5.4.0,
`PATCH_ID=latest`), then add watsonx.data (and other entitled services) — with a
simple **no-build web UI** (light/dark) showing live status, a progress tracker,
clear next steps, error capture, and **pause/resume/retry**. See the
**[Roadmap](#roadmap)** for status and issue
[#1](https://github.com/msmygit/ibm_wxd/issues/1).

Docs: <https://www.ibm.com/docs/en/cloud-paks/cp-data>

## Run the installer

```bash
export PATH="/usr/local/opt/rust/bin:$HOME/.cargo/bin:$PATH"
cargo run -p sw-api --bin wxd   # binds 127.0.0.1, prints a tokenized URL to open
```

Open the printed `http://127.0.0.1:<port>/?token=<token>` URL, choose a mode, and
click **Start install**. The tool **auto-installs the CLIs it needs**
(`oc`, `helm`, `openshift-install`, `cpd-cli`) into `~/.wxd/bin` — you don't
pre-install them. Two run modes:

- **Provision a new AWS cluster** — creates OpenShift (IPI), installs Software Hub
  5.4.0 + watsonx.data. Needs AWS credentials, a Route53 base domain, a Red Hat
  pull secret, and an IBM entitlement key.
- **Use an existing cluster** — give it a kubeconfig; it installs Software Hub +
  watsonx.data onto your cluster (no provisioning spend).

Supply credentials in the UI's **Cloud credentials** panel (AWS / IBM / Azure /
GCP), or leave them blank to fall back to `~/.aws/credentials` and
`~/.ibm/IBM_CLOUD_API_KEY`. Every created cloud resource is tagged with the name
you provide. Full walkthrough: **[Running the installer guide](docs/running-the-installer.md)**.
All tests are hermetic (no cloud spend): `cargo test --workspace`.

## Status

The installer is a **Cargo workspace** (binary: `wxd`). Generic IBM Software Hub
infrastructure is prefixed `sw-*`; watsonx.data-specific code is `wxd-*`:

- **`sw-core`** — orchestrator spine: Module/Step framework, run state machine
  with pause/resume/retry, event bus → SSE, `CommandRunner` seam (no module
  calls `std::process`), run store under `~/.wxd`.
- **`sw-api`** — axum web server (OpenAPI 3.1.0 REST + SSE), serves the no-build
  UI, binds 127.0.0.1 with a session token.
- **`sw-mod-prereqs`** — auto-installs `oc` / `helm` / `openshift-install` /
  `cpd-cli` into `~/.wxd/bin`.
- **`sw-mod-existing`** — adopt an existing cluster via kubeconfig (mode `existing`).
- **`sw-mod-provision`** — `Provisioner` trait + `AwsProvisioner`
  (`openshift-install` IPI), tags every resource with your name.
- **`sw-mod-softwarehub`** — IBM Software Hub 5.4.0 (operators → control plane →
  readiness).
- **`sw-mod-services`** + **`wxd-svc-watsonxdata`** — service framework +
  watsonx.data installer (default service).
- **`wxd-config`** — the original `cpd_vars.sh` generator (below), still shipped.

## Getting started

### Prerequisites

- A **Rust toolchain** (`cargo`). On this machine it's installed via Homebrew at
  `/usr/local/opt/rust/bin` and may not be on your `PATH`.

### Build, install, and run

```bash
# 1. Make sure cargo is on your PATH (Homebrew Rust):
export PATH="/usr/local/opt/rust/bin:$PATH"

# 2. From the repo root, build and run the tests:
cargo build
cargo test

# 3. Run it directly without installing:
cargo run -- --help

# 4. Or install the `wxd-config` command onto your PATH (~/.cargo/bin):
cargo install --path .

# 5. Make ~/.cargo/bin available in every new terminal (one-time):
echo 'export PATH="/usr/local/opt/rust/bin:$HOME/.cargo/bin:$PATH"' >> ~/.zshrc
source ~/.zshrc

# 6. Now the command is available:
wxd-config --help
```

> If `wxd-config: command not found`, `~/.cargo/bin` isn't on your `PATH` — run
> step 5, open a new terminal, or call it by full path: `~/.cargo/bin/wxd-config`.

### Generate a `cpd_vars.sh`

```bash
# Non-interactive from an answers file (KEY=VALUE lines, # comments ok):
wxd-config --non-interactive --answers ./my-answers.txt --output ./cpd_vars.sh

# Non-interactive from the environment:
export OCP_URL=https://api.cluster.example.com:6443
# ... export the other required vars ...
wxd-config --non-interactive

# Interactive — prompts for whatever is still missing (secrets are hidden):
wxd-config
```

Then verify and use the file with the (manual, for now) IBM install steps:

```bash
bash -n cpd_vars.sh && source cpd_vars.sh   # never commit cpd_vars.sh — it holds secrets
```

## `wxd-config` reference

### Modes & options

```text
wxd-config [OPTIONS]

MODES
  Interactive (default)   Prompts for any required value not already supplied
                          via --answers or the environment. Secrets are read
                          without echo and never printed back.
  --non-interactive       Never prompts. Every required value must come from
                          --answers and/or environment variables, or the run
                          fails listing what is missing.

OPTIONS
  --non-interactive       Do not prompt; use --answers + environment only.
  --answers <FILE>        Read KEY=VALUE answers from FILE (# comments ok).
  --output <FILE>         Output path for cpd_vars.sh (default: ./cpd_vars.sh).
  -h, --help              Print help and exit.
  -V, --version           Print version and exit.
```

Run `wxd-config --help` for the always-current, authoritative input list.

### Required inputs (IBM Software Hub 5.4.0 contract)

Each value may be provided via `--answers`, an environment variable, or an
interactive prompt (precedence: env > answers file > prompt > built-in default).

| Variable | Meaning | Validation |
|---|---|---|
| `OCP_URL` | OpenShift API server URL | well-formed `https://` URL |
| `OPENSHIFT_TYPE` | Cluster flavor | one of `self-managed`, `roks` (unknown warns) |
| `IMAGE_ARCH` | Target architecture | one of `amd64`, `s390x` (unknown warns) |
| `OCP_USERNAME` | OpenShift login user | non-empty *(auth, choose-one)* |
| `OCP_PASSWORD` | OpenShift login password | non-empty, secret *(auth, choose-one)* |
| `OCP_TOKEN` | OpenShift login token | non-empty, secret *(auth, choose-one)* |
| `IBM_ENTITLEMENT_KEY` | IBM Entitled Registry pull secret | non-empty, secret |
| `IMAGE_PULL_SECRET` | Image pull secret name | non-empty |
| `PROJECT_CPD_INST_OPERATORS` | CPD operators namespace | Kubernetes namespace |
| `PROJECT_CPD_INST_OPERANDS` | CPD operands namespace | Kubernetes namespace |
| `PROJECT_LICENSE_SERVICE` | IBM License Service namespace | Kubernetes namespace |
| `PROJECT_SCHEDULING_SERVICE` | Scheduling service namespace | Kubernetes namespace |
| `PROJECT_SCHEDULING_BR_SVC` | Scheduling backup/restore namespace | Kubernetes namespace |
| `STG_CLASS_BLOCK` | RWO (block) storage class | non-empty |
| `STG_CLASS_FILE` | RWX (file) storage class | non-empty |
| `VERSION` | watsonx.data / IBM Software Hub release | non-empty; **defaults to `5.4.0`** |
| `PATCH_ID` | Patch level for the release | non-empty; **defaults to `latest`** |
| `COMPONENTS` | Component list for `apply-cr` (must include `watsonx_data`) | non-empty |

**Cluster auth is choose-one:** provide **both** `OCP_USERNAME` and `OCP_PASSWORD`,
**or** provide `OCP_TOKEN`. Only the chosen method's variables are written.

**Derived (computed automatically, never prompted):** `SERVER_ARGUMENTS`,
`OLM_UTILS_IMAGE` (`icr.io/cpopen/cpd/olm-utils-v4:${VERSION}`), `PROJECT_INST_BR_SVC`
(`${PROJECT_CPD_INST_OPERATORS}-br-svc`).

`VERSION` and `PATCH_ID` are the only defaulted variables; every other required
variable errors if missing.

### Validation & safety

- **Fail early, fail clearly** — a missing/malformed value is rejected up front,
  naming the variable and the rule violated; no partial file is written.
- **Safe shell quoting** — values are single-quoted so sourcing reproduces the
  exact input with no shell expansion or injection.
- **Deterministic** — identical inputs always produce a byte-identical file, and
  re-feeding a generated file via `--answers` round-trips exactly.
- **Secret hygiene** — `OCP_PASSWORD`, `OCP_TOKEN`, and `IBM_ENTITLEMENT_KEY` are
  read without echo and masked (`********`) in console output. The generated file
  holds real values, so **never commit it** (it is gitignored).

### Project layout

```text
Cargo.toml
src/
  main.rs        thin binary: wires real IO, maps outcomes to exit codes
  lib.rs         run() orchestration + real stdin (no-echo) prompter
  spec.rs        the authoritative variable contract (single source of truth)
  validate.rs    required-ness, URL, namespace, allowed-value, auth checks
  generate.rs    safe shell-quoting + deterministic cpd_vars.sh rendering
  collect.rs     interactive + non-interactive input collection
  mask.rs        secret masking for console output
  cli.rs         argument parsing + --help/usage text
tests/
  cli_integration.rs   subprocess tests incl. bash -n and source round-trip
```

### Downstream client-workstation prerequisites

The later install modules drive the cluster with these client tools — IBM
Software Hub 5.4.x requires all three on `PATH`:

- `oc` — the OpenShift CLI.
- `cpd-cli` — the Cloud Pak for Data CLI (matched to the 5.4.x release).
- `helm` — **v3.18 / 3.19 / 3.20+** (5.x install steps use Helm).

`wxd-config` itself needs none of these — only a Rust toolchain to build.

## Roadmap

The goal is a true plug-n-play, end-to-end experience. Increments:

| # | Module | Status |
|---|---|---|
| 1 | **Config generation** (`wxd-config` → `cpd_vars.sh`) | ✅ shipped |
| 2 | **AWS OpenShift cluster provisioning** (control plane + workers; extensible to other clouds) | ✅ shipped (`sw-mod-provision`) |
| 3 | **IBM Software Hub install** (operators → control plane → readiness) | ✅ shipped (`sw-mod-softwarehub`) |
| 4 | **Service framework + watsonx.data add-on** (extensible to other services) | ✅ shipped (`sw-mod-services` + `wxd-svc-watsonxdata`) |
| 5 | **Web UI** — live status, progress tracker, next steps, error capture, **pause/resume/retry** | ✅ shipped (no-build UI, light/dark) |
| 6 | **Existing-cluster path** (skip provisioning, install onto a cluster you already have) | ✅ shipped (`sw-mod-existing`, run mode `existing`) |
| 7 | **Cloud resource tagging** — user-provided name tags every created resource (AWS `userTags`; same input flows to other clouds) | ✅ shipped |
| 8 | **Prerequisite auto-install** (`oc`/`helm`/`openshift-install`/`cpd-cli` into `~/.wxd/bin`) + UI credential entry (AWS/IBM/Azure/GCP) | ✅ shipped (`sw-mod-prereqs`) |
| 9 | Other clouds (IBM Cloud / Azure / GCP) as working provisioners + other entitled IBM services | ⏳ planned |

The orchestrator + web server are generic IBM Software Hub infrastructure
(`sw-*`); only watsonx.data-specific code is `wxd-*`, so other entitled IBM
services plug in behind the same `ServiceInstaller` trait. Design lives under
`docs/superpowers/specs/`; the end-to-end run guide is
[`docs/running-the-installer.md`](docs/running-the-installer.md).
