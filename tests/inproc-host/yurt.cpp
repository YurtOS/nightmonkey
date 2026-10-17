// Native contract test for the production Yurt host TU. Link with an SDK
// include directory; only the guest hostcalls and compiler FFI are substituted.
#include "runtime/NightInprocHost.h"
#include <yurt_jit.h>
#include <assert.h>
#include <fcntl.h>
#include <unistd.h>

#ifdef NDEBUG
#error "The host contract test requires assertions enabled"
#endif

static uint32_t reservedLen = 3, blobCount = 2;
static int fd, mounts;
static int32_t mountRc;
static bool moduleAvailable = true;
static int32_t mountedBase = 40;
extern "C" int32_t yurt_jit_reserve(uint32_t, yurt_jit_info* info) {
  fd = dup(STDOUT_FILENO);
  *info = {40, reservedLen};
  return fd;
}
extern "C" int32_t yurt_jit_last_error(char*, uint32_t) { return 0; }
extern "C" uint32_t night_inproc_num_blobs(night_inproc_out_t*) { return blobCount; }
extern "C" const uint8_t* night_inproc_module(night_inproc_out_t*, uint32_t* len) {
  static uint8_t bytes[8] = {};
  *len = sizeof(bytes);
  return moduleAvailable ? bytes : nullptr;
}
extern "C" int32_t yurt_jit_mount_module_reserved(const void*, uint32_t,
    uint32_t, int32_t reservation, yurt_jit_module_t* mod) {
  assert(reservation == fd);
  close(reservation); // Kernel consumes even a failed mount.
  ++mounts;
  *mod = 123;
  return mountRc;
}
extern "C" int32_t yurt_jit_query_module(yurt_jit_module_t, yurt_jit_info* info) {
  *info = {mountedBase, reservedLen};
  return 0;
}
int main() {
  using namespace js::night;
  uint32_t base = 999;
  // A capped reservation must fail before batch compilation starts.
  assert(!InprocHostReserve(4, &base));
  assert(base == 999);
  assert(fcntl(fd, F_GETFD) == -1);
  InprocHostRelease();
  assert(mounts == 0);
  reservedLen = 4;
  assert(InprocHostReserve(4, &base) && base == 40);
  assert(InprocHostInstall(nullptr, base));
  InprocHostRelease();
  assert(mounts == 1);
  assert(InprocHostReserve(4, &base));
  mountRc = -22;
  assert(!InprocHostInstall(nullptr, base));
  // A consumed fd can be reused immediately; Release must leave it alone.
  int replacement = dup(STDOUT_FILENO);
  assert(replacement == fd);
  InprocHostRelease();
  assert(fcntl(replacement, F_GETFD) != -1);
  close(replacement);

  // Pre-mount failures release the reservation without attempting a mount.
  mountRc = 0;
  assert(InprocHostReserve(4, &base));
  blobCount = 5;
  assert(!InprocHostInstall(nullptr, base));
  assert(fcntl(fd, F_GETFD) == -1 && mounts == 2);
  blobCount = 2;
  assert(InprocHostReserve(4, &base));
  moduleAvailable = false;
  assert(!InprocHostInstall(nullptr, base));
  assert(fcntl(fd, F_GETFD) == -1 && mounts == 2);
  moduleAvailable = true;

  // Never arm code whose published range differs from its baked indices.
  assert(InprocHostReserve(4, &base));
  mountedBase = 41;
  assert(!InprocHostInstall(nullptr, base));
  mountedBase = 40;
  assert(InprocHostReserve(4, &base));
  reservedLen = 1;
  assert(!InprocHostInstall(nullptr, base));
  InprocHostRelease();
  assert(mounts == 4);
}
