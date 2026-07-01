# Roadmap

The goal is a true plug-n-play, end-to-end experience: from nothing to a running
**IBM watsonx.data** on **IBM Software Hub 5.4.x**, driven entirely from a
no-build web UI. Tracking issue:
[#1](https://github.com/msmygit/ibm_wxd/issues/1).

## Increments

| # | Increment | Status | Where |
|---|---|---|---|
| 1 | **Config generation** — `wxd-config` → `cpd_vars.sh` | ✅ shipped | `crates/wxd-config` |
| 2 | **AWS OpenShift provisioning** (control plane + workers, resource tagging, root-disk sizing) | ✅ shipped | `sw-mod-provision` |
| 3 | **IBM Software Hub install** (login → entitlement → cert-manager → namespaces → License Service → cluster-scoped resources → platform → readiness) | ✅ shipped | `sw-mod-softwarehub` |
| 4 | **RWX storage** (AWS EFS `efs-sc`, CSI operator) | ✅ shipped | `sw-mod-storage` |
| 5 | **Service framework + watsonx.data** (generic `ComponentsModule` + `ServiceInstaller`; catalog-driven multi-select) | ✅ shipped | `sw-mod-services`, `wxd-svc-watsonxdata` |
| 6 | **Web UI** — live status, progress tracker, next steps, error capture, pause/resume/retry, **access-details panel** | ✅ shipped | `sw-api/ui` |
| 7 | **Existing-cluster path** (skip provisioning, install onto your own cluster) | ✅ shipped | `sw-mod-existing` (mode `existing`) |
| 8 | **Prerequisite auto-install** (`oc`/`helm`/`openshift-install`/`cpd-cli` into `~/.wxd/bin`) + UI credential entry | ✅ shipped | `sw-mod-prereqs` |
| 9 | **DNS + unattended robustness** (Route53 delegation, macOS `/etc/hosts` remediation surfaced in-UI, `DOCKER_HOST` auto-detect, reconcile-race gating) | ✅ shipped | `sw-mod-provision`, `sw-mod-softwarehub` |
| 10 | **Other clouds** (IBM Cloud / Azure / GCP `Provisioner`s + matching RWX storage) | ⏳ planned | `Provisioner` seam |
| 11 | **More entitled services** as first-class catalog entries / bespoke installers | ⏳ ongoing | `ServiceInstaller` seam |

## What "planned" needs

- **New cloud provider (#10):** implement the `Provisioner` trait and register it
  in `ProvisionerRegistry` (the step graph, cluster-spec form, and dispatch are
  already generic). Also needs a cloud-appropriate **RWX storage** module
  (Azure Files / GCP Filestore) since `sw-mod-storage` is AWS-EFS-specific today.
- **More services (#11):** most entitled services just need a catalog entry
  (`catalog::services()`) — the generic `ComponentsModule` installs them through
  the same cluster-scoped-resources + `install-components` flow. Services needing
  custom logic implement `ServiceInstaller`.

See [`architecture.md`](architecture.md) for the extension seams in detail.
