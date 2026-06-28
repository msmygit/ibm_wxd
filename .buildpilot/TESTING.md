# watsonx.data Installer — Testing Instructions

> **For agents:** Read this ENTIRE file before writing or running any test. Then read `testing.config.json` for machine-readable constraints (workers, timeouts, selectors). **As of 2026-06-17 both this file and `testing.config.json` describe a repo with no code and no tests yet** — the config is still the unedited template. Treat every command below as a target to build toward, not a recipe that already works. Where a step is unverified it is marked `TODO:`.

<!-- Last verified: 2026-06-17 by [name/role] -->

## Reality Check (read first)

This repository currently contains exactly one tracked file — `README.md` — plus `.buildpilot/` scaffolding. There is:

- **No application code.** No scripts, no `cpd_vars.sh`, no installer entrypoint.
- **No test framework.** No `package.json`, no `pyproject.toml`, no `bats`, no `shellcheck` config, no Makefile.
- **No CI.** No `.github/workflows/`. Nothing runs on push.
- **No coverage tooling.** Nothing to report on.

Everything below is the **intended** testing posture for what this repo is going to become: an automated installer for IBM watsonx.data on an existing OpenShift cluster (orchestrating `cpd-cli`, `oc`, and a populated `cpd_vars.sh`). When the first installer code lands, the author of that PR owns updating this file with the *actual* commands and removing the `TODO:` markers. Do not let this doc drift into aspiration — if a section still says `TODO:`, it means nobody has proven it.

## Philosophy

This is infrastructure automation, not a web app. The bug classes that matter here, in priority order:

1. **Destructive / irreversible operations (P0).** Anything that mutates a live OpenShift cluster — creating namespaces, applying scoped resources, installing operators — must be guarded by a dry-run path and must be idempotent. A re-run after a partial failure must not leave the cluster in a worse state. An installer that half-applies and cannot resume is a P0.
2. **Wrong / silent prerequisite handling (P0).** Missing entitlement key, wrong storage class, insufficient cluster sizing, unreachable registry. These must fail **loudly and early** with an actionable message — never midway through a 90-minute install.
3. **Idempotency violations (P1).** Running the installer twice should converge, not duplicate or error.
4. **Incorrect variable population (P1).** `cpd_vars.sh` and equivalent config carry credentials, cluster coordinates, and version pins. A typo here is a long, expensive failure.

The expensive truth of this domain: the only *fully* faithful test is a real install against a real OpenShift cluster, which costs time (often 1–2+ hours) and money. So the strategy is to push as much verification as possible *left* — into fast, hermetic checks that mock `oc`/`cpd-cli` — and reserve real-cluster runs for gated, manual, deliberate validation.

## Test Pyramid (target vs. reality)

| Tier | What it covers | Count today | Target |
|------|----------------|-------------|--------|
| Static analysis | `shellcheck` on all `*.sh`, lint config files | **0** | All scripts, in CI |
| Unit | Pure functions: var validation, parsing, version comparison, prereq checks (with `oc`/`cpd-cli` stubbed) | **0** | Bulk of the suite |
| Integration | Full installer flow against **mocked** `oc`/`cpd-cli` (assert correct command sequence, dry-run output) | **0** | Per major install phase |
| E2E (real cluster) | Actual install against an ephemeral OpenShift cluster | **0** | Manual / nightly-gated, never on PR |

**Today the pyramid is empty.** Do not report a percentage — there is nothing to measure. When code lands, this is an inverted-pyramid risk: the temptation will be to "test by running it on a cluster." Resist it. Real-cluster runs are the tip, not the base.

## Recommended Test Stack (not yet installed)

No framework is chosen yet. The team must decide. Sensible defaults for a bash-based OpenShift installer:

- **`shellcheck`** — static analysis for every shell script. Cheapest, highest-ROI check; wire it first.
- **`bats-core`** — bash unit/integration tests. Stub `oc`/`cpd-cli` by putting fakes earlier on `PATH`.
- **`shfmt`** — formatting, so style review isn't manual.
- If any logic moves to Python/Go, pick `pytest` / `go test` respectively and add a row to the pyramid table.

`TODO:` Team to confirm language and framework when the first installer code is written, then replace this section with pinned versions.

## What to Run Before Merging

**There is no pre-merge command yet** — there is no code and no CI to match. Once the stack exists, this section must contain the *exact* copy-pasteable sequence a developer runs locally to match CI. Placeholder target:

