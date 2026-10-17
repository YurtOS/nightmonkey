# PR #2 review disposition

The earlier strict-quality review is handled by the shared assembler in
`compiler/src/wasm/assemble.rs`, the type-index invariant assertions there,
and reservation slack. The runner retains its independent assembler.

The subsequent review is handled as follows:

1. **Reservation over-allocation:** retained deliberately. The published Yurt
   reservation ABI has no trim operation; closing releases the handle but
   allocated ranges are never reused, and mounted INFO includes the unused
   tail. Counting only widened-ABI scripts before translation would still
   require predicting successful translations and would remove the slack
   explicitly requested in the earlier review. The post-build bound check
   remains. Reducing this overhead safely requires host support for trimming
   or a compiler sizing pass; neither is claimed here.
2. **Retained compiled bytes:** `night_inproc_release_code` frees blobs, the
   assembled module, helper signatures, and helper indices after successful
   installation. It preserves the descriptor address (live tables alias it)
   and script map. A regression test checks both freed capacity and stable
   descriptor storage.
3. **Encoder features:** the production dependency disables defaults and
   enables only `std`. Wasmtime is a native test dependency only.
4. **Short reservation:** Reserve now rejects an undersized range before
   compilation, closes its fd, and leaves the caller's base untouched. The
   Install check remains as a defensive lifecycle check. A native test links
   the production Yurt host implementation and verifies these behaviors.
5. **Early capability probe:** no effective probe exists in the current SDK.
   `yurt_jit_supported()` unconditionally returns 1, including on the JS host
   where reservations return EOPNOTSUPP. A successful reserve-and-close probe
   permanently grows the table because ranges are never reused. Reserving zero
   is invalid, not a capability query. The driver therefore retains the first
   meaningful reservation after the walk computes the bound. An early probe
   needs an owning SDK/kernel ABI change; this limitation remains open.
6. **Instantiation coverage:** a real Wasmtime test binds the named
   `yurt.table` helper, shared owner memory, and owner funcref table; it installs
   exports at a fixed base and executes direct-helper and indirect-table calls.
   The C++ host contract test additionally covers reservation and consumption.
   Actual Yurt guest/kernel mounting remains a separate qualification step.
   Shared memory is the current Yurt module ABI requirement; this does not add
   support for non-shared owner memories.

Validation: `cargo test --workspace` passed; the final focused compiler run
passed all 61 tests. The native C++ contract test passed with
`-Wall -Wextra -Werror`, and `cargo check -p night-guest --target wasm32-wasip1`
passed. Changed Rust files pass rustfmt with child-module traversal disabled;
a recursive formatting check reports pre-existing differences in the generated
opcode table and `wasm/mod.rs`. No full SpiderMonkey shell build or Yurt guest
mount run was performed, and the PR currently has no hosted checks.
