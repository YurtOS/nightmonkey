/* -*- Mode: C++; tab-width: 8; indent-tabs-mode: nil; c-basic-offset: 2 -*-
 * vim: set ts=8 sts=2 et sw=2 tw=80: */

#ifndef night_runtime_NightInprocHost_h
#define night_runtime_NightInprocHost_h

#include <stdint.h>

#include "compiler/night-compiler.h"

namespace js {
namespace night {

// The in-process host: how a compiled batch reaches the running instance's
// funcref table. One implementation is linked in, chosen at build time
// (NIGHT_INPROC_HOST):
//   runner  NightInprocHost.cpp      wasm-jit-runner's hostcall imports
//   yurt    NightInprocHostYurt.cpp  Yurt's JIT mount ABI: a reserved table
//                                    range and a self-assembled module
//
// The compiled code bakes absolute table indices in, so the base is fixed
// before compiling and blob i must land at base + i:
//   InprocHostReserve(maxBlobs, &base)  before night_inproc_build; maxBlobs
//                                       is night_inproc_max_blobs
//   InprocHostInstall(out, base)        after it
//   InprocHostRelease()                 when the build fails in between
// Reserve and Install return false, with a message on stderr, on failure;
// the caller then stays interpreted. Install releases the reservation on its
// own failures.
bool InprocHostReserve(uint32_t maxBlobs, uint32_t* tableBase);
bool InprocHostInstall(night_inproc_out_t* out, uint32_t tableBase);
void InprocHostRelease();

}  // namespace night
}  // namespace js

#endif  // night_runtime_NightInprocHost_h
