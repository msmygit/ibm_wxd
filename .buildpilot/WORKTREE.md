# WORKTREE.md

## Port Pool
4000-5000

## Files to Copy
- cpd_vars.sh
- .buildpilot.json
- .mcp.json

## Files to Skip
- cpd-cli-workspace/
- Diagnostics/
- *.log

## Port Env Vars

| File | Env Var | Value |
|------|---------|-------|

## DB Env Vars

| File | Env Var | Value |
|------|---------|-------|

## Database Setup
This project has no application database. watsonx.data is deployed onto the target
OpenShift cluster by the installer itself — there is no per-worktree database to
clone. `--with-db` does not apply here; this section is a no-op.

```bash
echo "No application database — nothing to set up for this worktree."
```

## Dependency Install
No package manager lockfile. The "dependencies" are client-workstation CLIs the
installer drives (`oc`, `cpd-cli`). Verify they are on PATH; fail fast if not.

```bash
command -v oc >/dev/null 2>&1 || { echo "oc CLI not found — install per IBM 'Setting up a client workstation' (Software Hub 5.3.x)"; exit 1; }
command -v cpd-cli >/dev/null 2>&1 || { echo "cpd-cli not found — download from the watsonx.data install docs and add to PATH"; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "jq not found — required by the install scripts"; exit 1; }
```

## Smoke Test
No local dev servers run. Readiness == a valid OpenShift session and a working
`cpd-cli`. Probe both; exit non-zero on failure.

```bash
oc whoami || { echo "Not logged in — run 'oc login <OCP_URL>' before working this ticket"; exit 1; }
cpd-cli version || exit 1
```

## Cleanup Hooks
No per-worktree database or local container was created, so there is nothing to
drop. Cluster-side resources created while working a ticket (projects, CPD
instances) are NOT torn down here — see `### Teardown`.

```bash
echo "No per-worktree DB/container to clean up." || true
```

## Notes

### Worktree Setup
BuildPilot tickets: `/buildpilot <TICKET>` runs `bp-prep` → `create.sh`, which adds
the worktree, copies the `## Files to Copy` list, and patches env files.

Manual feature work:

```bash
# from the workspace root (/Users/mrkr/Documents/00_coderepos/ibm_wxd)
git worktree add .worktree/<feature> -b feat/<feature> origin/main
cp cpd_vars.sh .buildpilot.json .mcp.json .worktree/<feature>/ 2>/dev/null || true
```

`cpd_vars.sh`, `.buildpilot.json`, and `.mcp.json` are gitignored and hold cluster
endpoints, credentials, and agent config — without them the `bp-*` tools and the
install scripts cannot run. Always copy them into a fresh worktree before starting.

### Services to Start
There is no local service stack — no backend, frontend, broker, or database to
launch. The only "service" is the **remote OpenShift cluster**. "Starting" the
environment means establishing an authenticated session:

```bash
oc login <OCP_URL> -u <OCP_USERNAME> -p <OCP_PASSWORD>   # or: oc login --token=<token> <OCP_URL>
oc whoami                                                # confirm session
oc project <PROJECT_CPD_INST_OPERANDS>                   # switch to the watsonx.data namespace
```

Dependency order: a valid `oc` session must exist before any `cpd-cli manage` /
install step. Nothing binds a localhost port.

### Environment Variables
There is no `.env.example` in the repo. All configuration is centralized in
`cpd_vars.sh`, which you populate per IBM's *Collecting required information* step
(Software Hub 5.3.x). Never commit a populated copy — it carries credentials and the
entitlement key. The variables the install flow expects (verify exact names against
the `cpd-cli` version in use):

- `OCP_URL` — required, OpenShift API server URL the installer targets.
- `OPENSHIFT_TYPE` — required, cluster flavor (e.g. `self-managed`, `roks`).
- `IMAGE_ARCH` — required, target architecture (e.g. `amd64`, `s390x`).
- `OCP_USERNAME` / `OCP_PASSWORD` — login credentials (or use `--token`); cluster-admin for install.
- `IBM_ENTITLEMENT_KEY` — required, pull secret for the IBM Entitled Registry.
- `PROJECT_CPD_INST_OPERATORS` — required, namespace for CPD operators.
- `PROJECT_CPD_INST_OPERANDS` — required, namespace for CPD operands (watsonx.data instance).
- `STG_CLASS_BLOCK` / `STG_CLASS_FILE` — required, RWO/RWX storage classes for the cluster.
- `VERSION` — required, watsonx.data / Software Hub release being installed (e.g. `5.3.x`).
- `COMPONENTS` — required, component list passed to `cpd-cli manage apply-cr`.

