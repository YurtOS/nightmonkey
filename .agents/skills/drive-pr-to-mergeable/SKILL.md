---
name: drive-pr-to-mergeable
description: Use when asked to get one open PR to a mergeable state through repeated review-fix rounds, to "babysit", "drive", or "shepherd" a PR, or after opening a PR that needs dual Claude+Codex review before merge.
---

# Drive a PR to Mergeable

Read `AGENTS.md` and `docs/agents/org.md` first. This skill adds only the
workflow below; repository and organization policy remain in those files.

## Overview

Run a bounded review/fix loop on one PR, ending with a ready report. This
workflow does not merge. For merge authority, consult `docs/agents/org.md`,
"Merge authority"; do not infer it from the outcome of this loop.

## Inputs and preparation

1. Identify the PR, branch, base, worktree and maximum review rounds (default
   five). Derive missing facts from the live PR; ask only for unresolved inputs.
2. Inspect the available agent inventory, worktrees and open PRs for concurrent
   work. Resolve ownership before editing; wait if another writer owns the PR.
3. Refresh the remote refs and confirm the PR is open and unmerged. Read its
   description, linked issue, comments, reviews and inline threads before edits
   or pushes. Include existing findings in the first round.
4. Sync the base into the PR branch when needed, using a merge commit. Record
   the resulting pushed head and base SHAs. Establish a baseline for applicable
   Verification checks, recording failures by test name rather than count.
5. Choose review depth from the change's risks and state it in the round record.
   Reassess if the scope grows. Include test and documentation changes in review.

## Review large diffs by aspect

Apply `docs/agents/org.md`, "Review large diffs by aspect", to the whole PR
against its base on every round. Derive aspect acceptance points from the issue
and PR risks; supply the same head/base SHAs, summary, previous findings and
relevant evidence to each reviewer. Nightmonkey examples include analysis and
guards, generic fallback and exceptions, GC lifetime, and snapshot/engine ABI.
Carry aspect labels into the deduplicated round comment below.

## Loop

1. **Review the recorded head.** Request independent Claude and Codex reviews
   using tools available in the session. Reviewers may run checks and probes but
   must not edit, commit or push. Give each the exact diff, acceptance criteria,
   earlier findings and their dispositions. Supply local context files if a
   reviewer cannot read the PR conversation. Report an unavailable required
   reviewer as missing coverage; do not substitute two of one model family.
2. **Refresh the conversation.** Read new comments, review bodies and inline
   threads every round. Treat human and additional reviewer findings the same
   way as the two requested reviews.
3. **Record one round comment.** Include round number, head/base SHAs and one
   deduplicated findings table with severity, source, aspect where applicable,
   evidence and disposition. Reply to inline findings in their original thread;
   group dispositions of review-body findings in one reply.
4. **Verify each finding before changing code.** Reproduce with an appropriate
   probe or regression. Fix verified findings within scope; track real findings
   outside scope in linked issues. Rebut disproved findings with evidence.
   An unreproduced finding is not disproved: record what was tried, and escalate
   unresolved high-severity findings to the user or a tracking issue. Derive
   scope from the approved design, otherwise the PR and linked issue; ask if
   the contract remains unclear.
5. **Validate and push follow-up commits.** Run applicable Verification checks
   and compare failures with the baseline. Refresh PR state before every push
   and confirm it remains open and unmerged. Record the new head SHA.
6. **Review every change after the reviewed head.** Give reviewers the delta
   since their last reviewed SHA plus previous findings. A fresh reviewer also
   gets the full PR context. A delta under roughly 50 changed lines may use one
   reviewer; larger deltas need both. Count each delta review as a round. Clean
   base merges need no delta review, but conflict resolutions do. Tests or CI
   passing do not substitute for delta review.
7. **Stop or repeat.** With only nits left and unresolved high-severity findings
   escalated, proceed to final verification. Otherwise repeat within the round
   limit. At the limit, stop patching, seek a second opinion and report the
   remaining findings and trend to the user; further rounds need their go.
   Reviewer errors are not convergence evidence: retry once, then report missing
   coverage without counting the failed review as a completed round.

## Ready report

Refresh the PR conversation and confirm every finding has a recorded disposition.
Confirm all changes since review have received the delta review above. Complete
applicable Verification checks and inspect hosted checks at the final head.
Report a failing required check as blocked, including evidence if reproduced
on the base. Missing required review or verification is incomplete coverage,
not readiness.

When these gates pass, mark the draft ready and post a report with head/base
SHAs, review rounds, findings fixed or tracked, local validation, hosted state,
skips and deferred items. End this workflow at that report. When invoked by
`drive-pr-stack`, return the report to the stack workflow.

## Tool example

If installed, `codex review --base <base-sha>` can provide the Codex review;
use an available Claude reviewer for the other review. This is an invocation
example, not a required CLI or a replacement for the context and gates above.
