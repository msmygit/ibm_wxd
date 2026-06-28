# ibm_wxd SDLC

<!-- Last verified: 2026-06-17 by [name/role] -->

> Reality check: this repo is greenfield. One commit (`Initial draft`), one branch
> (`main`), no `.github/`, no CI workflows, no CODEOWNERS, no PR template, no commit
> lint hooks. Most of the process below is the **intended** workflow, not an enforced
> one. Every gate that isn't backed by tooling is flagged. Treat the `TODO:`s as
> blocking — the orchestrator will route tickets on whatever this file claims, so a
> wrong claim here is a wrong transition downstream.

## Project Management Tool [REQUIRED]

We track work as **GitHub Issues** on the `msmygit/ibm_wxd` repository. The README
points contributors at `https://github.com/msmygit/ibm_wxd/issues/1` as the founding
ticket, and the only configured git remote is `origin → git@github.com:msmygit/ibm_wxd.git`.

**MCP server name**: `github` (the `github@claude-plugins-official` plugin, enabled in
`~/.claude/settings.json`).

> TODO: confirm GitHub Issues is the system of record. A Linear connector
> (`claude.ai Linear`) is also authenticated in this environment. If the team actually
> triages in Linear and only mirrors to GitHub, switch this section to Linear and set
> the MCP server name accordingly — this changes every state name and field name below.

## How We Work [REQUIRED]

Solo / small-team trunk-based development against `main`. There is no observable sprint
or cycle cadence — no scheduled release workflows, no milestones, no iteration labels in
the repo. Work is pulled, not planned into fixed-length iterations: an issue is opened,
picked up, shipped to `main`, closed.

BuildPilot should operate in **kanban / continuous-flow mode**: pick up the next eligible
open issue rather than waiting for an iteration boundary.

> TODO: confirm cadence. If the team runs Linear cycles or GitHub milestones, name the
> length (1-week? 2-week?) and switch the orchestrator to cycle mode. As written, there
> is no cadence to honor.

## Active Work [REQUIRED]

Fetch **open GitHub Issues** on `msmygit/ibm_wxd` assigned to the BuildPilot user. With
no labels or project board defined yet, the filter is simply: `state = open` AND
`assignee = <buildpilot account>`.

> TODO: confirm the BuildPilot service account / GitHub login it runs as, and whether
> work should be gated by a label (e.g. `ready`) or a Projects column rather than raw
> open-and-assigned. Without a label gate, BuildPilot will treat every freshly-filed
> open issue as actionable.

## Workflow Stages [REQUIRED]

GitHub Issues has no native multi-stage workflow — only `open` and `closed`. The stages
below are the **convention** we layer on top via labels. None of these labels exist in
the repo yet.

