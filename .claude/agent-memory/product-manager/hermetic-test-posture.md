---
name: hermetic-test-posture
description: This product's ACs must be verifiable without a live OpenShift cluster — mock oc/cpd-cli
metadata:
  type: feedback
---

For the watsonx.data installer, acceptance criteria must be verifiable **hermetically**
— with no live OpenShift cluster and no `oc`/`cpd-cli` on PATH.

**Why:** Dev boxes have no cluster; a real install costs 1-2+ hours and real money
(TESTING.md). The explicit strategy is to push verification LEFT into fast checks that
mock `oc`/`cpd-cli`. Destructive cluster mutations (creating namespaces, applying
scoped resources, installing operators) are TESTING.md's P0 risk class.

**How to apply:** When bounding installer tickets, prefer first increments that are
pure logic (input validation, config generation, parsing, version comparison) testable
via `bats`/unit tests with stubbed binaries. Push anything requiring a real cluster to
"out of scope for this run." Ticket #1's first slice was the config-collection →
`cpd_vars.sh`-generation module for exactly this reason. See [[architecture-undecided]].
