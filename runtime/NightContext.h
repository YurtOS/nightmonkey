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

class AutoCallerFrame;

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
  // only): the top of a chain of AutoCallerFrame guards, newest first (null:
  // none). Compiled frames sit on no engine Activation, so the engine's own
  // frame iterator cannot see them: loadRelativeToScript, indirect eval and
  // `new Function` called from compiled code would find no (or the wrong)
  // caller. Each guard holds a frame base on the value stack (its slot 0 is
  // the callee or, for global code, the script, and is traced and forwarded
  // in place, so the script is read from it when asked and a compacting GC
  // cannot leave it stale) and cx->activation() when it was recorded, which
  // tells the engine whether a frame of its own was pushed after it. Pushed
  // by the entries and the generic call helpers (runtime/NightEntry.cpp,
  // runtime/NightRuntime.cpp); a direct compiled-to-compiled call pushes
  // nothing. Appended after liveFrames: the property-IC base must stay at
  // offset 0.
  AutoCallerFrame* callerTop = nullptr;
};

// Null when the context has no state (the hooks are not installed, or the
// state could not be allocated): no compiled code runs on such a context.
inline NightContextState* NightStateOf(JSContext* cx) {
  return static_cast<NightContextState*>(cx->getExternalCompilerState());
}

// The context's value stack. Only for a context that runs compiled code
// (whose state exists: every entry checks NightStateOf first).
inline NightStack& TheNightStackInline(JSContext* cx) {
  NightContextState* state = NightStateOf(cx);
  MOZ_ASSERT(state);
  return state->stack;
}

// Records `frame` (the base of the compiled frame about to run or call out)
// as the innermost one for the lifetime of the guard: it is pushed on the
// context's chain and popped on exit, so once the outermost compiled frame
// returns the chain is empty and an interpreted frame's native calls see
// none. A null `frame` (a fallback from a helper with no frame to name)
// pushes nothing.
class MOZ_RAII AutoCallerFrame {
  NightContextState* state_;
  AutoCallerFrame* prev_ = nullptr;
  JS::Value* frame_;
  js::Activation* activation_ = nullptr;

 public:
  AutoCallerFrame(JSContext* cx, JS::Value* frame)
      : state_(NightStateOf(cx)), frame_(frame) {
    if (frame_) {
      prev_ = state_->callerTop;
      activation_ = cx->activation();
      state_->callerTop = this;
    }
  }
  ~AutoCallerFrame() {
    if (frame_) {
      state_->callerTop = prev_;
    }
  }
  AutoCallerFrame(const AutoCallerFrame&) = delete;
  AutoCallerFrame& operator=(const AutoCallerFrame&) = delete;

  JS::Value* frame() const { return frame_; }
  js::Activation* activation() const { return activation_; }
  // The guard that was innermost when this one was pushed.
  AutoCallerFrame* prev() const { return prev_; }
};

}  // namespace nightrt
}  // namespace js

#endif  // night_runtime_NightContext_h
