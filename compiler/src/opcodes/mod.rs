//! The engine's opcode table: the `JSOp` enum with each opcode's length and
//! stack effect, generated from SpiderMonkey's `vm/Opcodes.h` by
//! `scripts/gen_opcodes.py` and checked in, one file per supported engine
//! version. A cargo feature named after the version (`ff147`, `ff159`,
//! `ff160`) selects the one a build targets; exactly one must be enabled
//! (`ff160` also turns `ff159` on: 160's bytecode is 159's, so only the
//! `ff160` gates differ).
//!
//! Each table records a digest of the `Opcodes.h` it came from, and a build
//! against an engine runs `scripts/gen_opcodes.py check <version> <Opcodes.h>`
//! (the CMake build does; embedders do the same), so an engine whose
//! bytecode differs from the selected table, in shape or in meaning, is
//! refused rather than miscompiled. Adding a version: generate its file,
//! declare the feature in compiler/Cargo.toml and the crates that forward it,
//! add its arm here, and gate any lowering difference on the feature.

#[cfg(feature = "ff147")]
mod ff147;
#[cfg(feature = "ff147")]
pub use ff147::*;

#[cfg(all(feature = "ff159", not(feature = "ff160")))]
mod ff159;
#[cfg(all(feature = "ff159", not(feature = "ff160")))]
pub use ff159::*;

#[cfg(feature = "ff160")]
mod ff160;
#[cfg(feature = "ff160")]
pub use ff160::*;

#[cfg(not(any(feature = "ff147", feature = "ff159")))]
compile_error!(
    "night-compiler: select an engine version by enabling exactly one of the \
     `ff147`, `ff159`, `ff160` features (compiler/src/opcodes/)"
);
#[cfg(all(feature = "ff147", feature = "ff159"))]
compile_error!(
    "night-compiler: the `ff147`, `ff159` and `ff160` engine versions are exclusive; \
     build a non-default version with `--no-default-features --features <version>`"
);
