---
name: architecture-undecided
description: Unresolved Rust-vs-bash implementation language conflict in the watsonx.data installer
metadata:
  type: project
---

The watsonx.data Easy Installer has an unresolved implementation-language conflict
as of 2026-06-28: PRODUCT.md "What We Do" specifies a **Rust core + IBM Carbon UI**,
while TESTING.md and WORKTREE.md describe a **bash-based installer** (recommend
`shellcheck` + `bats-core`) orchestrating `cpd-cli`/`oc`. WORKTREE.md gates worktree
creation on BOTH cargo/rustc AND node/npm. No product code exists yet to disambiguate.

**Why:** Greenfield repo; the workspace config docs were authored independently and
were not reconciled.

**How to apply:** When writing ACs, keep them language-agnostic (assert clean build +
passing hermetic test suite regardless of Rust/Node/shell). Flag the choice to the
developer/Technical Architect as a non-blocking decision to document in the PR,
biased toward the long-term Rust-core direction unless there's a stated reason to
diverge. See [[hermetic-test-posture]].
