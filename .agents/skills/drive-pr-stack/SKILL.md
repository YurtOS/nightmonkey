---
name: drive-pr-stack
description: Use when several dependent PRs (A→B→C, each based on the previous branch) must each reach a mergeable state in merge order, or the user says "drive the stack".
---

# Drive a PR Stack (outer loop)

Read `AGENTS.md` and `docs/agents/org.md` first. This skill adds only the
workflow below; repository and organization policy remain in those files.

## Overview

Run `drive-pr-to-mergeable` on each PR in dependency order. The inner runs
return ready reports and do not merge. Merge authority is defined only in
`docs/agents/org.md`, "Merge authority".

## Inputs

Ordered list: PR number, branch, base, worktree. Stack-specific context
(temporary fallbacks, follow-up PR plans, peer handoffs) is passed as args,
never baked in here. Baselines per PR.

## Steps

1. Inspect the available agent inventory and worktrees. Resolve overlapping
   ownership before starting work on any PR in the stack.
2. For each PR in order: finish its inner loop (fix the parent first).
3. **Merge-forward** the finished PR's branch into the next PR's branch (merge
   commit, no rebase, no force-push), push, then start the next inner loop.
4. A fix to an earlier PR after a later one started → merge-forward again
   through every child. After any merge-forward, the child's next review is
   against its base branch (`origin/<parent-branch>`), not its last
   reviewed SHA, so parent commits are not re-reviewed as the child's; re-record
   the child's baseline.
5. If one PR does not converge, stop the stack, seek an independent second
   opinion and report the remaining blockers to the user.

## Ready report

Report each PR's head/base SHAs, rounds, issues filed, local validation,
hosted state, deferred items and dependency order. Apply "Merge authority" in
`docs/agents/org.md` for any subsequent merge decision.

Before an authorized parent merge, retarget the child PR to the parent's base
so deleting the parent branch does not close the child. Never merge a child
into its parent branch. Use the new base for subsequent synchronization,
review and baseline comparisons. After the parent merges, merge the new base
into each child, push, rerun checks and review, and send a fresh ready report.
Obtain fresh authorization for each updated child head; stack authorization
does not carry across these head changes. Perform an agreed handoff only after
the authorized merge it depends on.
