// A function containing direct eval is not compiled: the eval reads the
// caller's interpreter frame (its `arguments` object), which a compiled frame
// does not carry. Compiled, `arguments` was null inside the eval.
var a;
function f(x) {
  x = eval("a = arguments.callee.arguments; 10");
  return x;
}
function g() {
  with (g) return eval("arguments[0]");
}
function h(p, q) {
  return eval("arguments.length + p + q");
}
for (var i = 0; i < 20; i++) {
  assertEq(f(5), 10);
  assertEq(a[0], 10);
  assertEq(g(7), 7);
  assertEq(h(1, 2, 3), 6);
}
