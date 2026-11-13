// `x === k` and `x !== k` against an immediate constant (StrictConstantEq/Ne): the
// operand carries the engine's JSValueType number for undefined, null, booleans and
// int8s, which differs between engine versions (160 renumbered it). A compiler that
// decodes it with another version's numbers answers wrongly for undefined, booleans
// and ints (a default parameter is never applied, `a === 0` is never true).
function eqUndef(a) { return a === undefined; }
function neUndef(a) { return a !== undefined; }
function eqNull(a) { return a === null; }
function eqTrue(a) { return a === true; }
function eqFalse(a) { return a === false; }
function eqZero(a) { return a === 0; }
function eqSeven(a) { return a === 7; }
function eqNeg(a) { return a === -3; }
function defaulted(a = 5) { return a + 1; }
var arrow = (a = 0) => a + 1;

function check() {
  assertEq(eqUndef(), true);
  assertEq(eqUndef(undefined), true);
  assertEq(eqUndef(null), false);
  assertEq(eqUndef(0), false);
  assertEq(neUndef(), false);
  assertEq(neUndef(1), true);
  assertEq(eqNull(null), true);
  assertEq(eqNull(undefined), false);
  assertEq(eqTrue(true), true);
  assertEq(eqTrue(1), false);
  assertEq(eqTrue(false), false);
  assertEq(eqFalse(false), true);
  assertEq(eqFalse(0), false);
  assertEq(eqZero(0), true);
  assertEq(eqZero(-0), true);
  assertEq(eqZero(0.5), false);
  assertEq(eqZero(false), false);
  assertEq(eqZero(undefined), false);
  assertEq(eqSeven(7), true);
  assertEq(eqSeven(7.0), true);
  assertEq(eqSeven(8), false);
  assertEq(eqNeg(-3), true);
  assertEq(eqNeg(3), false);
  assertEq(defaulted(), 6);
  assertEq(defaulted(1), 2);
  assertEq(arrow(), 1);
  assertEq(arrow(50), 51);
}
check();
for (var i = 0; i < 200; i++) check();
