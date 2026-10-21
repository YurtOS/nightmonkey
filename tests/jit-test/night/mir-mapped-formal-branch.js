// A sloppy function whose `arguments` aliases its formals, writing a formal on
// only one path: the written formal is read back from the frame at the join.
// The MIR builder panicked (aborting the whole guest) on the forward edge.
function oneArm(c) { arguments; if (c) c = 5; return c; }
function viaAnd(b, c) { arguments[0]; var l = (l = String(c)) && (c = b); return [c, l]; }
function ifElse(a, b) {
  if (typeof a === "number") a = a | 0; else a = 0;
  return [a, b, arguments[0], arguments[1]];
}
function whileLoop(a, n) { arguments; while (n-- > 0) { if (n & 1) a = n; } return a; }
for (var i = 0; i < 40; i++) {
  assertEq(oneArm(3), 5);
  assertEq(oneArm(0), 0);
  assertEq(viaAnd(7, 1)[0], 7);
  assertEq(ifElse(2.5, "b")[0], 2);
  assertEq(ifElse("s", "b")[2], 0);
  assertEq(whileLoop(9, 4), 1);
}