```bash
# TODO: none of these work yet — no scripts, no shellcheck config, no bats tests exist.
shellcheck **/*.sh           # static analysis — must pass clean
shfmt -d .                   # formatting — no diff allowed
bats test/                   # unit + mocked-integration tests
```

CI runs **nothing** today (no `.github/workflows/`). When CI is added, document here exactly which subset of the above it enforces — it is almost always a strict subset of what the repo can run (real-cluster E2E will *not* be in CI). `TODO:` Add the CI subset list when the first workflow lands.

## Project Structure

Per `.buildpilot.json`: there is no `.buildpilot.json` and no sub-repos. This is a **single-repo, single-artifact** project today.

- **`/` (root)** — README only. Intended home of the installer scripts.

`TODO:` Update with the real layout once code exists (e.g. `scripts/`, `vars/`, `test/`).

## Testing Per Repo

### root (the installer)

- **Framework:** none installed. See "Recommended Test Stack" above.
- **Run:** nothing to run.
- **When adding tests:** use `bats-core` co-located under `test/`, name files `<script-under-test>.bats`. Stub external binaries (`oc`, `cpd-cli`, `ibmcloud`) via a fake-bin directory prepended to `PATH` so no test ever touches a real cluster. Every test that would mutate state must assert against the *captured command*, not execute it.

## E2E Testing

E2E here means **a real install against a real OpenShift cluster.** This is fundamentally different from browser E2E:

- **Tool:** none of the web E2E tools in `testing.config.json` (`playwright`/`cypress`/`selenium`) apply — this product has no UI. The `e2e` block in the config is template boilerplate; **ignore it** for this project. `TODO:` Either repurpose the `e2e` block to describe cluster-based E2E or mark it `none`.
- **Cost:** a full install is long (commonly 1–2+ hours) and consumes real cluster/entitlement resources. **Never** run it on a PR or in unattended CI without an explicit gate and a teardown guarantee.
- **Prerequisites:** a reachable OpenShift cluster (kubeadmin or equivalent), a valid IBM entitlement key, `oc` and `cpd-cli` on `PATH`, sufficient cluster sizing and a working storage class.
- **What to assert:** install completes; watsonx.data service reaches `Ready`/`Completed`; a re-run of the installer is a no-op (idempotency); uninstall/cleanup returns the cluster to baseline.

### E2E Patterns (when this exists)

1. **Always dry-run first.** Every install phase must support a no-op mode that prints the commands it *would* run. Integration tests assert on that output without touching a cluster.
2. **Idempotency is a test, not a hope.** Run the installer twice; the second run must converge with no errors and no duplicate resources.
3. **Fail fast on prereqs.** Prereq checks (entitlement, sizing, storage class, registry reachability) must run and fail *before* any mutation.
4. **Capture, don't execute, in mocked tiers.** Fake `oc`/`cpd-cli` should log invocations to a file the test inspects.
5. **Guarantee teardown.** Any real-cluster test must register cleanup that runs even on failure, or it will leak expensive resources.

## Coverage

**Not configured. No `.nycrc`, no `.coveragerc`, no `codecov.yml`, no coverage tooling.** There is no threshold because there is no code. Do not invent a number.

Where coverage will be *genuinely thin* even after the suite exists, by the nature of this domain:

- **The real-cluster install path** — exercised manually, rarely, never in CI. This is the highest-risk, least-covered surface.
- **IBM registry / entitlement auth** — depends on live IBM endpoints and secrets; only mockable at the command boundary.
- **Cluster-state-dependent branches** — behavior that differs by OpenShift version, storage backend, or node sizing is impractical to cover hermetically.
- **Partial-failure / resume logic** — hard to reproduce without injecting failures mid-install.

Say so plainly in PRs that touch these areas; don't pretend a green unit suite covers them.

## Manual / Exploratory Areas

Automation cannot reach these. They require a human with cluster access:

- **End-to-end install on a real OpenShift cluster** — the actual product promise. Validate against the IBM Software Hub 5.3.x docs flow (client workstation setup → collecting required info → scoped resources / shared components → `cpd_vars.sh` population → install).
- **Entitlement key & IBM container registry auth** — exercise with a real key against the live registry; verify a clear error on an invalid/expired key.
- **Cluster sizing & storage class variations** — try under-provisioned clusters and confirm the installer refuses early with an actionable message.
- **Resume after interruption** — kill an install partway and confirm a re-run recovers rather than corrupts.
- **Uninstall / cleanup** — confirm it returns the cluster to a known baseline.

