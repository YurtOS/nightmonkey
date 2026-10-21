// A suspended generator or async function keeps a dead local's last value
// alive, as baseline's and the interpreter's frame does (the frame holds the
// latest value of every local until it is assigned again). The suspend must
// save the frame's copy, not substitute `undefined` for a dead local.
function* g() { var go = makeFinalizeObserver(); yield 1; }
async function af() { var go = makeFinalizeObserver(); await 1; }
function* g2() {
  var go = makeFinalizeObserver();
  yield 1;
  go = 5;          // assigned: the object is released, whether or not it is read
  yield 2;
}
function suspendedGenerator() {
  gc();
  var c0 = finalizeCount();
  var it = g();
  it.next();
  gc();
  return finalizeCount() - c0;
}
function suspendedAsync() {
  gc();
  var c0 = finalizeCount();
  var p = af();
  gc();
  var held = finalizeCount() - c0;
  drainJobQueue();
  return held;
}
function assignedAfterYield() {
  gc();
  var c0 = finalizeCount();
  var it = g2();
  it.next();
  it.next();
  gc();
  return finalizeCount() - c0;
}
for (var n = 0; n < 20; n++) {
  assertEq(suspendedGenerator(), 0, "generator");
  assertEq(suspendedAsync(), 0, "async function");
  assertEq(assignedAfterYield(), 1, "local assigned after the first yield");
}
