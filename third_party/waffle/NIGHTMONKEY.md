# waffle 0.3.3, vendored

Unmodified copy of crates.io `waffle` 0.3.3 (https://github.com/bytecodealliance/waffle,
Apache-2.0 WITH LLVM-exception) except:

- `src/cfg/domtree.rs`: the Cooper-Harvey-Kennedy iterative dominator pass is
  replaced by Lengauer-Tarjan semidominators plus the Semi-NCA idom step.
  CHK is quadratic when one block has tens of thousands of predecessors (a
  `try` around ~17,500 calls: every call can throw into the one catch block;
  non262/regress/regress-155081.js took 164 s in this pass, issue #29). The
  result is the same dominator tree. `compiler/tests/domtree_diff.rs` checks it
  against a copy of the old pass.
- `src/cfg/mod.rs`: the one caller passes successors instead of the postorder.
- `Cargo.toml`: the `roundtrip` integration test and dev-dependencies are
  dropped (tests/ is not vendored).

Used through `[patch.crates-io]` in the workspace `Cargo.toml`. Upstream has
had no release since 0.3.3 and `domtree.rs` has not changed since 2022.