The `## Port Env Vars` and `## DB Env Vars` tables are empty — no values are
rewritten per worktree; every worktree shares the `cpd_vars.sh` it copied in.

### Database Setup and Migrations
No application database and no migration tool (`alembic` / `prisma` / `knex` are not
used). Persistent state lives inside watsonx.data on the cluster, provisioned by the
installer. The `bp-prep --with-db` heuristic is irrelevant for this repo — schema
tickets, if any, are watsonx.data catalog changes applied through its own tooling,
not local migrations.

### First-Run Checklist
```bash
# 1. Get the worktree (or use /buildpilot <TICKET>)
git worktree add .worktree/<feature> -b feat/<feature> origin/main
cd .worktree/<feature>

# 2. Bring in gitignored config
cp ../../cpd_vars.sh ../../.buildpilot.json ../../.mcp.json . 2>/dev/null || true

# 3. Confirm the client toolchain is installed
command -v oc && command -v cpd-cli && command -v jq

# 4. Load config and authenticate to the cluster
source ./cpd_vars.sh
oc login "$OCP_URL" -u "$OCP_USERNAME" -p "$OCP_PASSWORD"

# 5. Verify readiness
oc whoami
cpd-cli version
```

### Health Checks
No localhost `/health` endpoint exists. Readiness is checked against the cluster:

- `oc whoami` — verifies the `oc` session/token is still valid.
- `cpd-cli version` — verifies the installer CLI runs and matches the target release.
- `oc get csv -n "$PROJECT_CPD_INST_OPERATORS"` — verifies CPD operators reconciled (`Succeeded`).
- `oc get ZenService -n "$PROJECT_CPD_INST_OPERANDS"` — verifies the platform instance is `Completed`.
- Once watsonx.data is deployed: `curl --fail "$(oc get route -n "$PROJECT_CPD_INST_OPERANDS" cpd -o jsonpath='{.spec.host}')"` — verifies the console route answers.

### Common Failures
- **`oc` calls return "Unauthorized" / token expired.** First thing to check: `oc whoami`. If it errors, re-run `oc login "$OCP_URL" ...`; OpenShift tokens are short-lived and expire between sessions.
- **Install script errors on an empty/missing variable.** First thing to check: `echo "$OCP_URL"` (and the other required vars). If blank, you forgot `source ./cpd_vars.sh`, or `cpd_vars.sh` was never copied into this worktree.
- **`oc: command not found` or `cpd-cli: command not found`.** First thing to check: `command -v oc cpd-cli`. Install/extract the client workstation tools per the IBM "Setting up a client workstation" doc and add them to PATH before retrying.

### Teardown
BuildPilot worktrees: `bp-clean <TICKET-ID>`.

Manual worktrees:

```bash
git worktree remove --force .worktree/<feature>
git branch -D feat/<feature>
git worktree prune
```

There are no local containers or volumes to orphan. **The risk is cluster-side:**
removing the worktree does NOT delete anything created on OpenShift during the
ticket. If a ticket provisioned namespaces or a CPD/watsonx.data instance, clean
them up explicitly on the cluster (e.g. `oc delete project <ns>` /
`cpd-cli manage delete-cr ...`) or they keep consuming cluster quota and storage.

### Operational hints
- `create.sh` appends `.worktree/` to the workspace `.gitignore` on first
  run, even when the workspace isn't yet a git repo.
- `create.sh` writes `<worktree>/.ports` (one `KEY_PORT=N` per line) and,
  when `--with-db` is set, a `<worktree>/.with-db` marker. Do not delete
  either while debugging — `start.sh` and `stop.sh` read them.
- `create.sh` publishes the new branch with `git push -u origin <branch>`
  per repo. The push is atomic across repos: if any per-repo push fails,
  all previously pushed remote branches are deleted and all local
  worktrees are removed. Skipped when no `origin` remote exists.
- `create.sh` auto-starts a status dashboard on `http://localhost:9000`
  if no process is already bound to that port. One dashboard per
  workspace; subsequent worktrees reuse it.

---

Last verified by `[name/role]` on `[YYYY-MM-DD]` against commit `[sha]`.
