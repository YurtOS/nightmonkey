// A value rooted in a home slot (it was live across a may-GC call) and then
// dead must not stay alive: overwriting its local releases the object at the
// next GC, as in baseline, whose frame slot is overwritten. The slot's stale
// copy would otherwise keep a finalizable object, or a weak-map key, alive.
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
function overwriteObject() {
  gc();
  var o = makeFinalizeObserver();
  var c0 = finalizeCount();
  o = {};
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
  assertEq(overwriteObject(), 1);
  assertEq(lastUseThenCall(n & 1), 1);
  assertEq(stillHeld(), 0);
}
