/* -*- Mode: C++; tab-width: 8; indent-tabs-mode: nil; c-basic-offset: 2 -*-
 * vim: set ts=8 sts=2 et sw=2 tw=80: */

// The engine version the runtime is built for, and what the compiler
// assumes of that engine beyond its opcode table.
//
// NIGHT_JSOP_SEMANTICS_VERSION is the JSOP_SEMANTICS_VERSION recorded in
// the opcode table NIGHT_ENGINE_VERSION selects (compiler/src/opcodes/,
// passed by CMake); it must be the engine's own. The per-version code in
// the runtime keys on it.
//
//   1: Firefox 147 (ff147).
//   2: Firefox 159 (ff159): JSWhyMagic renumbered and JS_GENERATOR_CLOSING
//      removed; a resumed generator gets [value, resume kind] and handles
//      throw and return resumptions in bytecode (no CheckResumeKind or
//      closing magic); DynamicImport takes an ImportPhase; the
//      explicit-resource-management ops are unconditional; module
//      generators hold their ModuleObject in the callee slot.

#ifndef runtime_NightEngineVersion_h
#define runtime_NightEngineVersion_h

#include "js/Value.h"    // JSWhyMagic, JSVAL_TAG_*
#include "vm/Opcodes.h"  // JSOP_SEMANTICS_VERSION

#ifndef NIGHT_JSOP_SEMANTICS_VERSION
#  error "NIGHT_JSOP_SEMANTICS_VERSION is set by CMake from the selected opcode table"
#endif
static_assert(JSOP_SEMANTICS_VERSION == NIGHT_JSOP_SEMANTICS_VERSION,
              "the engine's bytecode semantics are not the selected opcode "
              "table's: build with the NIGHT_ENGINE_VERSION of this engine");

// The value tags the compiler emits (compiler/src/wasm/translate.rs TAG_*,
// which also relies on their order). These are 147's; 159 renumbered
// JSVAL_TYPE_STRING, _SYMBOL and _PRIVATE_GCTHING, which the compiler has
// not been ported to, so a 159 engine is refused here.
static_assert(JSVAL_TAG_CLEAR == 0xFFFFFF80, "translate.rs TAG_CLEAR");
static_assert(JSVAL_TAG_INT32 == 0xFFFFFF81, "translate.rs TAG_INT32");
static_assert(JSVAL_TAG_BOOLEAN == 0xFFFFFF82, "translate.rs TAG_BOOLEAN");
static_assert(JSVAL_TAG_UNDEFINED == 0xFFFFFF83, "translate.rs TAG_UNDEFINED");
static_assert(JSVAL_TAG_NULL == 0xFFFFFF84, "translate.rs TAG_NULL");
static_assert(JSVAL_TAG_MAGIC == 0xFFFFFF85, "translate.rs TAG_MAGIC");
static_assert(JSVAL_TAG_STRING == 0xFFFFFF86, "translate.rs TAG_STRING");
static_assert(JSVAL_TAG_SYMBOL == 0xFFFFFF87, "translate.rs TAG_SYMBOL");
static_assert(JSVAL_TAG_BIGINT == 0xFFFFFF89, "translate.rs TAG_BIGINT_HI");
static_assert(JSVAL_TAG_OBJECT == 0xFFFFFF8C, "translate.rs TAG_OBJECT");

namespace js {
namespace night {

// The magic payloads the compiler emits (translate.rs MAGIC_*), and the
// resume sentinel EnterNightResume stages as `this` (MAGIC_RESUME_THIS):
// a magic no call passes as `this`.
#if JSOP_SEMANTICS_VERSION == 1
static_assert(JS_ELEMENTS_HOLE == 0 && JS_NO_ITER_VALUE == 1 &&
                  JS_GENERATOR_CLOSING == 2 && JS_IS_CONSTRUCTING == 5 &&
                  JS_UNINITIALIZED_LEXICAL == 10,
              "translate.rs MAGIC_* (ff147)");
inline constexpr JSWhyMagic kNightResumeThisMagic = JS_GENERATOR_CLOSING;
#elif JSOP_SEMANTICS_VERSION == 2
static_assert(JS_UNINITIALIZED_LEXICAL == 0 && JS_ELEMENTS_HOLE == 1 &&
                  JS_NO_ITER_VALUE == 2 && JS_IS_CONSTRUCTING == 5 &&
                  JS_GENERIC_MAGIC == 12,
              "translate.rs MAGIC_* (ff159)");
inline constexpr JSWhyMagic kNightResumeThisMagic = JS_GENERIC_MAGIC;
#else
#  error "unsupported JSOP_SEMANTICS_VERSION: add the engine version (README, Engine versions)"
#endif

}  // namespace night
}  // namespace js

#endif  // runtime_NightEngineVersion_h
