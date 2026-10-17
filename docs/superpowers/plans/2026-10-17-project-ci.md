# Project CI Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Gate pull requests and `yurt` pushes with Rust checks and both execution lanes, with full jstests scheduled and manually available.

**Architecture:** Keep cheap checks in CI and move the existing engine build into a reusable integration workflow. The integration workflow also supports scheduled/manual full jstests. Reuse the existing generated-quality fixes rather than duplicate them.

**Tech Stack:** GitHub Actions, stock Rust/Cargo, Python unittest, actionlint, CMake, SpiderMonkey wasm32-wasip1.

**Spec:** Scope approved in chat on 2026-10-17: fast Rust/workflow gates, both AOT and interpreter local regressions and jit-tests, scheduled/manual full jstests, read-only permissions, caches and timeouts.

## Global Constraints

- Start from `origin/yurt` in an isolated worktree.
- Use `--locked` for host Cargo checks and retain the engine-table check.
- No exclusions added to hide failures; document actual coverage and hosted limits.
- Do not change branch protection or merge without authorization.

## Review Focus

- Fork PRs must run without private credentials.
- Independent gates must report even if formatting fails.
- Engine revision and build inputs must be represented in caches.
- Empty local suites and missing wrappers must fail.
- Scheduled/manual full jstests must run both lanes even if one fails.

### Task 1: Restore quality gates

- [x] Reuse and commit the existing generator regression; observe it fail on the baseline.
- [x] Reuse the generator fix and regenerated table in separate commits.
- [x] Reproduce Clippy's late-init failure and reuse its tuple initialization fix.
- [ ] Run fmt, check, test, Clippy and generator tests.

### Task 2: Add CI coverage

- [x] Add behavioral local-harness tests for missing wrapper, empty suite and shell failure; commit red tests first.
- [x] Make the local harness reject setup failures and empty suites.
- [x] Split fast workflow checks from the reusable engine build; add both local and jit-test lanes.
- [x] Add scheduled/manual full jstests and retain logs as artifacts.
- [x] Run actionlint and shell syntax checks; inject an invalid workflow context and shell failure to prove gate rejection.

### Task 3: Verify and review

- [x] Update repository CI coverage documentation.
- [ ] Run applicable verification and report missing engine prerequisites explicitly.
- [ ] Obtain independent Codex and Claude reviews where available; resolve valid findings.
- [ ] Commit the reviewed changes and report the worktree/branch and hosted state.

## Validation record

- Generator test failed on the unformatted digest before the existing fix was reused; the regenerated table matches the supported local engine header at `c0fb41d8ebcd902848b343c4ef46e6fbf3513b08`.
- The unchanged regex code failed Clippy with `needless_late_init`; tuple initialization removes the finding.
- Local-harness tests reproduced empty-suite and missing-wrapper false successes and path splitting before the fix.
- Workflow-command tests reproduced empty upstream suites returning zero; the hosted jit and jstests steps now reject missing/zero coverage. All nine Python tests pass.
- Actionlint rejected an injected undefined context. Bash pipefail rejected an upstream harness failure piped through tee.
- Codex approved `3b462b9` after the empty-suite finding was fixed. Claude review and hosted integration validation are pending.
- No compatible engine dist exists in this worktree; C++ integration, both actual JS execution lanes, full jstests and shipping snapshot execution have not been locally qualified.
- Live GitHub inspection found no classic branch protection and no applicable branch rulesets on `yurt`; no required-check policy was changed.

Ruling: pin host Rust 1.98.1, matching sibling yurt-rust CI, instead of 1.99.1. The latter enables new `chunks_exact_to_as_chunks` lints in unchanged snapshot code; retaining the established toolchain avoids widening CI setup into snapshot semantics changes. Rust 1.98.1 exceeds the locked dependencies' 1.93.0 minimum. All gates must pass on this exact pin.
