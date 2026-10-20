// A value a retaining store put in the frame (the local was live across a
// may-GC call) must not stay alive once its local is overwritten: the next GC
// releases it, as in baseline, whose frame slot is overwritten. An exit that
// treated the overwritten, dead local as "keep the frame's value" would keep a
// finalizable object, or a weak-map key, alive until the function returned.
function collect() { gc(); }

// `o` is live across the finalizeCount() call, so a retaining store puts it in
// the frame; it is then overwritten and dead at collect().
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
function overwriteFresh(x) {
  gc();
  var o = makeFinalizeObserver();
  var c0 = finalizeCount();
  o = { a: x };
  collect();
  return finalizeCount() - c0;
}
function overwriteParam(x) {
  gc();
  var o = makeFinalizeObserver();
  var c0 = finalizeCount();
  o = x;
  collect();
  return finalizeCount() - c0;
}
// Overwritten by a scalar: the local's old object must go too.
function overwriteInt() {
  gc();
  var o = makeFinalizeObserver();
  var c0 = finalizeCount();
  o = 5;
  collect();
  return finalizeCount() - c0;
}
function overwriteBool() {
  gc();
  var o = makeFinalizeObserver();
  var c0 = finalizeCount();
  o = true;
  collect();
  return finalizeCount() - c0;
}
function overwriteDouble() {
  gc();
  var o = makeFinalizeObserver();
  var c0 = finalizeCount();
  o = 1.5;
  collect();
  return finalizeCount() - c0;
}
// Overwritten by a scalar that is still read afterwards: the slot is live, but
// the frame must not keep the old object.
function overwriteLiveScalar() {
  gc();
  var o = makeFinalizeObserver();
  var c0 = finalizeCount();
  o = 5;
  var r = o + 1;
  o = 6;
  assertEq(r, 6);
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
// A value still held by its local must stay alive (no over-collection), and a
// dead local that is never overwritten keeps its object, as baseline does.
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
  assertEq(overwriteFresh(n), 1);
  assertEq(overwriteParam({}), 1);
  assertEq(overwriteInt(), 1);
  assertEq(overwriteBool(), 1);
  assertEq(overwriteDouble(), 1);
  assertEq(overwriteLiveScalar(), 1);
  assertEq(lastUseThenCall(n & 1), 1);
  assertEq(stillHeld(), 0);
}
