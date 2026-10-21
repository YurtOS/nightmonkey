// `x === k` where k is null on one path and undefined on another: the union
// {null, undefined} is not one constant, so the comparison must still tell
// them apart.
function eq(x, b) { var k = b ? null : undefined; return x === k; }
function ne(x, b) { var k = b ? null : undefined; return x !== k; }
function br(x, b) { var k = b ? null : undefined; if (x === k) return 1; return 0; }

for (var i = 0; i < 30; i++) {
  assertEq(eq(null, false), false);
  assertEq(eq(undefined, true), false);
  assertEq(eq(null, true), true);
  assertEq(eq(undefined, false), true);
  assertEq(ne(null, false), true);
  assertEq(ne(undefined, false), false);
  assertEq(br(null, false), 0);
  assertEq(br(undefined, false), 1);
}