| State name in tool | What it means |
|---|---|
| open (no stage label) | Backlog — filed, not yet triaged or started |
| `status:todo` | Triaged, ready to pick up |
| `status:in-progress` | Being worked on; branch exists |
| `status:in-review` | PR open against `main`, awaiting human review |
| `status:blocked` | Waiting on a dependency or a clarification |
| closed | Shipped to `main` (or won't-fix — distinguish via close reason) |

> TODO: confirm whether the team wants real stage labels (create them in
> `.github/labels.yml`) or prefers a GitHub Projects board with columns. The
> orchestrator needs exact label/column names — the ones above are proposed, not real.

## State Transitions [REQUIRED]

Using the proposed labels above:

- **BuildPilot picks up ticket** → add `status:in-progress` (remove `status:todo`)
- **Coding complete, PR open** → add `status:in-review` (remove `status:in-progress`)
- **NEEDS_CLARITY (halted)** → leave state unchanged, add a comment on the issue stating
  the specific question; optionally add `status:blocked`
- **Pipeline failed** → leave state unchanged, add a comment with the failure; do not close

BuildPilot never closes issues. A human merges the PR and closes the issue (GitHub can
auto-close via `Closes #N` in the PR body — see PR Process).

## Ticket Structure [REQUIRED]

GitHub Issues field names:

- **Title field**: `title`
- **Description field**: `body`
- **Priority field**: none native. Express priority via labels, e.g.
  `priority:high` / `priority:medium` / `priority:low`. *(Not yet created.)*
- **Labels/tags field**: `labels`
- **Bug type**: label `bug`. *(GitHub's default `bug` label; not yet applied to any issue.)*

> TODO: confirm the priority and type taxonomy. No labels are defined in the repo today,
> so any label-based routing is aspirational until `.github/labels.yml` exists.

## Definition of Done [REQUIRED]

This product installs IBM watsonx.data onto an existing OpenShift cluster, so "done"
leans on real install verification, not just unit tests.

A ticket is **Done** when:

1. A PR is open against `main` and approved by a human reviewer (see Ownership TODO).
2. The change is exercised against a real or representative OpenShift cluster and the
   install / step it touches completes successfully — see `.buildpilot/TESTING.md`.
3. The PR is merged to `main` by a human.
4. The issue is closed (auto-closed by the merged PR's `Closes #N`, or manually).

BuildPilot does **not** close issues or merge PRs — humans do both.

**What CI enforces today: nothing.** There are no `.github/workflows/`, no lint, no
type-check, no test gate. Clauses 1–2 are currently honor-system.

> TODO: confirm the verification bar. This is installer tooling — define the minimum
> proof of a working install (which cluster, which steps must pass) that gates merge,
> and add a CI workflow that runs at least lint + the test suite described in
> `.buildpilot/TESTING.md` so "done" is machine-checkable instead of asserted.

## Additional Context [OPTIONAL]

- **Repository**: `msmygit/ibm_wxd`, default branch `main`, single remote `origin`.
- **Branch & commit conventions**: not yet established. One commit exists (`Initial
  draft`) and no `commitlint`/`.husky` hooks are present. Proposed rules until the team
  decides otherwise:
  - Branch per issue off `main`: `feat/<issue#>-slug`, `fix/<issue#>-slug`.
  - **Conventional Commits** for messages (`feat:`, `fix:`, `chore:`, `docs:`) so history
    stays greppable and a changelog can be generated later.
  - Trunk-based: short-lived branches, merge back to `main` frequently.
  > TODO: confirm branch naming and commit convention, then enforce with a `commitlint`
  > + Husky `commit-msg` hook so it's a rule, not a suggestion.
- **PR Process**: no `.github/PULL_REQUEST_TEMPLATE.md` exists. Proposed minimum: PR body
  links the issue (`Closes #N`), describes the change, and states how the install/change
  was verified. Required reviewers: see Ownership below. Required CI checks: none exist
  yet.
- **Ownership / reviewers**: **CODEOWNERS does not exist.** There is no automatic reviewer
  assignment and, with a solo history, effectively no second-pair-of-eyes gate. This is
  load-bearing: as written, nothing prevents self-merge to `main`.
  > TODO: name the design approver, the code reviewer, and the architecture tie-breaker.
  > Add a `.github/CODEOWNERS` and a branch-protection rule on `main` requiring ≥1 review,
  > so "reviewed" is enforced rather than nominal.
- **Anti-patterns (proposed, pending team confirmation)**:
  - No force-pushes to `main`.
  - No self-merging a PR once review is required.
  - No committing cluster credentials / secrets (e.g. `cpd_vars.sh` values) — this repo
    deals with OpenShift cluster details; keep them out of git history.
- **Release Process**: none defined. No `release*.yml`, no semantic-release, no
  `.changeset/`, no version tags. "Release" today means "merged to `main`."
  > TODO: confirm whether this tool needs versioned releases (git tags / GitHub Releases)
  > or ships continuously from `main`.
- **Dependency updates**: no `renovate.json` or `dependabot.yml` configured.
  > TODO: confirm whether to enable Dependabot/Renovate.

## Completion States [REQUIRED FOR CYCLE MODE]

GitHub Issues has a single terminal state. A ticket is fully done — and its worktree may
be pruned — when:

- closed (as completed)
- closed (as not planned / won't-fix)

> Note: GitHub exposes these as the issue `state_reason` (`completed` /
> `not_planned`). If the team adopts a `status:*` label scheme or a Projects board with a
> "Done" / "Released" column, list those exact names here too.
