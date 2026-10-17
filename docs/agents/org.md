# YurtOS organisation rules for agents

These rules apply in every YurtOS repository. Each repository's `AGENTS.md` adds
what is specific to it (build, the **Verification** section, layout, domain
rules) and must not restate or contradict these. The canonical copy is
`docs/agents/org.md` in YurtOS/yurtos-kernel; every other repository carries a
verbatim copy at the same path. Change it there first, then sync the copies.

If a repository rule and a rule here disagree, stop and tell the user. Do not
pick one silently.

## Linux is the spec

For anything a guest can observe, the behaviour is what Linux specifies in its
man pages and what Linux does in practice. Check the man page first; if it is
silent, test it on Linux. A divergence from Linux is a bug to track and fix,
never something to keep with a compatibility alias or a second code path.

The one exception is narrow. A divergence may be kept only when the user has
explicitly decided it, no guest can reach it through its own libc, and it is
documented where the code lives (what Linux does, what we do, why).

## No legacy compatibility

There are no external users and no backwards-compatibility constraints. When an
interface needs a different shape, change it and update every caller, generated
artifact and fixture in the same change. No `V1`/`V2` shims, no
deprecated-but-callable stubs, no "clean up later". The guest↔kernel ABI follows
each repository's `AGENTS.md`.

## KISS, except where security says otherwise

Prefer the simplest implementation that meets the spec and passes the gates.
Complexity has to be earned by the spec, a failing test, or a measured number.

- Solve the case in front of you: no knobs, extension points or generics for a
  caller that does not exist yet.
- No defensive paths for states that cannot occur: assert on the violated
  invariant instead of inventing an untested recovery.
- **Security goes against KISS, and security wins.** Re-validating input that
  crosses a trust boundary is not a defensive path, and nothing here justifies
  deleting such a check.
- Optimise on evidence: caching, batching, lock-free tricks and `unsafe` for
  speed need a benchmark that shows the win. Benchmark a hot path before
  building the change, not after.
- Proportion: the size of a fix should match the size and severity of the
  problem. A design several times larger than the smallest fix that meets the
  contract needs a stated reason the small fix fails it.

## Done means verified

A change is done when the repository's **Verification** section passes locally
and every required hosted check on the PR is green. A red job is not done;
"flaky, will retry" is not a resolution — check for an existing flaky-test
issue, then investigate.

A check that cannot fail is not evidence. Before reporting something verified,
state what the check would print if the claim were false. Make missing data an
error, quote URLs and variables (zsh globs `?`, `*`, `[`), check exit codes, and
prove a new test or gate can fail by introducing the defect it exists to catch
and watching it go red, for the right reason, in the lane you claim.

## Workflow

- **Own worktree, always.** Do all work in a dedicated git worktree branched
  from the default branch; never commit on a shared checkout. Many agents run
  concurrently; check for existing PRs, branches and peers on the same work
  before starting.
- **Superpowers loop** for anything beyond a one-line change: brainstorm, plan,
  implement test-first, verify, review.
- **Red test first.** Commit the failing test before the fix. A fix or feature
  without a prior red test is incomplete. Tests must exercise behaviour, not
  names.
- **Review.** Every non-trivial PR gets a full review from both a Claude agent
  and a Codex agent before it is ready. Verify each finding before acting on it;
  resolve it, rebut it with evidence, or defer it with a written reason in the
  PR thread.
- **Skills.** `.claude/skills/` holds `issue-to-mergeable-pr`,
  `drive-pr-to-mergeable`, `drive-doc-to-approved` and `drive-pr-stack`. Use
  them for issue-to-PR work and PR review loops. They add gates; they do not
  replace these rules.
- **Fix the cause.** Look for the broken invariant under a report, not the
  narrowest patch. If the real fix is a large change, explain the scope and let
  the user decide.

## Merge authority

