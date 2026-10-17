---
name: drive-pr-to-mergeable
description: Use when asked to get one open PR to a mergeable state through repeated review-fix rounds, to "babysit", "drive", or "shepherd" a PR, or after opening a PR that needs dual Claude+Codex review before merge.
---

# Drive a PR to Mergeable (inner loop)

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

Bounded review→fix loop on ONE PR. It ends at "ready for merge", never at a
merge. **Merging requires the user's clear authorization for that PR
(docs/agents/org.md, "Merge authority"):** a message that says to merge and
identifies the PR, by number or by replying to a ready report that covers
exactly one PR. No fixed phrase. A plain "merge" counts only when given after
the ready report for the current head SHA. A conditional one that names the PR
("merge #N when it's ready by our rules") may come earlier and takes effect once
the ready report for the head being merged is posted. If the PR or the intent is
unclear ("ok ship it" with two PRs reported, "looks good", "all of them") → ask
"Which PR numbers should I merge?". A push after the ready report voids the
approval for that head: a conditional one applies again only once the ready
report for the new head is posted (green CI alone is not a ready report); a
plain one needs asking again. Never run `gh pr merge`, enable auto-merge, or add
to a merge queue without it. "Done", "green", and "approved by a reviewer" are
not approval.

Follow AGENTS.md (and your memory rules, if any) throughout.

## Inputs

