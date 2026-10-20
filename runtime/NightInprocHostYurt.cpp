/* -*- Mode: C++; tab-width: 8; indent-tabs-mode: nil; c-basic-offset: 2 -*-
 * vim: set ts=8 sts=2 et sw=2 tw=80: */

// The Yurt in-process host (NightInprocHost.h), over Yurt's JIT mount ABI
// (yurtos-kernel docs/abi/jit-mount.md): reserve a table range sized by the
// exact translated blob count, bind its base, assemble the blobs into one module
// (night_inproc_module) and mount it into the reservation, which publishes
// export __jit_entry_<i> at base + i. Reservations and reserved mounts are
// Wasmtime-only today; elsewhere yurt_jit_reserve fails (EOPNOTSUPP) and the
// shell stays interpreted. A mount belongs to the calling thread's store, so
// compiled code must only be called from the thread that mounted it.
// NIGHT_INPROCESS_DUMP=<path> in the environment writes the module there.

#include "runtime/NightInprocHost.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include <yurt_jit.h>

// The live reservation fd between Reserve and Install, or -1.
static int32_t gReservation = -1;
static uint32_t gReservedLen = 0;

static void JitError(const char* what, int32_t rc) {
  char msg[512] = {0};
  yurt_jit_last_error(msg, sizeof(msg) - 1);
  fprintf(stderr, "night: inprocess: %s: %s%s%s\n", what, strerror(-rc),
          msg[0] ? ": " : "", msg);
}

bool js::night::InprocHostReserve(uint32_t nblobs, uint32_t* tableBase) {
  // One reservation at a time: drop one still held rather than leak its fd
  // (and its table range) by overwriting it below.
  InprocHostRelease();
  struct yurt_jit_info info;
  int32_t fd = yurt_jit_reserve(nblobs, &info);
  if (fd < 0) {
    JitError("yurt_jit_reserve", fd);
    return false;
  }
  if (info.len < nblobs || info.base < 0) {
    fprintf(stderr,
            "night: inprocess: invalid reservation: base %d, %u slots "
            "for %u requested\n", info.base, info.len, nblobs);
    close(fd);
    return false;
  }
  gReservation = fd;
  gReservedLen = info.len;
  *tableBase = static_cast<uint32_t>(info.base);
  return true;
}

void js::night::InprocHostRelease() {
  if (gReservation >= 0) {
    close(gReservation);
    gReservation = -1;
  }
}

bool js::night::InprocHostInstall(night_inproc_out_t* out,
                                  uint32_t tableBase) {
  uint32_t nblobs = night_inproc_num_blobs(out);
  if (nblobs > gReservedLen) {
    // Refuse an inconsistent host session before publishing anything.
    fprintf(stderr,
            "night: inprocess: batch has %u blobs but only %u table slots were "
            "reserved\n",
            nblobs, gReservedLen);
    InprocHostRelease();
    return false;
  }
  uint32_t len = 0;
  const uint8_t* module = night_inproc_module(out, &len);
  if (!module) {
    InprocHostRelease();
    return false;
  }
  if (const char* path = getenv("NIGHT_INPROCESS_DUMP")) {
    FILE* f = fopen(path, "wb");
    if (!f || fwrite(module, 1, len, f) != len) {
      fprintf(stderr, "night: inprocess: cannot write %s\n", path);
    }
    if (f) {
      fclose(f);
    }
  }
  // The mount consumes the reservation fd whether or not it succeeds (and
  // the fd number may be reused at once), so it is never closed after this.
  int32_t reservation = gReservation;
  gReservation = -1;
  yurt_jit_module_t mod;
  int32_t rc =
      yurt_jit_mount_module_reserved(module, len, 0, reservation, &mod);
  if (rc < 0) {
    JitError("yurt_jit_mount_module_reserved", rc);
    return false;
  }
  struct yurt_jit_info info;
  rc = yurt_jit_query_module(mod, &info);
  if (rc < 0) {
    JitError("yurt_jit_query_module", rc);
  } else if (static_cast<uint32_t>(info.base) == tableBase &&
             info.len >= nblobs) {
    // Entry i is at INFO.base + i, as baked into the compiled code.
    return true;
  } else {
    fprintf(stderr,
            "night: inprocess: module mounted at table[%d..+%u], compiled for "
            "table[%u..+%u]\n",
            info.base, info.len, tableBase, nblobs);
  }
  // Nothing has been armed yet, but the mount published callable entries.
  // Unpublish them before the driver frees the batch and stays interpreted.
  rc = yurt_jit_unmount_module(mod);
  if (rc < 0) {
    JitError("unmounting invalid published module; module is still live", rc);
    abort();
  }
  return false;
}
