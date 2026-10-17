# Guest MIR implementation evidence

Gate 2 approved by user "go" for plan b4a98b29e89655878c0186b7b9c477369eb06a35.
Implementation is in progress; no merge readiness claim.

Recorded inputs: compiler upstream 8f5360d70707679a9080edf6a4dcd4f65d89aa71;
NightMonkey parent PR2 dda91dc5243720d297abd07caf81d8f756da9ebc, still open;
Firefox yurt/original engine f0c060c52260d41fc631a14cda05372ad1c2cbe1;
required engine delta head 08702e6f7b6c6d9a2ebf9c0cfc37ccebdd395706.

Setup: local macOS arm64, Rust 1.99.1; CMake, Ninja, actionlint, Wasmtime and
wasm32-wasip1 Rust target available. Local engine checkout/build being prepared.
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

Quality prerequisite at e8e3664: generator regression committed red0815c97
(rustfmt rejects emitted digest declaration), green after generator3b0d3b7;
ff147 regenerated separately c094a07 and matches the pinned engine header.
Regex Clippy lint reproduced and removed by tuple initialization cc59c0a;
focused compiler Clippy, workspace fmt and actionlint pass. Full workspace gates
and engine build remain in progress.
