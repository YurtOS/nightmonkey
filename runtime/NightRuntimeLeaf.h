/* -*- Mode: C++; tab-width: 2; indent-tabs-mode: nil; c-basic-offset: 2 -*-
 * vim: set ts=8 sts=2 et sw=2 tw=80: */

// The leaf half of the AOT runtime's C ABI. A leaf helper runs no JavaScript
// and never GCs, so compiled code calls it without rooting its live values or
// publishing a scan top: none takes the `top` parameter every may-GC helper
// in NightRuntime.h takes. Keep a helper here only while that stays true; a
// helper that can allocate, run a hook or throw belongs in NightRuntime.h.
//
// Include through NightRuntime.h, which defines NIGHT_RUNTIME_EXPORT.

#ifndef night_runtime_NightRuntimeLeaf_h
#define night_runtime_NightRuntimeLeaf_h

#ifndef NIGHT_RUNTIME_EXPORT
#  error "include runtime/NightRuntime.h, not NightRuntimeLeaf.h"
#endif

#include <stdint.h>

#include "js/TypeDecls.h"

extern "C" {

// The whole-cell post barrier for a tenured cell (a cached iterator) a
// compiled for-in start stored a nursery object in (leaf).
NIGHT_RUNTIME_EXPORT(night_runtime_post_whole_cell)
void night_runtime_post_whole_cell(JSContext* cx, uint32_t cell);
// `1` iff every slot of native object `obj` below `n` holds a property: the
// stamp gates' coverage test for a shape with permuted slots, whose span
// alone does not say it (holes). Leaf (a walk of the shape, no GC).
NIGHT_RUNTIME_EXPORT(night_runtime_slots_covered)
int32_t night_runtime_slots_covered(uint32_t obj, uint32_t n);
// MIR's `getprop.data` miss (leaf: no GC, no JS): `recv.atomId` where
// the lookup runs no code -- an own or prototype data property, an absent
// one, or a pure builtin length -- filling the site's inline ways as the
// IC miss does. A magic value where the lookup would run code (a getter, a
// proxy, a resolve hook) or throw.
NIGHT_RUNTIME_EXPORT(night_runtime_get_prop_pure)
uint64_t night_runtime_get_prop_pure(JSContext* cx, uint64_t recv,
                                     uint32_t atomId, uint32_t cacheIdx);
// MIR's `setprop.data` miss: `recv.atomId = val` where the set runs no
// code (an own writable data property of a native object, or an add with
// no setter, read-only property or hook on the chain; never the global or
// a Watchtower-watched object), through the IC miss path (filling the
// site's way). `flags` bit 1 (2): the compiled vouch, as
// `night_runtime_set_prop_ic_miss`'s. 1 once stored, 2 once stored where
// it demoted a claim of the object's published class MIR reads (TYPES,
// SLOTS, the class), 0 where it would not be such a set (nothing done), 3
// on an engine error. May GC.
// Whether ToPrimitive of `v` (boxed) runs no user code: a primitive, or
// an object whose conversion is Object.prototype's own (no @@toPrimitive,
// the builtin valueOf and toString, a data @@toStringTag or none), found
// by pure lookups (MIR's `prim.*`). Leaf.
NIGHT_RUNTIME_EXPORT(night_runtime_to_primitive_pure)
int32_t night_runtime_to_primitive_pure(JSContext* cx, uint64_t v);
// `method.load`'s miss: arm the predicted-method cell at `cellAddr` for
// receiver `recvBits` (its name `atomId`, its predicted script at
// `scriptAddr`). 1 = armed for this receiver's prototype; 0 = not (the cell
// stays unarmable until a GC). A leaf.
NIGHT_RUNTIME_EXPORT(night_runtime_method_arm)
int32_t night_runtime_method_arm(uint64_t recvBits, uint32_t cellAddr,
                                 uint32_t atomId, uint32_t scriptAddr);
NIGHT_RUNTIME_EXPORT(night_runtime_ctor_stamp)
void night_runtime_ctor_stamp(uint64_t thisBits, uint32_t layoutId,
                              uint32_t nFields, uint32_t keepBits);
// The two-phase restamp at an init delegate's returns (the restamp form
// of the ctor-exit stamp): advance `this` to layout `layoutId` when it is
// already that layout (no-op), still under the CONSTRUCTING sentinel with
// an early key of ours, a prefix layout's, or none, or stamped with a
// prefix layout and eligible to advance; and its slot span covers the
// layout. `prefixN` are prefix layout ids + 1 (0: none). Leaf.
NIGHT_RUNTIME_EXPORT(night_runtime_ctor_restamp)
void night_runtime_ctor_restamp(uint64_t thisBits, uint32_t layoutId,
                                uint32_t nFields, uint32_t keepBits,
                                uint32_t prefix0, uint32_t prefix1,
                                uint32_t prefix2, uint32_t prefix3);
// The pristine RegExp.prototype.exec (forTest 0) / .test (1) called on `re`
// with `str`, decided by a leaf: the result, or magic bits to take the call.
NIGHT_RUNTIME_EXPORT(night_runtime_regexp_leaf)
uint64_t night_runtime_regexp_leaf(JSContext* cx, uint64_t re, uint64_t str,
                                   uint32_t forTest);

}  // extern "C"

#endif  // night_runtime_NightRuntimeLeaf_h
