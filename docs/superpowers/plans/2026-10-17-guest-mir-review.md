# Guest MIR implementation plan review

Plan: `2026-10-17-guest-mir.md`. User Gate 2 approval is pending.

## Reviews and revisions

- Codex APPROVE at `449442d566eaae68e07074ed7e3196e8b4928009`.
  Clarification: retained FFI adaptation belongs in the buildable import;
  subsequent integration is not permission for a broken intermediate PR.
- Parent PR #2 advanced to `dda91dc5243720d297abd07caf81d8f756da9ebc`
  and was merged into the child. Plan preserves code-buffer release and stable
  environment storage, reuses new installation tests and keeps generator fixes.
- Codex APPROVE at `119e4eadf833eddb7a76619771900025c0d7dc22`.
- Replaced nonexistent guessed ecma_6 filters with test262 directories verified
  via GitHub contents at pinned engine `08702e6`. Codex APPROVE at `d7267c8`.
- Initial Claude plan review did not return after several minutes and was
  terminated. A restricted retry returned a limited approval but stopped after
  command permissions denied further inspection; that was not counted as a
  completed review. The next invocation allowed read-only Bash commands and
  completed the missing source/policy checks.
- Final clarification requires stored import inventory, absence of waffle's
  parallel feature as well as Rayon in the guest graph, and explicit sequential
  and parallel compiler test runs despite workspace feature unification.

## Final result

Both Claude and Codex APPROVE the exact plan at
`b4a98b29e89655878c0186b7b9c477369eb06a35`, with no verified blockers.
Both are source/plan reviews, not engine or runtime qualification.

Claude checked policy, manifests/locks and feature isolation, FFI option shape,
code-release lifecycle, generator/CMake integration, bound formula, source/test
counts and supplied SDK reservation header/implementation. Limits: no builds,
no network verification of engine/test filters/release provenance and no local
waffle 0.3.3 manifest. Codex checked exact plan deltas and pinned source for
contract coverage, interfaces, sequencing, ownership and qualification limits.

Orchestrator independently verified test262 filter directories, live engine
PR delta (16 files, 498 additions/12 deletions), SDK release tag
`56b67bb2346095ff6eaf54a5116d5b66f21bd43b` and source header/implementation via
GitHub. This is source provenance, not payload or guest qualification.

Local evidence: parent `a8e553a` compiler tests 59 pass; fmt and Clippy fail on
unchanged parent source. Latest parent `dda91dc` native C++ host contract test
passes with `-Wall -Wextra -Werror` against the SDK release source header and
mocked host calls. No engine build, JS execution or real Yurt mount was run.
The issue had no new comments when last checked. No implementation or push yet.
