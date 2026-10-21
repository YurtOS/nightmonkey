// A strict arrow with no bindings containing direct eval has no environment,
// but the eval reads the caller's interpreter frame, which an inlined callee
// does not have: it must not be inlined into a caller. The tier check is in
// scripts/test-guest-tier-options.py (no `call.eval` in `caller`'s MIR).
"use strict";
var inner = () => eval("1 + 1");
function caller(n) {
  var s = 0;
  for (var i = 0; i < n; i++) s += inner();
  return s;
}
var total = 0;
for (var k = 0; k < 50; k++) total += caller(20);
assertEq(total, 2000);
