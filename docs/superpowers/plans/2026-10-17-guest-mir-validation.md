# Guest MIR implementation evidence

Gate 2 approved by user "go" for plan b4a98b29e89655878c0186b7b9c477369eb06a35.
Implementation is in progress; no merge readiness claim.

Recorded inputs: compiler upstream 8f5360d70707679a9080edf6a4dcd4f65d89aa71;
NightMonkey parent PR2 37706817fb6f25aa532e366e7a86960473e9a5e9, still open;
Firefox yurt/original engine f0c060c52260d41fc631a14cda05372ad1c2cbe1;
required engine delta head 08702e6f7b6c6d9a2ebf9c0cfc37ccebdd395706.

Setup: local macOS arm64, Rust 1.99.1; CMake, Ninja, actionlint, Wasmtime and
wasm32-wasip1 Rust target available. Local engine checkout and WASI builds completed.
Firefox issue1 (native wasm32-yurt-linux-musl mozbuild target) remains open;
Firefox PR2 source fixes remain open at c0fb41d8. No temporary wrappers added.

Planning baseline: old parent a8e553a compiler tests 59 pass; fmt and Clippy
failures reproduced on unchanged source. Latest parent's C++ host contract
test passed with source SDK header and mocked host calls. These do not establish
JavaScript execution, snapshot or Yurt mount correctness.

SDK payload: `guest-sdk-0.0.28-darwin-arm64.tar.zst` passes its published SHA256
check. Extracted only in the plan workspace. Manifest identifies LLVM revision
`0d261d1ca552c95a8f007e061c787ac7132fbcbc`; packaged `yurt_jit.h` is byte-identical
to the audited release-tag header. This establishes payload/source consistency,
not guest runtime behavior.

Quality prerequisite PR6 at 17a177bb38b9f31c22bef50072e2eb9e604ffa7a:
generator regression committed red0815c97
(rustfmt rejects emitted digest declaration), green after generator3b0d3b7;
ff147 regenerated separately c094a07 and matches the pinned engine header.
Regex Clippy lint reproduced and removed by tuple initialization cc59c0a;
full workspace fmt/check/test/clippy pass, with 58 compiler tests and no failed
tests. Python generator unittest, pinned engine-table check, actionlint and shell
syntax checks pass. Mutation to the base generator makes the same CI test fail
on the digest formatting, and restoring the fixed generator makes it pass.
Snapshot inherits the existing workspace Rust version to preserve its intended
MSRV during Clippy checks. The Linux-only stdcxx option is conditional on host
Linux. Claude and Codex approved the e8e3664 full diff; Codex approved the small
17a177b metadata/configuration delta. Hosted jit-test passed at that head
(run39047202775, job117438201455; 35m49s).

Engine build: unchanged f0c060c, isolated Mozilla bootstrap/sysroot. macOS27 SDK
TAPI arm64e.x1 is unsupported by bundled LLVM20's host linker. Explicit
`HOST_LDFLAGS=--ld-path=/usr/bin/ld` in `mach configure` uses the system linker
for host utilities, retaining the original Wasm target compiler/linker. This
fixes the observed nsinstall link failure. The baseline engine and CMake builds
succeeded. A plain-engine JS assertion control passes; all five existing local
regressions pass in both compiled and interpreter lanes. The cache is isolated
under the build tree. Initial unwritable HOME-cache setup failures are excluded
from behavioral results. Broad AOT and interpreter sweeps were interrupted after discovering a false-green
expected-error verdict in the engine harness. Their selected-count summaries
are not executed-pass totals and do not qualify either lane.

At child b19dcd0, refreshed workspace fmt/check/test/clippy pass (61 compiler
tests, including the latest parent's assembler/lifetime regressions). A real
snapshot fixture with main() was captured; its force-interpreted and BBV-compiled
shipping artifacts both execute under Wasmtime and print42. These baseline
results do not qualify the new tiers or Yurt execution.

Matching engine prerequisite is draft [Firefox PR3](https://github.com/YurtOS/firefox/pull/3),
head f850071eda00e1499d3b85b0616e0921853efd68. It imports the pinned hooks and
fixes property-map flag copying and bulk Object.assign bypasses demonstrated by
prior red regressions. Three direct JS regressions and four Python harness tests
pass. A mutation disabling fuse notification makes its regression fail. Both
Claude and Codex approved the final source; Codex independently ran those tests.
Final WASI engine artifact SHA256 is
4738dbdc11891ca89ab7294d17f23a31ed7387811770e7ecc82ee3ed338a97b4.
This is focused source verification; broad execution and native Yurt qualification
remain incomplete, and the PR stays draft.

The corrected harness exposes an old AOT failure in auto-regress/bug765483.js:
expected ReferenceError, observed TypeError from evalInFrame. AOT reports one
failure/exit2; interpreter reports one pass/exit0. No exclusion was added. The
original harness printed an unexpected failure but returned success; it also
returned success after interruption. Both verdict defects have red regressions
and fixes in Firefox PR3. Full new-tier qualification must use the corrected
harness. Quality PR6's hosted green does not replace that qualification.

Task3 selective import is in progress; the [import inventory](2026-10-17-guest-mir-import.tsv)
records every upstream path disposition. Guest staticlib/FFI, parent assembler,
installation host and lifecycle remain present. Host compilation hostcalls are
not imported. Waffle is pinned to0.3.3; guest graphs exclude Rayon and Waffle's
parallel feature. Forcing either feature makes the graph checker fail.
Sequential compiler check passes. Generator operand-format regression was
committed red; generated output was regenerated in its own commit, matches the
engine header and passes rustfmt. Dropping format flag separation makes the
regression fail. Full workspace checks and execution of the new tiers are pending.

Digest malformed-input tests were committed red caa87e0. Native testing rejects
the old parser's acceptance of trailing bytes; actual wasm32 tests trap on
source-line/scope-binding capacity overflow and GC-thing byte arithmetic overflow.
The valid WASI digest control passes. Bounds corrections pass all three native and all six WASI tests. The same
overflow inputs now return parser errors rather than trapping. This focused
reader proof does not yet establish the shipping snapshot flow.
