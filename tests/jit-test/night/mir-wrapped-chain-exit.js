// A chain of int32 additions whose intermediate values are only truncated at
// the end is computed wrapped; a guard failure in the middle (here the setter
// leaves a non-integer in box.v) must still recover the exact sum, not the
// wrapped one.
function Box() { this.v = 1; }
var box = new Box();
var setter = { setv: function (x) { box.v = x; } };
function setByName(x) { setter[["set", "v"].join("")](x); }

function chain20(a) {
  var s = a | 0;
  s = s + s; s = s + s; s = s + s; s = s + s; s = s + s;
  s = s + s; s = s + s; s = s + s; s = s + s; s = s + s;
  s = s + s; s = s + s; s = s + s; s = s + s; s = s + s;
  s = s + s; s = s + s; s = s + s; s = s + s; s = s + s;
  return (s + box.v) | 0;
}
function chain9(a) {
  var s = a | 0;
  s = s + s; s = s + s; s = s + s; s = s + s; s = s + s;
  s = s + s; s = s + s; s = s + s; s = s + s;
  return (s + box.v) | 0;
}

for (var i = 0; i < 20; i++) {
  setByName(1);
  assertEq(chain20(1), (2 ** 20 + 1) | 0);
  setByName(0.5);
  assertEq(chain20(2147483647), -1048576);
  assertEq(chain9(2147483647), -512);
}
