# Agent instructions — Nightmonkey

Read [docs/agents/org.md](docs/agents/org.md) first. It is a verbatim copy of
YurtOS's shared policy; change the canonical kernel copy before syncing it.
`CLAUDE.md` points here. The Linux contract in that policy governs OS interfaces;
JavaScript semantics follow ECMAScript and the supported SpiderMonkey engine.

## Purpose and layout

Nightmonkey is an ahead-of-time JavaScript-to-WebAssembly compiler layered on
SpiderMonkey. Read `README.md`, `docs/DESIGN.md` and `docs/INTEGRATION.md` before
changing architecture or engine integration.

- `compiler/`: `night-compiler`; bytecode, likely-types analysis, facts and Wasm emission.
- `snapshot/`: snapshot/live-heap reader and registration/layout validation.
- `nightmonkey/`: host AOT tool; snapshot rewriting and optional Wizer integration.
- `runtime/`, `shell/`: C++ engine integration, runtime helpers and shell entry points.
- `guest/`: compiler/snapshot reader staticlib for the in-process lane.
- `wasm-jit-runner/`: Wasmtime host and function-injection hostcalls.
- `spidermonkey/`, `configs/`: engine build configurations.
- `scripts/`: build, opcode generation and test harness wrappers.
- `tests/jit-test/`: local JavaScript regressions; `tests/*excludes.txt`: annotated exclusions.
- `tools/`: profiling and benchmarking helpers.

The default branch is `yurt`; use `origin/yurt` as the starting base for new
work and PRs unless the task names another base. Do not infer the default from
the upstream CI workflow's `main` push trigger.

## Build

This repository uses stock Cargo and `wasm32-wasip1`, not `cargo-yurt` or the
Yurt guest SDK. The workspace declares Rust 1.82.0 as its minimum; it does not
pin a toolchain. Use `--locked` for reproducible host Rust commands.

```sh
cargo build --workspace --locked
scripts/build-spidermonkey.sh /path/to/firefox
cmake -S . -B build -DSPIDERMONKEY_DIST=/path/to/firefox/obj-nightmonkey-sm/dist
cmake --build build
```

Integrated builds need the patched SpiderMonkey tree, its private headers and
libraries, the WASI sysroot, CMake and the `wasm32-wasip1` Rust target. Consult
`.github/workflows/ci.yml` for bootstrap steps and the engine branch. That branch
is resolved to a SHA per CI run; record the resolved revision when reproducing.
Do not assume a neighboring repository's engine build is ABI-compatible.

## Verification

For Rust changes, run the applicable workspace checks:

```sh
cargo fmt --all -- --check
cargo check --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

These are local development checks; the current hosted workflow does not run
them as separate gates. Rust checks alone do not exercise C++ integration or
prove JavaScript execution correctness.

After an integrated build, run local regressions and SpiderMonkey jit-tests in
both AOT and interpreter lanes:

```sh
scripts/run-night-tests.sh build
NIGHT_INPROCESS_OFF=1 scripts/run-night-tests.sh build
scripts/run-jit-tests.sh /path/to/firefox build -- --format=automation
NIGHT_INPROCESS_OFF=1 scripts/run-jit-tests.sh /path/to/firefox build -- --format=automation
```

For affected language semantics, also run scoped jstests in both lanes with
`scripts/run-jstests.sh /path/to/firefox build -- <path-filter>` and its
`NIGHT_INPROCESS_OFF=1` counterpart. State the filter and test counts. Full
jstests can take hours. A zero-test run is not evidence: inspect harness totals,
including the local regression wrapper's pass count.

For snapshot reader/rewriter, runtime layout or entry changes, exercise the
shipping snapshot flow as well, using a representative program with `main()`:

```sh
build/bin/nightmonkey --shell build/bin/js /absolute/path/program.js -o build/program-aot.wasm
wasmtime run build/program-aot.wasm
```

Compare observable results with the interpreter. For shell/workflow edits run
`bash -n` on changed shell files and `actionlint` when available. Documentation
and skill-only changes need link/path, frontmatter and diff checks rather than
rebuilding the engine.

The current `.github/workflows/ci.yml` job `jit-test` builds SpiderMonkey and
Nightmonkey and runs **AOT jit-tests only** on PRs, pushes to `main` and manual
dispatch. It does not run baseline jit-tests, local regressions, jstests or the
shipping snapshot flow. Check live branch protection before describing any
hosted job as required. Report skipped lanes, missing prerequisites, test
exclusions and base-reproduced failures explicitly.

## Correctness and integration rules

- Analysis predictions are speculative. Every specialization must be guarded,
  with a correct generic path when assumptions fail. Do not introduce runtime
  profiling as an input to the AOT analysis.
- Preserve JavaScript coercions, exceptions, evaluation order, observable side
  effects and GC behavior. Test through JavaScript execution and compare with
  the supported SpiderMonkey interpreter; Wasm validation alone is insufficient.
- Keep layout constants, engine assertions, snapshot ABI checks and the C ABI
  synchronized. Never silence a layout/version mismatch to make a build pass.
- Opcode tables are generated from `vm/Opcodes.h` by `scripts/gen_opcodes.py`.
  Regenerate them and review semantic lowering changes; do not hand-edit a table
  or bypass CMake's engine-table check. Forward new engine features through all
  consuming crates as described in `README.md`.
- Treat snapshot bytes, guest pointers/lengths and hostcall payloads as untrusted.
  Keep bounds checks, rooting, barriers and lifetime invariants at their owners.
  Narrow Rust `unsafe` blocks and document their invariants with `SAFETY` comments.
- Use Rust for compiler/reader/host work and C++ where engine integration needs
  it. Do not transplant kernel syscall policies or application workarounds here.
- Add behavioral regressions under `tests/jit-test/` for execution bugs and Rust
  tests for compiler/reader invariants. Establish the failure before fixing it.
- Exclusions document designed-out capability or an established harness limit;
  they must not hide a new miscompile. Keep WASI-wide and AOT-only lists distinct.
- Benchmark performance changes against the same engine revision, program,
  options and host. Separate correctness evidence from timing results.

## Skills and discovery

The four shared workflows live in `.agents/skills/`; `.claude/skills` links to
that directory so both clients use one maintained copy. Use issue-to-mergeable-pr,
drive-pr-to-mergeable, drive-pr-stack and drive-doc-to-approved for their named
workflows. Global plugins and CLIs are prerequisites, not vendored dependencies;
report unavailable reviewers rather than claiming their approval.

When codebase-memory-mcp is available, index the repository if necessary and
prefer `search_graph`, `trace_path`, `get_code_snippet` and `query_graph` for code
discovery. Use `rg` for literals, non-code files or insufficient graph results.
Never copy local permission grants, credentials, personal memory commands or
machine-specific MCP paths from another repository.
