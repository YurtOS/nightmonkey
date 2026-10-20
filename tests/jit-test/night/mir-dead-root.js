// A value a retaining store put in the frame (the local was live across a
// may-GC call) must not stay alive once its local is overwritten: the next GC
// releases it, as in baseline, whose frame slot is overwritten. An exit that
// treated the overwritten, dead local as "keep the frame's value" would keep a
// finalizable object, or a weak-map key, alive.
//
// Known limit: overwriting with a non-constant (`o = {}`) is released only at
// the next retaining store, not at an exit before it.
function collect() { gc(); }

// `o` is live across the finalizeCount() call, so it has a home slot; it is
// then overwritten and dead at collect().
function overwriteUndefined() {
  gc();
  var o = makeFinalizeObserver();
  var c0 = finalizeCount();
  o = undefined;
  collect();
  return finalizeCount() - c0;
}
// Dead through a branch rather than an overwrite: the last use is before the
// second call.
function lastUseThenCall(flag) {
  gc();
  var o = makeFinalizeObserver();
  var c0 = finalizeCount();
  var keep = flag ? o : null;
  keep = null;
  o = null;
  collect();
  return finalizeCount() - c0;
}
// A value still held by its local must stay alive (no over-collection).
function stillHeld() {
  gc();
  var o = makeFinalizeObserver();
  var c0 = finalizeCount();
  collect();
  var n = finalizeCount() - c0;
  return n + (o === undefined ? 1 : 0);
}
for (var n = 0; n < 40; n++) {
  assertEq(overwriteUndefined(), 1);
  assertEq(lastUseThenCall(n & 1), 1);
  assertEq(stillHeld(), 0);
}
