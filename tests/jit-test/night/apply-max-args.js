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
