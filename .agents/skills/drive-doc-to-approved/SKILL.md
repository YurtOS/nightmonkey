---
name: drive-doc-to-approved
description: Use when a spec or implementation plan document needs independent agent review rounds until reviewers approve it, before the user's approval or before executing it.
---

# Drive a Spec/Plan to Reviewer Approval

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

Document analogue of **drive-pr-to-mergeable**: same bounded review→fix loop,
but the artifact is a spec or plan, and the fix is a document revision. Ends at
"reviewers approve"; the user's approval is a separate gate owned by the calling
skill.

## Pointing reviewers at the latest version

Every round, give each reviewer the repo-local path
(`docs/superpowers/specs|plans/...`) in the worktree, plus the commit SHA it was
committed at. Commit each revision first, so "latest" is unambiguous. Reviewers
read that exact version (`git show <sha>:<path>`), not the working tree or a
pasted copy, and are told they are read-only: no edits, no commits. Alternative
when asked: a draft PR titled "[DRAFT SPEC]"/"[DRAFT PLAN]", never marked ready,
closed or converted once the plan is executed.

## Loop

1. **Review** at the committed SHA. Round 1: full review by a Claude agent AND
   an independent Codex reviewer, each told what to check: requirements met,
   contradictions, missing cases/failure modes, security (validate snapshot/hostcall input and preserve bounds,
   GC rooting and lifetime invariants), testability (functional tests, red test
   first), scope creep, proportionality (the smallest design that meets the
   stated contract? realistic size estimates? precision or cases beyond the
   contract, or beyond the applicable JavaScript/SpiderMonkey or OS contract, are scope creep), fit with the
   existing architecture. Later rounds: the same reviewer agents (continued via
   the available agent messaging tool) get the diff since their last SHA plus their own findings; a
   fresh reviewer gets the whole document plus that diff.
2. **Record** each review: round, SHA, findings by severity (comment on the
   issue/draft PR, or a review log next to the doc). Each round also read new
   comments on the issue (or draft PR) the document belongs to: human and agent
   comments there are findings too.
3. **Verify, then revise.** Check each finding against the codebase/docs; bogus
   → short rebuttal, no change. Real → revise, commit (new SHA). Out-of-scope
   real findings → file an issue.
4. **Reviewer trouble is not non-convergence.** A reviewer that errors or is
   unavailable → retry once, then tell the user; don't count the round. A
   reviewer re-raising a finding already rebutted → reply with the earlier
   rebuttal; it is not a new finding.
5. **Stop**: both reviewers approve with no verified findings above nit level.
   Not converging after ~3 rounds → stop, get a second opinion (the `advisor`
   tool if available), tell the user (likely the spec is wrong, not the
   wording).

## Output

Approved SHA, rounds, what each round changed, issues filed.

- Invoked by a calling skill (e.g. issue-to-mergeable-pr) → hand this back to
  it; it owns the user gates. Do not ask the user for approval yourself.
- Standalone → report it to the user and ask them to approve the document at
  that SHA before it is used.
