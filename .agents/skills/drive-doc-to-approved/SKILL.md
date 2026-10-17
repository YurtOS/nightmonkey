---
name: drive-doc-to-approved
description: Use when a spec or implementation plan document needs independent agent review rounds until reviewers approve it, before the user's approval or before executing it.
---

# Drive a Spec/Plan to Reviewer Approval

Read `AGENTS.md` and `docs/agents/org.md` first. This skill adds only the
workflow below; repository and organization policy remain in those files.

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

1. **Review** at the committed SHA with independent Claude and Codex reviewers.
   Ask them to check requirements, contradictions, failure modes, applicable
   security and domain rules from `AGENTS.md`, testability, proportionality and
   architectural fit. Give returning reviewers the delta and their findings;
   give fresh reviewers the full document and that delta. Reviewers are
   read-only and must not edit, commit or push.
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
   Not converging after ~3 rounds → stop, seek an independent second opinion
   and report the remaining disagreements to the user.

## Output

Approved SHA, rounds, what each round changed, issues filed.

- Invoked by a calling skill (e.g. issue-to-mergeable-pr) → hand this back to
  it; it owns the user gates. Do not ask the user for approval yourself.
- Standalone → report it to the user and ask them to approve the document at
  that SHA before it is used.
