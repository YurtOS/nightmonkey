// A generator's forced return (it.return(v)) runs the finally block with v in
// the frame's return-value slot; a release store into that slot would drop it.
function foo(a) { return a; }
function* g3(k) {
  var o = { v: k };
  for (var i = 0; i < 2; i++) {
    try { foo(o); o = 5; yield i; }
    finally { gc(); o = { v: i }; foo(o); o = 1; }
  }
}
for (var n = 0; n < 40; n++) {
  var it = g3(n);
  it.next();
  assertEq(it.return(n).value, n);
  assertEq(it.next().done, true);
}
