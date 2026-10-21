// A constructor still in the nursery when `new` allocates `this`: the
// allocation may collect the nursery and move the constructor, so the cache
// fill after it must use the forwarded callee, not the pointer it was given.
function make(i) {
  function C(a) { this.a = a; this.b = i; }
  C.prototype.sum = function () { return this.a + this.b; };
  return C;
}
function build(i) {
  var C = make(i);
  var o = new C(i);
  var p = new C(i + 1);
  return o.sum() + p.sum();
}
var total = 0;
for (var i = 0; i < 200000; i++) total += build(i);
var expected = 0;
for (var i = 0; i < 200000; i++) expected += 4 * i + 1;
assertEq(total, expected);
