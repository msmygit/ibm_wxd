---
name: product-file-location
description: Where PRODUCT.md and BuildPilot workspace config actually live in the ibm_wxd repo
metadata:
  type: reference
---

PRODUCT.md, SDLC.md, TESTING.md, and WORKTREE.md live in the **main repo** at
`/Users/mrkr/Documents/00_coderepos/ibm_wxd/.buildpilot/`, NOT inside per-ticket
worktrees. The orchestrator's pointer to a worktree-local PRODUCT.md path can be
wrong — ticket worktrees (`.worktree/<id>/.buildpilot/`) contain only `tickets/`.

**How to apply:** If a PRODUCT.md path under a worktree 404s, look in the main repo
`.buildpilot/` directory. The WORKTREE.md "Environment Variables" section is the
authoritative `cpd_vars.sh` variable contract for this product.
