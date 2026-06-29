# Running the installer end-to-end (real AWS cluster)

This guide takes you from nothing to a running watsonx.data service on a
self-managed OpenShift cluster on AWS, driven by the `wxd` web UI.

> **Cost warning.** This provisions real, **paid** AWS infrastructure (EC2,
> ELB, NAT, EBS, Route53 records) and pulls licensed IBM software. A
> watsonx.data-capable cluster (3 control-plane + 3 workers on the default
> instance types) costs real money per hour. **Always tear the cluster down**
> (the **Destroy cluster** button, or `openshift-install destroy cluster`) when
> you are done.

## 1. Prerequisites

### Build host
- **Rust toolchain** (`cargo`). On this machine: `export PATH="/usr/local/opt/rust/bin:$HOME/.cargo/bin:$PATH"`.

### CLIs on your `PATH` (the installer shells out to these)
| Tool | Used for | Get it |
|---|---|---|
| `openshift-install` | provisioning the cluster (IPI) | console.redhat.com/openshift/install/aws/installer-provisioned |
| `oc` | talking to the cluster | same downloads page |
| `helm` (v3.18+) | Software Hub install steps | helm.sh |
| `cpd-cli` | IBM Software Hub / CPD install | IBM Software Hub 5.4.x downloads |
| `aws` | credential preflight | aws.amazon.com/cli |

The installer's **preflight** steps check these and stop with actionable
guidance if any are missing — they do not silently proceed.

### Credentials & inputs you'll need
- **AWS account credentials** with IAM permissions for IPI, exported in the
  environment you launch `wxd` from (`AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`,
  optionally `AWS_SESSION_TOKEN`) **or** configured via `aws configure`
  (`~/.aws/credentials`). `openshift-install` reads them the same way the AWS CLI does.
- **A Route53 public hosted zone** — this is your cluster's **base domain**
  (e.g. `example.com`). The cluster is created under `<cluster-name>.<base-domain>`.
- **Red Hat pull secret** — from console.redhat.com/openshift/install/pull-secret.
  The UI asks for it as a masked field; it is written only into the cluster's
  `install-config.yaml` artifact.
- **IBM entitlement key** — from myibm.ibm.com → Container software library. The
  UI asks for it (masked) during the Software Hub phase. Stored only in the
  run's `secrets.json` (`0600`), never in `state.json` or logs.

## 2. Launch

```bash
export PATH="/usr/local/opt/rust/bin:$HOME/.cargo/bin:$PATH"
export AWS_ACCESS_KEY_ID=...      # or rely on ~/.aws/credentials
export AWS_SECRET_ACCESS_KEY=...

# Dev run (compiles then serves). Override the port with WXD_PORT.
cargo run -p sw-api --bin wxd

# Or build a release binary first:
cargo build --release -p sw-api && ./target/release/wxd
```

The server binds **127.0.0.1 only** and prints a URL containing a one-time
session token, e.g.:

```
http://127.0.0.1:4178/?token=<token>
```

Open it in your browser. The token authenticates the UI (the API rejects
requests without it).

## 3. Choose your path

On the start screen pick one:

- **Provision a new OpenShift cluster on AWS** (default) — the full flow below.
- **Use an OpenShift cluster I already have** — skips provisioning. You give the
  installer a **kubeconfig** (a path on this machine, e.g. `~/.kube/config`, or
  pasted contents) and it goes straight to installing Software Hub + watsonx.data
  on that cluster. This is the cheapest way to exercise the back half of the
  pipeline without paying for a new cluster.

> **Resource tagging.** In the provision path, the **Cluster / resource name**
> you enter tags *every* cloud resource the installer creates (`Name=<your-name>`
> via AWS `platform.aws.userTags` in `install-config.yaml`), and you can add more
> `key=value` tags in the **Extra cloud tags** field. The same tag input will
> flow to IBM Cloud / Azure / GCP `userTags` equivalents as those provisioners
> land. The existing-cluster path creates no resources, so there is nothing to tag.

## 4. The flow (provision a new cluster)

Click **Start install**. The run drives these phases; each streams live status,
a progress tracker, logs, and — on failure — an error with next steps. You can
**Pause / Resume / Retry** at any step boundary; state survives a restart of
`wxd` (it's persisted under `~/.wxd/runs/<id>/`).

1. **Preflight** — `oc` / `helm` / `aws` present.
2. **Provision cluster** (`mod-provision`)
   - *Define cluster spec*: region (default `us-east-1`), **base domain**,
     control-plane type/count (default `3 × m6i.2xlarge`), worker type/count
     (default `3 × m6i.4xlarge`).
   - *Preflight AWS*: `openshift-install` + `aws` + `aws sts get-caller-identity`.
   - *Write install-config*: prompts for the **Red Hat pull secret** (and an
     optional SSH key), renders `install-config.yaml`.
   - *Create cluster*: runs `openshift-install create cluster` (**15–40 min**).
     On success the kubeconfig is published to the run's artifacts so every later
     step targets the new cluster automatically (via `KUBECONFIG`).
3. **Install IBM Software Hub** (`mod-softwarehub`, 5.4.0) — prompts for the IBM
   entitlement key; installs operators → control plane → waits for readiness.
   Readiness can take a while; if *Wait for readiness* reports "not ready yet",
   click **Retry** after a few minutes.
4. **Install services** (`mod-services`) — **watsonx.data** is selected by
   default; installs it (`cpd-cli manage apply-cr --components watsonx_data`) and
   verifies it.

All steps are idempotent (check-then-act), so **Retry** and **Resume** are safe.

## 5. Artifacts

Everything for a run lives in `~/.wxd/runs/<run-id>/`:
- `state.json` — per-step status (no secrets).
- `events.log` — full event stream (replayed to the UI on reconnect).
- `secrets.json` — `0600`; entitlement key, pull secret, etc.
- `artifacts/kubeconfig` — the cluster's kubeconfig (use it with `oc` directly).
- `artifacts/cluster/` — the `openshift-install` working dir (incl.
  `auth/kubeadmin-password` and `.openshift_install.log`).

## 6. Teardown (do not skip)

In the UI, click **Destroy cluster** (enabled once provisioning completed), or
run it yourself:

```bash
openshift-install destroy cluster --dir ~/.wxd/runs/<run-id>/artifacts/cluster
```

If teardown is interrupted, some AWS resources may need manual cleanup in the
console — check the install log in the cluster dir for resource identifiers.

## 7. Hermetic development (no AWS, no spend)

Every module is tested against a mock command runner — no cloud, no real CLIs:

```bash
cargo test --workspace
```
