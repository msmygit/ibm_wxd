# Resume notes — live watsonx.data on OCP 4.21 / Software Hub 5.4.0

_Handoff written 2026-06-30 (evening). Pick up here tomorrow._

## TL;DR
A **real** end-to-end install is in flight and has passed every gate up to the
platform install. OpenShift 4.21 is fully up; EFS storage is provisioned; the
Software Hub prerequisite chain (login → entitlement → cert-manager → namespaces
→ License Service) is done at **5.4.0**. It's currently on `install-platform`
(cpd_platform). ⚠️ **The AWS cluster is running overnight — it costs ~$10–15/hr.**

## The live run
- **Run ID:** `02d24737-cbbd-47f1-a894-36cb10bf4096` (dir: `~/.wxd/runs/<id>/`)
- **Cluster:** `swwxdinstallpractice1` · base domain `wxd1.ocpcpdtest.com` · region `us-east-2`
  · **OCP 4.21.21** · **SWH 5.4.0** · infra ID `swwxdinstallpractice1-8lxmc`
- **Console:** https://console-openshift-console.apps.swwxdinstallpractice1.wxd1.ocpcpdtest.com
- **kubeadmin password:** `~/.wxd/runs/02d24737-.../artifacts/cluster/auth/kubeadmin-password`
- **KUBECONFIG:** `~/.wxd/runs/02d24737-.../artifacts/cluster/auth/kubeconfig`

## Progress (steps completed)
prereqs ✓ · cluster-spec/preflight/ensure-dns/write-config/**create-cluster (OCP 4.21)** ✓
· storage: ensure-efs / install-efs-csi / efs-storage-class ✓
· softwarehub: preflight-hub · restart-container · login-to-ocp · entitle-registry ·
wait-nodes-ready · install-cert-manager · create-namespaces · **apply-cluster-components
(License Service, ibm-licensing-operator v4.2.23 Succeeded)** · scheduler/br steps (skipped, opt-in) ✓
· **NOW:** `instance-cluster-resources` → next `install-platform` (cpd_platform) → wait-ready
· then services module installs **watsonx.data**.

## Operational state to preserve (don't lose these)
- **`/etc/hosts` pins** (required — macOS neg-DNS / same-name-reuse): the api name is
  pinned to the live control-plane IPs. Current pins:
  `18.224.232.249`, `3.137.53.55`, `3.149.199.161` → `api.swwxdinstallpractice1.wxd1.ocpcpdtest.com`.
  If the cluster is recreated these change — re-pin from `dig +short A <api> @8.8.8.8`.
- **olm-utils container:** `olm-utils-play-v4`, image `icr.io/cpopen/cpd/olm-utils-v4:5.4.0`,
  **logged into OCP**. Do NOT `docker rm` it — that wipes the login session (see gotcha #4).
- **Server:** must run with `DOCKER_HOST` set. It was started manually as:
  `DOCKER_HOST="unix://$HOME/.docker/run/docker.sock" nohup ./target/debug/wxd &` (port 4178).
- **cpd-cli workspace (server):** the container currently mounts
  `<repo>/cpd-cli-workspace/olm-utils-workspace/work` (see gotcha #3).

## How to resume the run
It auto-advances. If it parks at a `failed`/`awaiting_input` step, retry via:
`curl -s -X POST http://127.0.0.1:4178/api/runs/02d24737-cbbd-47f1-a894-36cb10bf4096/retry`
Before retrying a `cpd-cli manage` step, make sure the container is still logged in
(gotcha #4) — if unsure, re-run `login-to-ocp` (kubeadmin) into the container first.

## Confirmed live this session (works)
- OpenShift **version selector → openshift-install 4.21.21** (real).
- Resumable create + **DNS `/etc/hosts` fix**; **EFS RWX** module (efs-sc created live).
- The **5.4.0** olm-utils image + apply-cluster-components (License Service) — after the fixes below.

## Fixes shipped this session (merged to main)
- **PR #35** UX: awaiting-input shows an amber "Action needed" banner + auto-scrolls to the form.
- **PR #36** provision: detect local DNS-resolution failure and surface the exact
  copy-pasteable `/etc/hosts` remediation in the failed-step guidance.
- **PR #37** UI: cluster-spec goes read-only while a run is active (editable again on reset / spec failure).
- **PR #38** provision: **stream openshift-install progress** into the live log during create-cluster.
- **PR #39** softwarehub/services: **always set `OLM_UTILS_IMAGE=icr.io/cpopen/cpd/olm-utils-v4:${VERSION}`**
  (VERSION alone does NOT switch cpd-cli's olm-utils image; default is 5.3.1 → wrong release).

## KNOWN ISSUES / tool fixes still needed (do tomorrow)
1. **`DOCKER_HOST` not set by the tool.** cpd-cli's internal Go docker client reads
   `DOCKER_HOST` (not the docker CLI context). On Docker Desktop for Mac the daemon socket
   is `~/.docker/run/docker.sock`. The server only works because I exported `DOCKER_HOST`
   before launch. **Fix:** have the tool export `DOCKER_HOST` for `cpd-cli manage` (detect
   the socket: `~/.docker/run/docker.sock` or `/var/run/docker.sock`), e.g. in `cpd_env`.
2. **`CPD_CLI_MANAGE_WORKSPACE` ambiguity.** cpd-cli creates its workspace either next to the
   binary (`~/.wxd/bin/cpd-cli-workspace`) or in the cwd, causing two workspaces. The
   `instance-cluster-resources`/`scheduler`/`br` steps do `find cpd-cli-workspace -name
   cluster_scoped_resources.yaml` relative to cwd, which can miss the file. **Fix:** set
   `CPD_CLI_MANAGE_WORKSPACE` to a deterministic per-run dir (e.g. `<artifacts>/cpd-workspace`)
   in `cpd_env`, and point the `find` at that path.
3. **login-to-ocp session is ephemeral.** It lives in the olm-utils container; recreating the
   container (restart-container, or a manual `docker rm`) wipes it, and a retry that resumes
   *after* login-to-ocp then fails "please run login-to-ocp". **Fix:** make `cpd-cli manage`
   steps resilient — e.g. re-login inside apply-*/install-* steps, or don't recreate the
   container after login, or persist the session via the mounted workspace.
