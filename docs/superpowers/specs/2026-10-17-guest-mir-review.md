# Guest MIR design review

Gate 1: user approved `020eeec42cf6071fc53454c1418261b6a5eaaa2a`, including
large import scale; then explicitly authorized stacking on PR #2. Rebase changed
the documentation commit identity; the subsequent spec edit only recorded that
approval and stack instruction. No product code changed.

## Round 1 — f19cf906556eba8a31e6ef66aa5f0cd4a9c22b76

- Codex: APPROVE, read-only source/design review. Plan must thread options into
  guest FFI and prove actual MIR rather than generic fallback. Incorporated.
- Claude: CHANGES REQUESTED. Source-only review, no builds or engine audit.

Verified disposition of Claude findings:

1. High, possible blob undercount: not confirmed. The review inferred “more than
   2+1” without reading the emitter. `8f5360d:compiler/src/wasm/mir/mod.rs:268`
   emits exactly one extra baseline body; `wasm/mod.rs:2107` places extras after
   adapters. Main+adapter+extra = 3, which fits the parent's 3/script allowance.
   Clarified the actual bound and kept one spare separately (4/script), with
   real companion-body tests required. No runtime overflow claim.
2. Medium, guidance/base: reviewer used moving HEAD, which acquired guidance
   after the reviewed SHA. The parent does predate it. Clarified guidance is
   already merged into the child and recorded the approved-content delta above.
3. Medium, engine scope/effort: audited all 16 changed engine files through
   GitHub's complete PR #4 patch list. Recorded mechanisms and unqualified Yurt
   adaptation; estimates remain provisional and Gate 2 must report separate
   engine size. Matching-engine benchmarking remains explicit, not assumed.
4. Medium, proportion: clarified that retaining BBV fails the user's explicit
   compiler-adoption contract; no unmeasured speedup is used as justification.
5. Low/medium, dependencies: added waffle 0.3.3, normal wasm-encoder dependency,
   bounded lockfile updates, and explanation that guest sequential execution
   avoids thread-pool runtime dependence, not a claim of a Rayon build failure.
6. Low, arming proof: named stats, tier census, strict coverage, and explicit
   positive MIR/baseline witnesses. Required guest option transport.

Current parent validation: `cargo test -p night-compiler --locked`: 59 pass.
`cargo fmt --all -- --check`: FAIL, generated opcode and other parent files.
No changes to Rust source; no engine execution performed. Current issue #3 has
no new comments. Full verification remains future implementation work.

## Round 2 — 2b458fd30e93371122352f9c91a69b944d0050db

Both Codex and Claude APPROVE. Both checked the exact spec delta and the
three-body emission claim against source. No builds by reviewers. Claude's
non-blocking notes: quantify alternatives, classify clarifications, and enumerate
option transport pieces in the plan. These are addressed there.

The bound/spare formula, C API option transport and positive tier witnesses are
implementation decisions within Gate 1's approved guest API adaptation and
verification contract, not new capabilities or a different approach. Report
them to the user at Gate 2. User chose local builds first; AWS remains an option
only if local prerequisites prevent qualification.

Parent Clippy `cargo clippy --workspace --all-targets --locked -- -D warnings`
also FAILS with `needless_late_init` at `compiler/src/wasm/regex.rs:1510`.