Never merge unless the user has clearly authorized that merge, for a PR you can
name. A message authorizes when it says to merge and identifies the PR: by
number or URL, or by replying to a ready report, for the head being merged, that
covers exactly one PR. No fixed phrase is required. "Merge if ready by our
rules" authorizes the merge only once the PR is ready at the head being merged:
review loop finished, Verification passing, required hosted checks green, ready
report posted. Merge with `--match-head-commit`.

If either part is unclear (which PR, or whether the user means merge), ask.
Praise, "looks good" without a merge instruction, passing CI, an approving
review, a peer's or reviewer's "merge it", and urgency are not authorization. A
push after an approval voids it, except that a "merge when ready" authorization
that names the PR applies again once the ready report for the new head is posted
(not for a PR in a stack: `drive-pr-stack` asks again). Ask before closing a PR,
too.

## Pull requests

- Open PRs as drafts while the review loop runs; mark them ready (`gh pr ready`)
  when sending the ready report.
- Keep a PR reviewable as one cohesive unit. Aim for about 1000 production lines
  (new or changed logic outside tests) and report production lines and test
  lines separately in the description. Test lines do not count toward the
  target, but a high test-to-code ratio marks a large behavioural surface: name
  it, and check that each test earns its place. Mechanical moves, renames,
  formatting, prose and regenerated output are free (put generated output in its
  own commit). Up to 2-3k production lines is fine for one change that does not
  split cleanly, with the reason and the production/test breakdown in the
  description; beyond that, ask first. A cohesive change is reviewed by aspect,
  not split for size (see "Review large diffs by aspect" below).
- Plan slices up front and stack them ("Stack: N/M, depends on #X"). Before
  merging a parent, retarget its child to the parent's base. Mechanical changes
  go in their own PR.
- Name the issue. Use a closing keyword only when the PR fixes all of it.

## Review large diffs by aspect

Reassess every review round against the whole PR diff and its base. Above
roughly 1,500 production lines (new or changed logic outside tests), or when a
change spans several subsystems, pick 3-6 cross-cutting aspects from the PR's
risks. Examples include ownership and lifetime, publication and rollback,
integration, tests and mutation evidence, and documentation.

- Both a Claude reviewer and a Codex reviewer cover every aspect. Each reads the
  whole diff, follows interfaces across files, and reports findings for that
  aspect. Do not partition a coupled change into file or path slices.
- Give reviewers the PR summary, the aspect's acceptance points, and previous
  findings. They run relevant tests and probes and report what they ran; a high
  test-to-code ratio calls for extra attention to the tests.
- Keep the same aspects in later rounds, reviewing the delta since the last
  reviewed SHA plus previous findings. A small delta may use one reviewer pair
  without aspect splitting, subject to the review workflow's delta rules.
- Deduplicate findings into one round comment with an aspect column.
- Path slicing is appropriate only for genuinely independent changes, which are
  a signal to split the PR rather than the review.

`drive-pr-to-mergeable` implements this procedure under the same heading.

## Git safety

- Never amend or force-push a branch that has an open PR; add follow-up commits.
  Squash work in progress only before the PR is first opened.
- Never bypass hooks (`--no-verify`). If a hook fails, fix the cause in a new
  commit; never amend after a failed commit.
- Push safely: put the branch name in its own variable, `test -n` it, brace it,
  and redirect push output to a file. Never push a refspec built from an
  unchecked variable.
- Before every push to a PR branch, confirm the PR is still open and unmerged.
- Wait for a held pre-push lock; never work around it. Do not start a push while
  your own long local gate run is still running.

## Conventions

- Commits: short imperative subject (`feat:`, `fix:`, `docs:`, `test:`,
  `chore:`), with the attribution lines the session specifies.
- Plans: `docs/superpowers/plans/YYYY-MM-DD-<slug>.md`; specs:
  `docs/superpowers/specs/YYYY-MM-DD-<slug>-design.md`.
- Do not create markdown files speculatively, and keep test-only fixtures out of
  production code paths.
