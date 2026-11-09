// A call with as many arguments as the engine allows (getMaxArgs()) does not
// fit on the AOT value stack even when it is empty. It must run on the
// interpreter, as it does with in-process compiling off, not fail with
// "too much recursion". Real over-recursion must still be an InternalError.

var n = getMaxArgs();
function f() { return arguments.length + "," + arguments[arguments.length - 1]; }

// A sparse array (holes), as non262/Array/regress-350256-02.js builds.
var sparse = new Array();
sparse[n - 2] = "n-2";
sparse[n - 1] = "n-1";
assertEq(f.apply(null, sparse), n + ",n-1");
assertEq(Array.apply(null, sparse).length, n);

// A dense array.
var dense = new Array(n);
for (var i = 0; i < n; i++) dense[i] = i;
assertEq(f.apply(null, dense), n + "," + (n - 1));

// Plain recursion still runs out of stack.
function r(k) { return r(k + 1) + 1; }
var threw = false;
try { r(0); } catch (e) { threw = e instanceof InternalError; }
assertEq(threw, true);

// A generator or async function is never started on the interpreter by this
// path (a script keeps one tier for its whole run): either the call raises
// over-recursion (compiled lane) or it runs (interpreter lane), and what runs
// resumes correctly.
function* gen() { var a = yield arguments.length; return a; }
async function af() { await 0; return arguments.length; }
var gres = null;
try {
  var it = gen.apply(null, sparse);
  assertEq(it.next().value, n);
  assertEq(it.next(7).value, 7);
  gres = "ran";
} catch (e) { assertEq(e instanceof InternalError, true); gres = "threw"; }
var ares = null, aval = -1;
try {
  af.apply(null, sparse).then(v => { aval = v; });
  ares = "ran";
} catch (e) { assertEq(e instanceof InternalError, true); ares = "threw"; }
drainJobQueue();
assertEq(gres === "ran" || gres === "threw", true);
assertEq(ares === "ran" || ares === "threw", true);
if (ares === "ran") assertEq(aval, n);

// One more argument than the engine allows is a RangeError in every lane.
var tooMany = false;
try { f.apply(null, new Array(n + 1).fill(0)); } catch (e) { tooMany = e instanceof RangeError; }
assertEq(tooMany, true);
