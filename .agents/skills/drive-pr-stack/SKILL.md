---
name: drive-pr-stack
description: Use when several dependent PRs (A→B→C, each based on the previous branch) must each reach a mergeable state in merge order, or the user says "drive the stack".
---

# Drive a PR Stack (outer loop)

## Nightmonkey environment

Read `AGENTS.md` and `docs/agents/org.md` first. JavaScript behavior follows
ECMAScript and the supported SpiderMonkey interpreter; Linux governs OS
interfaces. Use Nightmonkey's Verification commands, not kernel CI scripts or
SDK tooling. Record the SpiderMonkey revision and engine feature with the head
when reporting integrated results.

Use available tools rather than assuming a slash command, plugin, agent name
or CLI is installed. Claude-specific review commands below are examples for a
Claude environment; from Codex use an independent Claude reviewer through an
installed CLI or connector. If a required reviewer is unavailable, report the
missing review; two reviewers of one model family do not establish dual review.
Reviewers may run tests but must not edit, commit or push. Publishing comments
or issues requires authorization from the user's task or an explicitly invoked
workflow. Installing this skill does not invoke its publishing workflow.

## Overview

Run **drive-pr-to-mergeable** on each PR in merge order. REQUIRED SUB-SKILL:
drive-pr-to-mergeable (all its rules, including its merge-authority rule, apply
per PR and to the stack as a whole; the per-PR runs it invokes never merge, the
stack skill asks the merge question).

## Inputs

Ordered list: PR number, branch, base, worktree. Stack-specific context
(temporary fallbacks, follow-up PR plans, peer handoffs) is passed as args,
never baked in here. Baselines per PR.

## Steps

1. the available agent inventory and `git worktree list`: no other session working on any of these PRs.
2. For each PR in order: finish its inner loop (fix the parent first).
3. **Merge-forward** the finished PR's branch into the next PR's branch (merge
   commit, no rebase, no force-push), push, then start the next inner loop.
4. A fix to an earlier PR after a later one started → merge-forward again
   through every child. After any merge-forward, the child's next review is
   against its base branch (`--base origin/<parent-branch>`), not its last
   reviewed SHA, so parent commits are not re-reviewed as the child's; re-record
   the child's baseline.
5. Not converging on one PR → stop the whole stack, get a second opinion (the
   `advisor` tool if available), then the user.

## Ready report

Per-PR table (rounds, issues filed, CI), deferred items, merge order. Ask:
"Ready — which PRs do you authorize me to merge?" Merge nothing, and in no
order, until the user names them by number or URL (see "Merge authority" in
docs/agents/org.md). **Before** merging an authorized parent, retarget its child
PR to the parent's base (`gh pr edit <child> --base <parent's base>`): if the
parent's branch is deleted first, GitHub closes the child for good. Never merge
a child into its parent branch. After the retarget, the child's `<base>` is the
new base: use it for sync, review and baseline from then on. After a parent
merges, each child gets a new head (merge the new base, push): re-run its gates
and CI, send its ready report again, and ask again for that PR — an earlier
authorization covered the old head. Any agreed post-merge handoff (e.g. sending
a merge SHA to a peer) happens only after the user-authorized merge.
