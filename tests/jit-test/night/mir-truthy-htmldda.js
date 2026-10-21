// An object that emulates undefined (document.all) is falsy. A kind guard that
// proves "native object" must not fold ToBoolean to true.
function f(a) { a[0] = 1; return !!a; }
function g(a) { a[0] = 1; if (a) return 1; return 0; }

for (var i = 0; i < 30; i++) {
  assertEq(f([]), true);
  assertEq(f({}), true);
  assertEq(f(createIsHTMLDDA()), false);
  assertEq(g([]), 1);
  assertEq(g(createIsHTMLDDA()), 0);
}