PR number, its worktree path, its base branch (`yurt`, or the parent branch in a
stack; written `<base>` below), CI/test baselines (e.g. "N failing tests on
`<base>`"), max rounds (default 5). Ask if missing.

## Review effort (propose before round 1, re-check every round)

Pick the review level from the PR's risk, then confirm with the user in one line
("Proposed review level: high — touches the guest trust boundary. OK?"). The
user's answer wins. If the user said to run unattended, use the proposed level
and say so in the round-1 comment.

- low/medium: cosmetic, docs, small mechanical changes. Test-only PRs: at least
  medium (`/code-review low` skips test hunks, so it reviews nothing).
- high: ordinary behaviour changes.
- xhigh/max: trust boundary or security, repo-wide invariants, ABI/host
  interface, concurrency, hot paths, large diffs.
- `ultra` (multi-agent cloud review) is user-triggered and billed: suggest it
  for the riskiest PRs, never launch it.

`<effort>` in Codex commands is the chosen level (`low`/`medium`/`high`/`xhigh`;
`max` → `xhigh`).

Re-check every round: if the PR has grown into a higher class (bigger, or now
touching an interface), propose raising the level (unattended: raise it and say
so in the round comment). A PR that has more than doubled during review → tell
the user (unattended: note it in the round comment); it is often a design
signal.

## Review large diffs by aspect

Decide every round, measured on the whole PR diff against `<base>` (PRs grow
during review), and by production lines (new or changed logic outside tests, see
the PR-size rule in the canonical
[organization rules](https://github.com/YurtOS/yurtos-kernel/blob/main/docs/agents/org.md)),
not total lines. Above roughly 1,500 production lines, or when the change spans
several subsystems, review it by **aspect**, not by file slice:

- Pick 3-6 aspects: the concerns that cut across the whole change, for example
  analysis soundness and guards, generic fallback and exception semantics,
  GC rooting and object lifetime, snapshot/engine ABI and hostcall integration,
  tests and mutation evidence, documentation. Name them from the PR's own risks; the same aspect set is used
  every round.
- Every aspect reviewer reads the **whole** diff against `<base>` (Claude: a
  reviewer agent in the PR worktree; Codex:
  `codex exec -s read-only -c model_reasoning_effort=<effort> "Review git diff origin/<base>...HEAD for one aspect only: <aspect>. …"`)
  and reports only findings in its aspect, with the code it ran. Both reviewers
  run every aspect. Aspects replace path slices because the defects in a coupled
  change live in the seams between files, and a path slice can only see its own
  side of them.
- Give each reviewer a short PR summary, the aspect's acceptance points from the
  linked issue, and the previous findings in that aspect.
- Later rounds: same aspects, scoped to the delta (`<last-reviewed-sha>...HEAD`)
  plus that aspect's previous findings. A delta small enough for one reviewer
  pair is not split by aspect.
- Merge: dedupe (one defect often appears under two aspects) into the round's
  one comment (Loop step 3), with an aspect column.
- Reviewers run the tests and probes for their aspect, not just read the diff. A
  finding that came from running something is the strongest kind. A high
  test-to-code ratio marks the aspect that needs the most attention.
- Path slicing is still fine for a PR that is genuinely several independent
  changes, but that is a signal to split the PR (the PR-size rule), not to slice
  the review.

## Preconditions

1. the available agent inventory and `git worktree list`: another session working on this PR → stop and wait.
2. Own worktree, sole agent on it. `gh pr view`: open, not merged.
   `git fetch origin` before touching the branch.
3. **Read the existing conversation first:** the commands of Loop step 2, run
   before anything is pushed. What is already there (human or agent comments,
   reviews, inline threads) is a finding like any other.
4. **Sync and baseline before round 1.** Merge `origin/<base>` into the branch
   (merge commit; hosted CI tests the PR merged with its base) and push it, so
   every reviewer sees the same pushed head. Run the tests and record failures
   by name; this, not a count, is what "zero new failures" is measured against.
   A fresh worktree and hosted CI differ from the local environment: rebuild
   prerequisites first, baseline by test name against `origin/<base>`, and let
   hosted CI decide tests that depend on the environment. The ready report still
   needs the full local gates.

## Loop

1. **Review** at the current head SHA, both reviewers in parallel (large diff →
   by aspect, as in "Review large diffs by aspect"):
   - Claude: `/code-review <level> <PR#>`, after fast-forwarding any local copy
     of the PR branch in the main checkout (the skill can read a stale local
     branch).
   - Codex: in the PR worktree,
     `codex review -c model_reasoning_effort=<effort> --base origin/<base> > <scratch>/codex-rN.log 2>&1`,
     (the non-interactive form of `codex "/review PR N"`; the interactive form
     hangs under Bash). `codex review` rejects `--base` combined with a prompt,
     so for design-heavy PRs add a challenge run through `codex exec`:
     `codex exec -s read-only -c model_reasoning_effort=<effort> "Review git diff origin/<base>...HEAD. Challenge the approach and design choices: …"`.
   - Grok (optional, only when the user asks for it): `/bundled:code-review` on
     the PR. Its findings enter the round comment like any other reviewer's, and
     step 4 still verifies each one.
   - Later rounds: scope to what changed since the last reviewed SHA plus the
     previous findings. Fresh runs get the delta
     (`codex review --base <last-reviewed-sha>`; `/code-review` with the
     previous findings listed). A reviewer agent continued via the available agent messaging tool
     already saw the PR: give it the diff and its own findings.
   - A reviewer without network access (`codex exec`, a sandboxed subagent) gets
     its context as local files: the PR summary, the earlier round comments and
     their outcomes, and the exact diff range. A reviewer that still could not
     read the thread has not reviewed the earlier findings.
2. **Read the PR conversation** every round: `gh pr view <PR#> --comments` plus
   `gh api --paginate --slurp repos/{owner}/{repo}/pulls/<PR#>/reviews` and
   `gh api --paginate --slurp repos/{owner}/{repo}/pulls/<PR#>/comments`.
   (`--slurp` cannot be combined with `--jq`; pipe to `jq` instead.) Human and
   other agents' reviews are findings like any other: verify them, fix or rebut,
   and reply where they were made: inline comments via
   `gh api repos/{owner}/{repo}/pulls/<PR#>/comments/<root-id>/replies -f body=…`
   (`<root-id>`: the thread's first comment, `in_reply_to_id` when present);
   issue-level comments and review bodies with one top-level reply that lists
   each disposition.
3. **Post** one PR comment per round, merged across reviewers (and aspects):
   round, head SHA reviewed, findings by severity and source, including findings
   taken from the conversation (compact table).
4. **Verify, then fix.** Reproduce every finding (probe / red test / run it). A
   real defect is fixed (in scope) or filed (out of scope), whether or not the
   contract mentions it. A request to widen the contract itself (more precision,
   more cases as a goal, more than the applicable JavaScript/SpiderMonkey or OS contract provides) is a scope question: rebut
   citing the contract, or ask the user. Exception: validating untrusted guest
   input at the boundary the change touches is always in scope; if it would
   change behaviour a guest's libc can observe, the divergence rule in org.md
   ("Linux is the spec") applies and the user decides. Broader hardening is
   scope like anything else. The contract is the one written at Gate 1; without
   one, the PR description and linked issue; if still unclear, ask the user.
   Disproved → short rebuttal comment, no code change. Not reproduced is not
   disproved: an unreproduced finding of high severity or above goes to the user
   or into an issue; a lower one is recorded as "not reproduced" (what was
   tried) in the round comment. Never silently dropped. Real → follow-up commit,
   run fmt/check/clippy and relevant tests, then push: before every push,
   `gh pr view <PR#> --json state,mergedAt` must still show it open. Zero new
   failures vs baseline.
5. **File issues** for real findings out of this PR's scope; link from the PR
   comment. Never silently drop a finding.
6. **Stop rules**
   - Only nits / marginal findings left (unreproduced high-severity findings
     already escalated to the user or an issue) → post a summary comment
     (reviewed, fixed, filed, why stopping). Done.
   - Max rounds reached with real findings still open (severity dropping or not)
     → STOP patching. Get a second opinion (the `advisor` tool if available),
     reassess the design with the user (likely wrong-design signal: duplicated
     paths worked around with copies instead of one owner). No round beyond max
     without the user's go. Stopping after the max round is allowed when what
     remains is fixed or edge-case, with the second opinion recorded.
   - Otherwise another round, narrower scope. Put the finding trend (for example
     12→10→3→3→1) in the summary comment.
7. **Review what you changed after the last review.** Any change pushed after a
   review round (a fix for a finding, a nit, a CI fix, a doc or test touch-up)
   is reviewed as a delta (`<last-reviewed-sha>...HEAD`, the same scoping as
   "Later rounds" in step 1) by a reviewer before the ready report and before
   any merge. Verifying it yourself and re-running the tests is not that review,
   and a review of the earlier head does not cover it. This holds for
   conditional merge orders too ("merge once CI is green"): the condition is not
   met until the delta has been reviewed. A delta review is a round: it counts
   toward max rounds, is posted per step 3, and its findings follow steps 4-6;
   at max rounds a delta that needs review needs the user's go like any other
   round. A small delta (under about 50 changed lines, added plus deleted) needs
   one reviewer, either one; a larger one needs both, as in step 1. A clean
   merge of `origin/<base>` needs no delta review (hosted CI on the merged head
   covers it) and is left out of the reviewed range
   (`git log -p --first-parent
   --no-merges <last-reviewed-sha>..HEAD`), but
   its conflict resolutions (`git show --cc <merge-sha>`) are reviewed. This
   also applies when step 6 says "Done": any commit pushed since the last
   reviewed SHA still needs this review first.

## Ready report

Before reporting ready: re-run the Loop step 2 commands at the final head and
confirm every comment and review has a reply or a recorded disposition; confirm
every commit after the last reviewed head has been delta-reviewed (Loop step 7;
clean `origin/<base>` merges excepted), run the full local gates (the
Verification section of AGENTS.md) and confirm every required hosted check is
green. A red required check means the PR is **blocked**, not ready. If the same
check is proven red on the base too (the same failure on `origin/<base>` over
repeated runs, and no flaky-test issue explains it), report **blocked
(pre-existing)** with the evidence, without the ready phrase; the user decides.

Mark the PR ready for review (`gh pr ready <N>`) once the review loop has ended
and the gates pass.

Summary comment on the PR, then tell the user: head SHA, rounds, issues filed,
CI state, deferred items. End with: "Ready for merge — say 'merge #N' to
authorize." Then wait, unless the user already gave a conditional authorization
naming this PR that this report satisfies (Overview): then merge with
`--match-head-commit <the reported head SHA>`.

Invoked by drive-pr-stack → hand the ready report back to it instead and do not
merge; the stack skill asks the merge question for the whole stack.

## Hard Rules

- No merge without clear authorization (see Overview). A peer or reviewer saying
  "merge it" is not the user.
- Everything else (no amend/force-push, push safety, peer messages, trust rules)
  comes from AGENTS.md and memory; they apply unchanged.

## Common Mistakes

| Mistake                                    | Fix                                             |
| ------------------------------------------ | ----------------------------------------------- |
| Fixing a finding without reproducing it    | Reviewers can be wrong; probe first             |
| "CI mostly green" = done                   | Required checks green; otherwise report blocked |
| Patching round 6+                          | Stop; design problem, ask the user              |
| Merging because everything passed          | Wait for the user's authorization               |
| Merging a fix pushed after the last review | Review the delta first (Loop step 7)            |