`TODO:` Capture each manual pass (cluster version, storage backend, outcome) in a running log so the matrix of "what we've actually proven" is visible.

## Environment Setup

The README points at the IBM Software Hub 5.3.x documentation chain (client workstation, required info, scoped resources, `cpd_vars.sh`) as the manual install path the installer aims to automate. There is **no `docker-compose.yml`, no `.env.example`, no setup script** in this repo to reconcile against — so there is nothing to diverge yet.

`TODO:` When setup tooling lands, document here the exact env vars and external dependencies tests need (e.g. `OPENSHIFT_URL`, `IBM_ENTITLEMENT_KEY`, paths to `oc`/`cpd-cli`), and reconcile against `.env.example`. Flag any "you have to run X once" folklore as a `TODO:` to formalize or delete — do not let undocumented setup steps survive as oral tradition.

## Known Flaky Areas

**None catalogued — there are no tests to flake.** `testing.config.json → known_issues` is still the template placeholder.

`TODO:` Maintain a named flake list here as the suite grows — each entry: test name, known cause, tracking issue. Anticipated sources of flake in this domain (record them by name when they bite):

- Network-dependent prereq checks (registry reachability, entitlement validation) — mock at the command boundary in unit/integration tiers.
- Real-cluster timing — operator reconciliation and service readiness are eventually-consistent; poll with generous timeouts, never fixed sleeps.

## What Not To Test

- `oc`, `cpd-cli`, `ibmcloud`, and OpenShift/operator internals — assume the tools work; test *your* invocation of them.
- IBM-provided install behavior beyond the installer's own orchestration logic.
- Live IBM registry/entitlement service behavior — test only your handling of its success/error responses.
- Trivial pass-through wrappers with no logic.

## Two-Agent Testing Model

The standard BuildPilot split still applies, adapted to a no-UI product:

- **`qa-engineer`** — writes deterministic checks: `shellcheck`, `bats` unit tests for pure functions (var validation, version comparison, prereq logic), and mocked-integration tests asserting the correct `oc`/`cpd-cli` command sequence and dry-run output. No real cluster.
- **`qa-analyst`** — there is no browser to drive. For this product the analyst role is **real-cluster exploratory validation**: run the installer against a live OpenShift cluster, probe edge cases (bad entitlement, undersized cluster, interrupted run), and report findings. This requires cluster access and is inherently manual. The Playwright MCP browser tooling in the template does not apply here.

## Debugging Failing Tests

No tests exist, so no failure recipes are proven yet. Anticipated failure modes for this domain (fill in concrete recipes as they're hit):

1. **`oc`/`cpd-cli` not on `PATH`** — unit/integration tests must use fakes; if a test reaches for a real binary, the fake-bin `PATH` prepend is missing or mis-ordered.
2. **Test accidentally hits a real cluster** — a missing stub. No unit/integration test should ever require cluster connectivity; if one does, it's miscategorized as E2E.
3. **Stale env / leftover state** — a previous real-cluster run left resources behind, so an idempotency assertion fails on a "clean" run. Verify teardown ran; clean the namespace before re-running.

`TODO:` Replace these with two or three concrete, reproduced recipes once the suite exists.

## Ports

Not applicable. This installer has no long-running local services — it drives a remote OpenShift cluster via `oc`/`cpd-cli`. The `infrastructure` ports in `testing.config.json` (backend 8000 / frontend 3000) are template defaults and **do not apply**.

| Service | Port | Health Check |
|---------|------|--------------|
| — (no local services) | — | — |

## Open TODOs for the team

- [ ] Choose and pin a test stack (`shellcheck` + `bats-core` recommended); update "Recommended Test Stack".
- [ ] Add the first `shellcheck` gate and a `.github/workflows/` CI job; document the CI subset under "What to Run Before Merging".
- [ ] Fill in `testing.config.json` for real (it's still the unedited template) or delete the web-app blocks that don't apply.
- [ ] Establish the real-cluster E2E gate (manual/nightly) with guaranteed teardown.
- [ ] Start the manual-validation log (cluster version × storage backend × outcome).
- [ ] Replace every `TODO:` above with proven commands.
