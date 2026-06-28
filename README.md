# Install watsonx.data with ease

A guided installer for IBM watsonx.data on an existing OpenShift cluster. This
repository currently contains the **first increment**: `wxd-config`, a small
Rust CLI that collects the install configuration, validates it, and generates a
correct, deterministic, source-able `cpd_vars.sh`. It contacts no cluster and
installs nothing — it is the configuration front door the later install modules
will consume.

## wxd-config

Collects every required watsonx.data install variable (interactively or
non-interactively), validates each value, and writes a `cpd_vars.sh` that the
downstream `cpd-cli`/`oc` steps can `source`.

### Build & test

Requires a Rust toolchain (`cargo`). No cluster, `oc`, or `cpd-cli` is needed.

```bash
cargo build              # compile
cargo test               # run unit + integration tests
cargo run -- --help      # see usage and required inputs
```

### Usage

```text
wxd-config [OPTIONS]

MODES
  Interactive (default)   Prompts for any required value not already supplied
                          via --answers or the environment. Secret values are
                          read without echo and never printed back.
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

Run `wxd-config --help` for the full, always-current list of required inputs.

### Required inputs

The generated `cpd_vars.sh` matches the documented contract in
`.buildpilot/WORKTREE.md`:

| Variable | Meaning | Validation |
|---|---|---|
| `OCP_URL` | OpenShift API server URL | well-formed `https://` URL |
| `OPENSHIFT_TYPE` | Cluster flavor | one of `self-managed`, `roks` (unknown warns) |
| `IMAGE_ARCH` | Target architecture | one of `amd64`, `s390x` (unknown warns) |
| `OCP_USERNAME` | OpenShift login user | non-empty |
| `OCP_PASSWORD` | OpenShift login password | non-empty (secret) |
| `IBM_ENTITLEMENT_KEY` | IBM Entitled Registry pull secret | non-empty (secret) |
| `PROJECT_CPD_INST_OPERATORS` | CPD operators namespace | Kubernetes namespace |
| `PROJECT_CPD_INST_OPERANDS` | CPD operands namespace | Kubernetes namespace |
| `STG_CLASS_BLOCK` | RWO (block) storage class | non-empty |
| `STG_CLASS_FILE` | RWX (file) storage class | non-empty |
| `VERSION` | watsonx.data / Software Hub release | non-empty |
| `COMPONENTS` | Component list for `apply-cr` | non-empty |

Each value may be provided three ways, in increasing precedence:
1. an answers file (`--answers FILE`, `KEY=VALUE` lines),
2. an environment variable of the same name,
3. an interactive prompt (interactive mode only).

### Examples

```bash
# Non-interactive from an answers file
wxd-config --non-interactive --answers ./my-answers.txt

# Non-interactive from the environment
export OCP_URL=https://api.cluster.example.com:6443
export OPENSHIFT_TYPE=self-managed
# ... export the rest ...
wxd-config --non-interactive

# Interactive — prompts for whatever is still missing
wxd-config
```

### Validation & safety

- **Fail early, fail clearly** — a missing or malformed value is rejected up
  front, naming the variable and the rule violated. No partial file is written.
- **Allowed-value sets** — `OPENSHIFT_TYPE`/`IMAGE_ARCH` accept the documented
  values silently; an unrecognised but plausibly-formatted value is *warned*
  about and allowed (the exact set varies by `cpd-cli` version).
- **Safe shell quoting** — every value is single-quoted so sourcing the file
  reproduces the exact input with no shell expansion or injection.
- **Deterministic** — the same inputs always produce a byte-identical file.
- **Secret hygiene** — `OCP_PASSWORD` and `IBM_ENTITLEMENT_KEY` are read without
  echo and masked (`********`) in all console output. The generated file
  contains the real values, so **never commit it** — it is gitignored.

### Project layout

```text
Cargo.toml
src/
  main.rs        thin binary: wires real IO, maps outcomes to exit codes
  lib.rs         run() orchestration + real stdin (no-echo) prompter
  spec.rs        the authoritative variable contract (single source of truth)
  validate.rs    required-ness, URL, namespace, allowed-value checks
  generate.rs    safe shell-quoting + deterministic cpd_vars.sh rendering
  collect.rs     interactive + non-interactive input collection
  mask.rs        secret masking for console output
  cli.rs         argument parsing + --help/usage text
tests/
  cli_integration.rs   subprocess tests incl. bash -n and source round-trip
```

### Out of scope (this increment)

Prerequisite checking against a live cluster, scoped-resource/shared-component
setup, running the actual install, checkpoint/resume, and the Carbon UI are
deferred to follow-on tickets — see `.buildpilot/tickets/1/product_context.md`.
