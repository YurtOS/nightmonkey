/* -*- Mode: C++; tab-width: 2; indent-tabs-mode: nil; c-basic-offset: 2 -*-
 * vim: set ts=8 sts=2 et sw=2 tw=80: */

// X-macro list of the MIR tier's census kinds and how night_runtime_census
// treats each: the MIR_*_CENSUS_KIND constants in compiler/src/options.rs,
// checked entry for entry by `census_kinds_match_runtime_list`.
//
//   Count      counted in the map, keyed by kind << 32 | id.
//   Trace      counted, and also prints a trace line (NIGHT_CENSUS_TRACE).
//   TraceOnly  prints the failing object while tracing; never counted.
//   FlatBlock  one flat counter per id (the dense per-block census), because
//              it ticks several times per bytecode op.

#ifndef night_runtime_NightCensusKinds_h
#define night_runtime_NightCensusKinds_h

#include <stdint.h>

#define NIGHT_CENSUS_KINDS(X)    \
  X(EXIT, 90, Trace)             \
  X(GET_MISS, 91, Count)         \
  X(GUARD, 92, Trace)            \
  X(GUARD_CLASS, 93, Count)      \
  X(TYPES_DROP, 94, Count)       \
  X(SET_MISS, 95, Count)         \
  X(GUARD_OBJECT, 96, TraceOnly) \
  X(SLOW, 97, Count)             \
  X(BLOCK, 98, FlatBlock)        \
  X(ROOT, 99, Count)             \
  X(VALUE, 100, Count)           \
  X(STAMP_NO_TYPES, 101, Count)  \
  X(MEGA_SET, 102, Count)

namespace js::night {

enum class CensusMode { Count, Trace, TraceOnly, FlatBlock };

// The kind of the flat per-block counter (the FlatBlock entry above).
constexpr uint32_t kCensusBlockKind = 98;

constexpr CensusMode CensusModeOf(uint32_t kind) {
  switch (kind) {
#define NIGHT_CENSUS_CASE(name, number, mode) \
  case number:                                \
    return CensusMode::mode;
    NIGHT_CENSUS_KINDS(NIGHT_CENSUS_CASE)
#undef NIGHT_CENSUS_CASE
    default:
      return CensusMode::Count;
  }
}

static_assert(CensusModeOf(kCensusBlockKind) == CensusMode::FlatBlock,
              "kCensusBlockKind must be the FlatBlock entry");

}  // namespace js::night

#endif  // night_runtime_NightCensusKinds_h
