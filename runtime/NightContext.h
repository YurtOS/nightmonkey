/* -*- Mode: C++; tab-width: 2; indent-tabs-mode: nil; c-basic-offset: 2 -*-
 * vim: set ts=8 sts=2 et sw=2 tw=80: */

// The tier's per-context state: what the ExternalCompilerHooks newContext
// hook makes for each JSContext (and destroyContext frees), reached through
// JSContext::getExternalCompilerState. Everything a context's compiled code
// and runtime helpers keep between calls belongs here, not in a global.

#ifndef night_runtime_NightContext_h
#define night_runtime_NightContext_h

#include "mozilla/Assertions.h"
#include "mozilla/Attributes.h"

#include <stdint.h>

#include "runtime/NightStack.h"
#include "vm/JSContext.h"  // JSContext::getExternalCompilerState

class JSLinearString;

namespace js {
class RegExpShared;

namespace night {
// runtime/NightRuntime.cpp's state: its caches, tables and flags.
struct NightRuntimeState;
NightRuntimeState* NewNightRuntimeState();
void DeleteNightRuntimeState(NightRuntimeState* state);
}  // namespace night

namespace nightrt {

// A successful exec() the compiled call's leaf decided (NightRegExpLeaf):
// its match pairs, so the generic path that builds the result object need
// not run the matcher again. Valid for the same shared and input, from
// position 0, while no GC has run (the pointers identify the objects only
// until one may move or free them).
struct NightLeafMatch {
  static constexpr uint32_t kMaxPairs = 16;
  RegExpShared* shared = nullptr;
  JSLinearString* input = nullptr;
  uint64_t gcNumber = 0;
  uint32_t pairCount = 0;  // 0: nothing cached
  int32_t pairs[2 * kMaxPairs] = {};
};

struct NightContextState {
  // The property-IC region's base (NightEnvDesc::propicPtr), at offset 0:
  // compiled code loads it from the state it reaches through its context
  // (JSContext::offsetOfExternalCompilerState) and addresses each site's
  // ways relative to it.
  uint32_t propIcBase = 0;
  NightStack stack;
  NightLeafMatch leafMatch;
  js::night::NightRuntimeState* runtime = nullptr;
  // How many compiled frames are executing on this context's native stack
  // (AutoLiveCompiledFrame, runtime/NightEntry.cpp). A suspended generator
  // does not count: its frame has exited. 159's js::CanSkipAwait asks for it
  // through ExternalCompilerHooks::externalFramesLive. Appended last: the
  // property-IC base must stay at offset 0.
  uint32_t liveFrames = 0;
  // The innermost compiled frame that is calling out, for the engine's
  // scripted-caller queries (ExternalCompilerHooks::innermostFrame; 159
  // only). Compiled frames sit on no engine Activation, so the engine's own
  // frame iterator cannot see them: loadRelativeToScript, indirect eval and
  // `new Function` called from compiled code would find no (or the wrong)
  // caller. `callerFrame` is that frame's base on the value stack (null:
  // none; its slot 0 is the callee or, for global code, the script, and is
  // traced and forwarded in place, so the script is read from it when asked
  // and a compacting GC cannot leave it stale); `callerActivation` is
  // cx->activation() when it was recorded, which tells the engine whether a
  // frame of its own was pushed after it. Set and restored by
  // AutoCallerFrame around the entries and the generic call helpers
  // (runtime/NightEntry.cpp, runtime/NightRuntime.cpp); a direct
  // compiled-to-compiled call touches neither. Appended after liveFrames:
  // the property-IC base must stay at offset 0.
  JS::Value* callerFrame = nullptr;
  js::Activation* callerActivation = nullptr;
};

// Records `frame` (the base of the compiled frame about to run or call out)
// as the innermost one for the lifetime of the guard, and restores the
// previous record after, so that once the outermost compiled frame returns
// the record is clear and an interpreted frame's native calls see none. A
// null `frame` (a fallback from a helper with no frame to name) leaves the
// record as it is.
class MOZ_RAII AutoCallerFrame {
  NightContextState* state_;
  JS::Value* savedFrame_ = nullptr;
  js::Activation* savedActivation_ = nullptr;
  bool active_;

 public:
  AutoCallerFrame(JSContext* cx, JS::Value* frame)
      : state_(static_cast<NightContextState*>(cx->getExternalCompilerState())),
        active_(frame != nullptr) {
    if (active_) {
      savedFrame_ = state_->callerFrame;
      savedActivation_ = state_->callerActivation;
      state_->callerFrame = frame;
      state_->callerActivation = cx->activation();
    }
  }
  ~AutoCallerFrame() {
    if (active_) {
      state_->callerFrame = savedFrame_;
      state_->callerActivation = savedActivation_;
    }
  }
  AutoCallerFrame(const AutoCallerFrame&) = delete;
  AutoCallerFrame& operator=(const AutoCallerFrame&) = delete;
};

}  // namespace nightrt
}  // namespace js

#endif  // night_runtime_NightContext_h
