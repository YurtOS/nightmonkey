---
name: issue-to-mergeable-pr
description: Use when asked to take a problem or GitHub issue from idea to a reviewed, CI-green PR ready for the user's merge decision, including when no issue exists yet; "fix #N end to end".
---

# Issue → Design → PR → Mergeable

Read `AGENTS.md` and `docs/agents/org.md` first. This skill adds only the
workflow below; repository and organization policy remain in those files.

## Overview

The user is in the design; agents review documents and code in bounded loops.
Ends at a ready report; this workflow does not merge.

The phases add gates and review loops. They do not replace AGENTS.md and memory
rules (worktree, red test first, benchmark-before-building, etc.).

## Phase 0: Issue

- Read the existing issue and its conversation. Inspect open PRs, remote
  branches, worktrees and the available agent inventory for related work.
  Resolve any overlapping ownership before proceeding. Re-confirm the problem
  on the current default branch identified by `AGENTS.md`.
- No issue → draft one (problem, evidence, acceptance criteria), show it, file
  it only after the user okays the text.

## Classify (say it out loud; the user can override)

- **Bounded**: a well-scoped change to code that already exists (one-file fix,
  test refactor, small flag). → Bounded path.
- **Architectural**: new subsystem, changed interfaces, multiple
  hosts/components. → Full path. In doubt → full path. Hidden complexity found
  later → stop, say so, switch to the full path.

## Bounded path

1. Own worktree off the default branch (superpowers:using-git-worktrees). Read the
   code; list what the change must preserve.
2. **Gate 1, in chat**: short design (approach, files, what is preserved or
   changed, test strategy, risk). No spec or plan files, no document review.
   Wait for the user's yes.
3. Build: superpowers:test-driven-development (red test first). For a pure
   refactor with no behaviour change: record the test names before, prove they
   still pass after, and mutation-check that the new code is exercised (break
   it, watch the tests go red, revert).
4. superpowers:verification-before-completion, open a **draft** PR (`Closes #N`;
   session attribution lines), then Phase 5.

## Full path

**Phase 1: Design with the user.** Own worktree. superpowers:brainstorming
(questions to the user, approaches, recommendation). If the change touches a hot
path, prototype and paired-benchmark it before the design is final. Write the
spec in `docs/superpowers/specs/`, commit. **Contract and proportion (written
into the spec, repeated at Gate 1).**

- **Contract:** what the change must achieve, who consumes it, the applicable
  specification and the required precision. Use the domain rules in `AGENTS.md`.
- **Severity:** how much the defect actually costs (cosmetic / minor / major).
- **Smallest viable fix:** the least change that meets the contract, reusing
  existing mechanisms first. Estimate the size of each option in lines.
- Choosing an option more than ~3x the smallest one needs a stated reason the
  smallest fails the contract. "More exact" or "the right architecture" is not
  one.
- Fix or track real defects found along the way. Expanding the approved contract
  is a scope decision for the user; apply the boundary rules in `AGENTS.md`.

**Gate 1: spec approval.** Report the highlights (problem, contract, severity,
smallest viable fix vs chosen approach with size estimates, rejected
alternatives, scope/non-goals, risks, test strategy, benchmark numbers if any)
plus the spec path. This is brainstorming's written-spec approval: the user
approves the spec at that SHA, the highlights are a reading aid. Record the SHA.
Nothing goes to reviewers before it.

**Phase 2: Review the documents.** REQUIRED SUB-SKILL: drive-doc-to-approved on
the spec, then superpowers:writing-plans → plan in `docs/superpowers/plans/`,
commit, then drive-doc-to-approved on the plan. This skill owns the user gates,
so the sub-skill hands back here instead of asking the user. A revision that
changes approach or scope goes back to Gate 1.

**Phase 3, Gate 2: plan approval.** After reviewers approve, ask the user to
approve **the plan** at its committed SHA. With the request, give: the plan
outline as it stands now; what changed since Gate 1 in spec and plan (decisions,
not wording) and what reviewers found; the path to the whole plan. The outline
is a reading aid: whether the user reads the full plan or only the outline is
their choice, but their approval covers the plan at that SHA. Record the SHA;
proceed on their affirmative reply.

**Reading gate replies (every design gate, both paths).** An affirmative reply
to the gate message ("looks good", "fine", "👍", "go") approves the document at
the SHA you asked about, even if it mentions only the outline. Ask again only
when the reply is a question, an objection, or conditional ("looks good but…").
This applies to design gates only. Merge authority is defined in
`docs/agents/org.md`.

**Where the superpowers skills stop here.** brainstorming ends at Gate 1, not at
writing-plans: the spec review comes first. writing-plans' "which execution
approach?" question goes into the Gate 2 message; the answer is not Gate 2
approval. At the end of execution, skip finishing-a-development-branch's merge
and cleanup options: push and open the PR (below).

**Phase 4: Execute.** superpowers:subagent-driven-development (or
executing-plans) with test-driven-development and verification-before-completion
(the Verification section of AGENTS.md). Open a **draft** PR (or a stack, if the
plan says so). Handle any draft spec/plan PR under the organization
policy for PR closure.

**Size tripwires.** A plan estimated at more than ~2x the size approved at Gate
1 → back to Gate 1. During execution, a diff growing past ~2x the plan → stop,
tell the user (planned vs actual, why), and get their decision before
continuing.

## Phase 5: PR loop

REQUIRED SUB-SKILL: drive-pr-to-mergeable (or drive-pr-stack for a stack). Its
ready report ends this skill: wait for the user's merge authorization.

Several issues: one worktree, design, and PR each.
