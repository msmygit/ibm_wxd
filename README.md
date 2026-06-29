# watsonx.data Easy Installer

A guided, modular installer that takes you from **nothing** to a running **IBM
watsonx.data** service — eventually end to end: create an OpenShift cluster on a
cloud provider, stand up **IBM Software Hub / Cloud Pak for Data 5.4.x** (latest:
5.4.0, `PATCH_ID=latest`), then add watsonx.data (and other services) — with a
Carbon-themed UI showing live status, a progress tracker, clear next steps, error
capture, and pause/resume. See the **[Roadmap](#roadmap)** for what's built vs.
planned, and issue [#1](https://github.com/msmygit/ibm_wxd/issues/1).

Docs: <https://www.ibm.com/docs/en/cloud-paks/cp-data>

## Status

This repo currently ships the **first increment**: `wxd-config` — a small,
dependency-free Rust CLI that collects the install configuration, validates it,
and generates a correct, deterministic, source-able `cpd_vars.sh`. It contacts no
cluster and installs nothing — it is the configuration front door the later
install modules consume. The cluster-provisioning, Software-Hub-install,
service-add, and Carbon-UI modules are **not built yet** (see [Roadmap](#roadmap)).

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
| 2 | **AWS OpenShift cluster provisioning** (control plane + workers; extensible to other clouds) | ⏳ planned |
| 3 | **IBM Software Hub install** (operators → control plane → readiness) | ⏳ planned |
| 4 | **Service framework + watsonx.data add-on** (extensible to other services) | ⏳ planned |
| 5 | **Carbon-themed UI** — live status, progress tracker, next steps, error capture, **pause/resume** | ⏳ planned |

Design for the end-to-end system is being worked under
`docs/superpowers/specs/` — see the latest design doc for architecture and module
boundaries.
