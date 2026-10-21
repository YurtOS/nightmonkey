// `.length` of a typed array is its length slot only while the ordinary
// property lookup still reaches the builtin getter: an own `length` (data or
// accessor) on the instance must win. The kind guard alone does not prove that.
// Each function sees only the overridden array, so the analysis predicts a
// typed array there.
function lenData(a) { a[0] = 1; return a.length; }
function lenAccessor(a) { a[0] = 1; return a.length; }
function lenPlain(a) { a[0] = 1; return a.length; }

var data = new Int32Array(2);
Object.defineProperty(data, "length", { value: 40 });
var acc = new Int8Array(2);
Object.defineProperty(acc, "length", { get() { return 7; } });
var plain = new Int32Array(3);

for (var i = 0; i < 30; i++) {
  assertEq(lenData(data), 40);
  assertEq(lenAccessor(acc), 7);
  assertEq(lenPlain(plain), 3);
}