4. **cert-manager rollout timeout (180s) is too short** on a fresh cluster — the step is
   retryable so it recovers, but bump the `oc rollout status ... --timeout` to ~300s.
5. Consider surfacing the DNS `/etc/hosts` remediation even more prominently (PR #36 puts it
   in next_steps; a dedicated UI callout during the API-wait would be better).

## The corrected Software Hub flow (validated command-by-command)
`login-to-ocp` → `add-icr-cred-to-global-pull-secret --entitled_registry_key` → wait nodes Ready
→ install Red Hat cert-manager → create namespaces → `apply-cluster-components --licensing_ns`
(License Service) → [opt] scheduler (`apply-scheduler`) / br (`apply-br`, needs OADP) →
instance cluster-scoped resources (`case-download --cluster_resources` + `oc apply --server-side
--force-conflicts`) → `install-components --components=cpd_platform ...` → wait ZenService
Completed → services: `case-download` + `install-components --components=watsonx_data ...`.
Env every `cpd-cli manage` needs: `VERSION`, `PATCH_ID`, `OLM_UTILS_IMAGE`, `OPENSHIFT_TYPE`,
`IMAGE_ARCH` (+ `DOCKER_HOST`, `CPD_CLI_MANAGE_WORKSPACE` per fixes above).

## Teardown (when done / to stop cost)
`openshift-install destroy cluster --dir ~/.wxd/runs/02d24737-.../artifacts/cluster`
(or the UI Destroy button → runs the provisioner destroy, which also tears down EFS +
writes a destroy resource report, per PR #25). Then remove the `/etc/hosts` pins.
