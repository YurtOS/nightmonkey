# Adopt MIR and baseline compilation inside the guest

Status: Gate 1 approved at `020eeec42cf6071fc53454c1418261b6a5eaaa2a`.
The user subsequently authorized stacking on fork PR #2. Tracks YurtOS/nightmonkey#3.

## Contract and motivation

Adopt the MIR and baseline tiers from bytecodealliance/nightmonkey PR #2,
without moving in-process compilation out of the guest. Preserve snapshot
compilation, the runner testing lane, and the Yurt reservation-and-mount lane.
JavaScript results, exceptions, evaluation order, coercions, side effects and GC
behavior must match the supported SpiderMonkey interpreter. Static analysis
remains speculative; guards and generic fallback remain mandatory.

This is an architectural upgrade, not a demonstrated correctness emergency.
Severity: minor for remaining on the older backend; major if migration breaks
guest execution or the engine ABI. Upstream performance claims are unverified.
No speedup is a release acceptance criterion until measured locally.

Compilation must call the compiler linked into the guest. Retain and adapt
`night_inproc_build`, `night_inproc_max_blobs`, `night_inproc_module`, the compiler
C header, snapshot FFI, guest staticlib, and guest build target. Do not introduce
`night_compile` or `InprocHostCompile`, including in the runner lane.

## Recorded sources and dependencies

Inspected on 2026-10-17:

- Fork default: `origin/yurt` at `be9736c47cea26c838e5256b5b6f36491f1eb6ee`.
- Upstream compiler PR #2: open, head
  `8f5360d70707679a9080edf6a4dcd4f65d89aa71`, unchanged since the issue audit.
- Fork host PR #2: open, head
  `a8e553aa5ab0c7e7274e426a48cb366fd08966bb`. It owns the Yurt host and assembler;
  reuse it rather than implementing a competing host. It must be integrated
  before the compiler adoption can be qualified as complete. The user authorized
  stacking on this PR while it remains open; no edits to its owned branch.
- Upstream engine PR #4: open, head
  `08702e6f7b6c6d9a2ebf9c0cfc37ccebdd395706` (`cfallin/external-slots`).
  Upstream compiler CI switches to this branch. The 16-file engine delta was
  read during design review: shape-for-add APIs, permuted slot span and cache
  restrictions, object-fuse invalidation, testing helpers, and context-state
  offsets. It is not yet independently reviewed or built for Yurt.
- YurtOS/firefox PR #2 remains open at
  `c0fb41d8ebcd902848b343c4ef46e6fbf3513b08`; it contains build fixes, not a
  complete native Yurt mozbuild target.
- Latest SDK release is `guest-sdk-v0.0.28`. Confirm its actual reservation ABI
  and target support before building; the version alone proves neither.
- Kernel issue #3311 (the 1 MiB module cap) is closed; re-run representative
  large batches rather than treating closure as runtime qualification.

The user's request starts design work despite the issue's earlier “not now”.
The unchanged upstream head supports designing against a pinned revision;
upstream movement requires a delta audit and possibly renewed approval.

## Smallest viable approach and alternatives

Recommended: selectively import the pinned compiler/runtime changes, restore
the guest-facing interfaces on top, and reuse the host/assembler from fork PR
#2. Stack the adoption branch on `yurt-host` at the recorded PR #2 head. Preserve
the fork default's policy and workflows (already merged into the child branch;
the parent predates their addition).
Retarget the child to `yurt` when the parent merges and revalidate its resulting
base and head. Preserve
upstream provenance in commits. Do not replace the fork tree wholesale.

The upstream replacement adds roughly 40,000 lines in MIR and baseline modules
and removes roughly 40,000 lines of BBV code, before shared runtime changes.
These are production logic, even when imported unchanged. Exact production,
test, generated and unrelated counts must be established in the plan. This
exceeds the organization size threshold and requires explicit user acceptance
of the import's scale before implementation. Estimate fork-specific integration
at 400–1,200 production lines, plus 300–800 test lines; revise these estimates
after the engine audit. A greater than twofold increase returns to Gate 1.
These are preliminary planning budgets, not measurements or approval to expand
the contract. Engine support is a 16-file upstream delta with Yurt adaptation
still unmeasured; Gate 2 must report its separate size and prerequisite PR.

Keeping BBV does not meet the issue's explicit decision to adopt upstream's new
compiler. The motive is that requested compiler adoption and continued alignment
with upstream, rather than a claimed measured performance improvement. The
large replacement is upstream logic needed for that contract; implementing a
smaller independent compiler would increase fork maintenance.

Alternatives:

1. Full upstream adoption is fewer integration edits but violates the required
   guest compiler contract by using host compilation. Reject.
2. Port only baseline first, followed by MIR. Smaller individual reviews, but
   duplicates work across coupled source/options/runtime interfaces. Use only
   if source audit identifies a clean, independently executable boundary.
3. Reimplement MIR locally. Substantially larger work with no contract benefit.
   Reject.

The final plan may use dependent PRs for engine support, host prerequisite and
compiler adoption. Do not mechanically split coupled compiler logic into
unbuildable slices to satisfy a line target. Review the large import by 3–6
cross-cutting aspects with both Claude and Codex reading the whole diff.

## Compiler and host integration

Keep upstream `Pipeline::Mir` and `Pipeline::Baseline` selection. Initially
retain upstream's MIR default; benchmark both before deciding whether the
guest default needs a separate policy. Any default change is a design decision
to report, not an incidental migration edit.

