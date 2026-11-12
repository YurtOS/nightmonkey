// Forced return (it.return(v)) and throw (it.throw(e)) through generator bodies.
//
// On 147 a forced return unwinds as a pending JS_GENERATOR_CLOSING exception
// that catches skip, finallys run and the error epilogue turns into a normal
// return (compiler GEN_CLOSING_UNWIND = true). 159 resumes it with ordinary
// bytecode (GEN_CLOSING_UNWIND = false), so the compiled body unwinds like any
// other. The language semantics are the same on both, and the interpreter lane
// runs the same assertions, so these are the spec's answers, not the tier's.
var log;
function check(r, value, done) {
  assertEq(r.value, value);
  assertEq(r.done, done);
}

// A forced return is not an exception: catch does not run, finally does, and
// nothing after the try does.
function* g1() {
  try { log.push("a"); yield 1; log.push("no"); }
  catch (e) { log.push("caught"); }
  finally { log.push("fin"); }
  log.push("after");
}
log = [];
var it = g1();
check(it.next(), 1, false);
check(it.return(7), 7, true);
assertEq(log.join(), "a,fin");
check(it.next(), undefined, true);

// A return in the finally overrides the forced return's value.
function* g2() { try { yield 1; } finally { return "f"; } }
it = g2();
it.next();
check(it.return(7), "f", true);

// A throw in the finally replaces it, and the generator is finished after.
function* g3() { try { yield 1; } finally { throw "boom"; } }
it = g3();
it.next();
var thrown = null;
try { it.return(7); } catch (e) { thrown = e; }
assertEq(thrown, "boom");
check(it.next(), undefined, true);

// A yield in the finally suspends the forced return; the next resume finishes
// the finally and then completes the return.
function* g4() {
  try { yield 1; }
  finally { log.push("f1"); yield 2; log.push("f2"); }
  log.push("after");
}
log = [];
it = g4();
it.next();
check(it.return(7), 2, false);
check(it.next(), 7, true);
assertEq(log.join(), "f1,f2");

// Finallys run innermost first.
function* g5() {
  try { try { yield 1; } finally { log.push("inner"); } }
  finally { log.push("outer"); }
}
log = [];
it = g5();
it.next();
check(it.return(3), 3, true);
assertEq(log.join(), "inner,outer");

// A throw() IS an exception: catch runs, the generator continues.
function* g6() {
  try { yield 1; }
  catch (e) { log.push("c:" + e); yield 2; }
  finally { log.push("fin"); }
  return 3;
}
log = [];
it = g6();
it.next();
check(it.throw("x"), 2, false);
check(it.next(), 3, true);
assertEq(log.join(), "c:x,fin");

// Returning before the body starts runs none of it.
function* g7() { log.push("body"); yield 1; }
log = [];
it = g7();
check(it.return(5), 5, true);
check(it.next(), undefined, true);
assertEq(log.length, 0);

// A for-of in the body closes its iterator on a forced return.
var closed = 0;
var iterable = {
  [Symbol.iterator]() {
    return { next() { return { value: 1, done: false }; },
             return() { closed++; return {}; } };
  }
};
function* g8() { for (var x of iterable) { yield x; } }
it = g8();
it.next();
check(it.return(9), 9, true);
assertEq(closed, 1);

// An ordinary return in a try still runs its finally.
function* g9() { try { return 1; } finally { log.push("f"); } }
log = [];
check(g9().next(), 1, true);
assertEq(log.join(), "f");

// An exception thrown and caught inside the finally of a forced return is
// an ordinary exception: it is caught, and the return completes.
function* g10() {
  try { yield 1; }
  finally { try { throw 1; } catch (e) { log.push("in-fin-caught"); } }
}
log = [];
it = g10();
it.next();
check(it.return(7), 7, true);
assertEq(log.join(), "in-fin-caught");

// Repeated, so a tier that compiles after a few runs is covered too.
for (var n = 0; n < 50; n++) {
  log = [];
  it = g1();
  it.next();
  check(it.return(n), n, true);
  assertEq(log.join(), "a,fin");
}
