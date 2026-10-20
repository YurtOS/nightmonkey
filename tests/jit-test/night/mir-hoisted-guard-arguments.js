// A hoisted (loop-entry) guard whose exit comes before the arguments object
// is made: the exit must hand baseline the real `arguments`, not the elided
// placeholder (`undefined`), as every ordinary exit does.
function D(x) {
  var a = 0, d = 'z';
  for (var w = 0; w < 2; w++) { a = arguments[0]; a = d == null; }
  return a;
}
for (var n = 0; n < 40; n++) assertEq(D(n), false);