Use an optional host parallelism feature for Rayon; the guest build selects a
sequential iterator over the same MIR front half. This is the proposed builder
choice: it preserves compilation semantics and avoids making this upgrade
depend on a new Rayon guest port. Host tools may retain parallel compilation.
Qualify that the guest dependency graph excludes Rayon and that host/guest
results agree on representative programs. A later Rayon port is separate work.
This is not a claim that Rayon cannot compile for WASI: successful compilation
would not prove its pool runs correctly in the single-threaded WASI shell. The
feature avoids pool initialization in that lane. Retain `wasm-encoder` as a
normal compiler dependency for the assembler, adopt waffle 0.3.3, and reconcile
Cargo.lock without unrelated dependency updates. All host gates use `--locked`.

Reservation occurs before code generation; blob i installs at base+i. Re-audit
the bound against all emitted helpers, scripts, widened adapters and regex
variants. Upstream increases `PRE_BODIES` from 10 to 11. Keep the post-build
bound check, checked arithmetic and reservation cleanup. A failed compilation
or installation must not arm incomplete scripts or reuse consumed handles.
Emitter audit: baseline emits a body and entry adapter; MIR emits those plus
one companion baseline body (`wasm/mir/mod.rs`, `extra_bodies`). Thus the actual
maximum is three per script. Preserve the parent's one spare per script
separately: proposed reservation bound is 11 + 4*scripts + regex_variants.
The old three-per-script bound would cover these bodies but consume the spare;
it is not evidence of a confirmed overflow. Test a real MIR companion body,
baseline-only compilation, declined scripts and both regex encodings.

Keep assembler import types and function/table indices synchronized with both
tiers. Validate the generated module, then execute it: validation alone does
not establish JavaScript correctness.

## Engine and snapshot ABI

Audit and bring required external-slot engine hooks into YurtOS/firefox through
its own reviewed change. Preserve Yurt build fixes. Record exact engine SHAs
for each build and every timing comparison. Do not copy upstream's temporary
CI branch switch; use a reviewed matching engine revision.

Synchronize new helpers such as `regexp_leaf` and `post_whole_cell`, method-cell
environment fields, C headers, Rust layouts, registration checks, engine
assertions and snapshot ABI validation. Regenerate opcode tables from the
selected engine when required; never hand-edit generated output or relax checks.
Preserve untrusted-input bounds validation, GC rooting/barriers and lifetime
rules in snapshot/FFI paths. Audit upstream parser changes rather than assuming
an import is safe because it is upstream.

## Scope boundaries

Include compiler tiers, required runtime/engine ABI work, guest FFI adaptation,
host/assembler integration and behavioral validation. Keep snapshot mode
working. Exclude upstream benchmark corpus/tooling churn, unrelated plugin
files and host compilation. Do not hide regressions with new exclusions.

Engine and host prerequisites are tracked dependencies, not implied completed
work. The implementation plan must name their PRs and ownership and establish
available build/guest execution infrastructure. No ready claim while any
required prerequisite, lane or review remains incomplete.

## Verification and performance evidence

Before implementation, commit failing behavioral/invariant tests for the guest
contract: bound enforcement, guest compilation with parallelism disabled,
assembler ABI and appropriate tier execution. Demonstrate each new check can
fail for its intended defect. Separate feature tests from existing baseline
failures. Imported upstream tests are useful but do not replace migration tests.

Run all AGENTS.md Rust gates with `--locked` and confirm current-base fmt and
Clippy status. Repair actionable baseline gate defects in separate prerequisite
changes; they remain blockers until resolved.

Build against the recorded matching engine. Verify the harness wrapper exists.
Run local regressions and jit-tests in AOT and interpreter lanes. Exercise MIR
and baseline explicitly; select scoped jstests for affected language semantics
with positive test totals in both lanes. Name filters, totals and exclusions.
Run the shipping snapshot flow on a program with `main()` and compare observable
output to the interpreter. Check changed scripts with `bash -n` and workflows
with actionlint when available.

Build Yurt `js-inproc` with the recorded SDK and run identical programs interpreted
and compiled. Include nonzero reservation bases, adapters, regexes, method cells,
GC pressure and a batch above 1 MiB. Compare output and prove scripts were armed,
not silently interpreted. Check failure cleanup paths.
Use `NIGHT_INPROCESS_STATS=1` compiled/armed counts, `--dump-tiers` records and
`--strict-coverage` together; strict coverage alone permits MIR declining to
baseline. Require a known MIR fixture to report `mir` and a baseline fixture to
report `baseline`; zero armed scripts or absent tier records fail qualification.
Carry `--night-options` through the restored guest C API to `Options::parse_str`
instead of retaining the parent's hard-coded `Options::default()`.

Performance is measured separately: old backend vs MIR vs baseline, same engine
revision, SDK, host/kernel, programs and options. Record compilation, assembly /
installation, runtime and batch size, using repeated Octane runs. Where the old
backend cannot build against the matching engine, establish a compatible common
revision or report the comparison unavailable; do not compare incompatible
builds as an isolated compiler speedup. No local engine build has been located
in the workspace so far; prototype and paired benchmark remain prerequisites to
finalizing a performance-driven choice.

## Workflow gates

Gate 1 approves this design, the proposed sequential guest strategy and the
large upstream import scope. Then independently review spec with Claude and
Codex, audit engine/dependencies, write and review the execution plan, and ask
for Gate 2 approval at its committed SHA. Implementation begins only afterward.
Open draft PRs during execution and drive each through verification and dual
review. The workflow ends at a ready report; merging requires separate authority.
