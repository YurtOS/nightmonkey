/* -*- Mode: C++; tab-width: 8; indent-tabs-mode: nil; c-basic-offset: 2 -*-
 * vim: set ts=8 sts=2 et sw=2 tw=80: */

// The wasm-jit-runner in-process host (NightInprocHost.h): the runner's
// hostcall imports, isolated in their own TU. They make the module
// instantiable only under hosts that provide them (the runner; wizer stubs
// unknown imports; plain `wasmtime run` needs `-W unknown-imports-trap`), so
// callers must gate on --night-inprocess. The runner appends functions
// contiguously, so the table size read before compiling is the base. The
// runner also provides a third hostcall, env.wasm_add_funcs (no
// extern-function resolution); the driver does not use it.

#include "runtime/NightInprocHost.h"

#include <stdio.h>
#include <vector>

#ifdef __wasm__

extern "C" {
__attribute__((import_module("env"), import_name("wasm_table_size"))) int32_t
wasm_table_size();
__attribute__((import_module("env"), import_name("wasm_add_funcs2"))) int32_t
wasm_add_funcs2(uint32_t bytecodeArr, uint32_t lensArr, int32_t nfuncs,
                uint32_t externArr, int32_t nextern, uint32_t outPtr);
}

static uint32_t Addr(const void* p) {
  return static_cast<uint32_t>(reinterpret_cast<uintptr_t>(p));
}

bool js::night::InprocHostReserve(uint32_t, uint32_t* tableBase) {
  int32_t size = wasm_table_size();
  if (size < 0) {
    fprintf(stderr,
            "night: inprocess: wasm_table_size hostcall failed (not running "
            "under wasm-jit-runner?)\n");
    return false;
  }
  *tableBase = static_cast<uint32_t>(size);
  return true;
}

bool js::night::InprocHostInstall(night_inproc_out_t* out,
                                  uint32_t tableBase) {
  uint32_t nblobs = night_inproc_num_blobs(out);
  std::vector<uint32_t> blobPtrs(nblobs), blobLens(nblobs), got(nblobs);
  for (uint32_t i = 0; i < nblobs; i++) {
    blobPtrs[i] = Addr(night_inproc_blob_ptr(out, i));
    blobLens[i] = night_inproc_blob_len(out, i);
  }
  if (wasm_add_funcs2(Addr(blobPtrs.data()), Addr(blobLens.data()),
                      static_cast<int32_t>(nblobs),
                      Addr(night_inproc_extern_indices(out)),
                      static_cast<int32_t>(night_inproc_num_externs(out)),
                      Addr(got.data())) != 0) {
    fprintf(stderr, "night: inprocess: wasm_add_funcs2 hostcall failed\n");
    return false;
  }
  for (uint32_t i = 0; i < nblobs; i++) {
    if (got[i] != tableBase + i) {
      fprintf(stderr,
              "night: inprocess: blob %u landed at table[%u], predicted %u\n",
              i, got[i], tableBase + i);
      return false;
    }
  }
  return true;
}

#else

bool js::night::InprocHostReserve(uint32_t, uint32_t*) { return false; }

bool js::night::InprocHostInstall(night_inproc_out_t*, uint32_t) {
  return false;
}

#endif  // __wasm__

void js::night::InprocHostRelease() {}
