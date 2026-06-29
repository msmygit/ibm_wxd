# BuildPilot Cycle 1 — Ticket #1

**Ticket:** #1 — Install watsonx.data, with all its pre-reqs successfully given an existing OpenShift cluster details
**Type:** feature (enhancement) · **Classification:** Medium → **Complex** (upgraded; greenfield epic)
**Branch:** `buildpilot/1` → base `main` · **PR:** https://github.com/msmygit/ibm_wxd/pull/2 (OPEN, `Refs #1`)
**Status:** completed · **GitHub issue #1:** OPEN, label `status:in-review`
**Follow-up:** #3 (BUG-1, Low) — COMPONENTS help/enforcement mismatch

## Delivered increment
Greenfield Rust CLI **`wxd-config`** (zero external crates): collects watsonx.data install
configuration interactively and non-interactively, validates it, and generates a
deterministic, source-able `cpd_vars.sh` — reconciled to the **authoritative IBM Software
Hub 5.4.0** `cpd_vars.sh` contract. No cluster contacted; fully hermetic.

Key properties: VERSION default `5.4.0` + PATCH_ID default `latest` (overridable); auth
choose-one (OCP_USERNAME+OCP_PASSWORD **or** OCP_TOKEN); k8s-namespace + URL + enum
validation; POSIX-safe quoting with verified injection safety; secret masking; derived
vars emitted (SERVER_ARGUMENTS, OLM_UTILS_IMAGE, PROJECT_INST_BR_SVC); generate→`--answers`
round-trip is byte-identical.

Deferred (documented out-of-scope): live prereq checks, scoped resources/shared components,
the actual install, checkpoint/resume, the Carbon UI, cluster provisioning, client-workstation
setup, backup/restore, S3, proxy, private registry, tethered projects.

## Pipeline Summary
| Phase | Agent | Verdict | Cycles |
|---|---|---|---|
| Workspace repair (pre-pipeline) | orchestrator | restored 4 stub config files + authored repos; commits fe573b7, 46edcdc | — |
| Classify | orchestrator | Complex (upgraded) | — |
| -1 Product Manager | product-manager | DONE (bounded epic → first increment, 12 ACs) | 1 |
| 0 UX Designer | — | SKIPPED (UI deferred out-of-scope) | — |
| 1 Developer | developer | DONE (Rust CLI) | — |
| 2 Code Review | codereview (+ 3 pr-review-toolkit) | LGTM | 4 review passes (cycle 1 LBTM → fixed) |
| 3 QA Engineer | qa-engineer | DONE (+5 gap tests) | 1 |
| 4 QA Analyst | qa-analyst | PASS (12/12 ACs, security clean) | 2 (re-run on 5.4.0 contract) |
| (mid) 5.4.0 retarget + contract reconciliation | developer + codereview + qa-analyst | LGTM / PASS | — |
| 5 Engineering Manager | engineering-manager | RELEASED (PR #2, issue #3, #1 in-review) | 1 |

Final tests: **128 pass** (115 unit + 13 integration), clippy clean, hermetic.

## Commits (buildpilot/1)
- 7948885 feat: add wxd-config Rust CLI for cpd_vars.sh generation
- c562867 fix: review cycle 1 — round-trip safety, secret echo, unknown-key warnings
- 8deec2c test: cover empty-env secret rejection, AC2 value-level, duplicate-key last-wins
- 281b8bc feat: retarget to IBM Software Hub 5.4.0 patch 1
- 6e90798 feat: reconcile SPEC to IBM Software Hub 5.4.0 cpd_vars.sh template

## Artifacts (.buildpilot/tickets/1/)
classification_note.md, product_context.md, questions_answers.md, implementation_notes.md,
review_report.md, test_plan.md, qa_report.md, release_report.md

## Notes / environment friction handled
- Workspace config was corrupted (34-byte `/login` stubs); recovered WORKTREE/SDLC/TESTING
  from session transcripts, authored missing `repos`.
- create.sh required a port placeholder + local oc/cpd-cli; adapted WORKTREE.md to gate on
  build-time toolchain (cargo/node) and ran bp-prep `--no-start`.
- GitHub MCP unauthenticated → all GitHub ops via `gh` CLI.
- Node broken on host (libllhttp mismatch) → could not run Playwright; IBM 5.4.x docs
  bot-blocked → variable contract supplied by the user and reconciled exactly.
- pm_upload.py absent → artifact upload skipped (artifacts remain in ticket dir).
