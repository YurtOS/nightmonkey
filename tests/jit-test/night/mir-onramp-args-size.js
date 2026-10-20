// A constructor whose exits all materialise the elided `arguments` object:
// an inner-loop onramp side-entering the loops around it made waffle's
// reducifier copy that code until the function body was 53x larger (13.7 MB:
// over the wasm body-size limit, or out of memory). Run strict, the batch
// install failing fails this test.
var s = "", j0 = 0, j2 = 0, a, b, c, d, i0, i1, i2, e, l1;
function foo(x) { return x; }
function inl2(x) { return x; }
function* gen(n) { for (var i = 0; i < n; i++) yield i; }
class K2 { constructor(x, m) { wrap: { try { } finally { [a] = [b === 5 ? 1 : x]; } if (m & 1) { lb1: for (i1 = 0; i1 < 2; i1++) { c = new (class { f1 = 1.5; f2 = this.f1; static sf = a; })(); var {p1: a, p2: [b, ...a]} = {p1: 1.5, p2: [5, -c, 1]}; if (m & 4) break lb1; } for (i1 = 0; i1 < 3; i1++) { for (i2 = 0; i2 < 3; i2++) { } a = {v:i1}  } } else { var {p1: a, p2: [b, ...a]} = {p1: foo(c), p2: [{v: x}, 's', 1]}; for (c of [foo(d), [x]]) { for (b in {a:1,b:2}) { ({v: d}); } } } try { { c = (function(){ })(); for (var gv of gen(2)) { a = inl2(d); foo(a); s += gv; } s += typeof l1; } try { } finally { switch (m & 3) { } } for (i1 = 0; i1 < 1; i1++) { b = b; } } finally { a = arguments.length + (arguments[0] === x ? 1 : 0); s += typeof a; s += String(typeof b) + (b && b.v !== undefined ? b.v : (typeof b === 'object' ? 'o' : String(b))); } if (m & 2) { foo(x); } else { try { for (i2 = 0; i2 < 3; i2++) { s += String(typeof a) + (a && a.v !== undefined ? a.v : (typeof a === 'object' ? 'o' : String(a))); a = {v:i2}; gc(); a = undefined; } while (j2++ < 1); } finally { b = d === 5 ? 1 : x; s += typeof l1; } if (m & 1) { b = b + 1; finalizeCount(); } else { switch (m & 3) { case 0: gc(); default: foo(a); foo(a); } } b = new (class { f1 = a; f2 = this.f1; static sf = b; })(); } while (j0++ < 2); } s += typeof a + typeof b + typeof c + typeof d; this.r = s; } }
var r = 0;
for (var m = 0; m < 16; m++) {
  try { var k = new K2(m, m); r += typeof k.r === "string" ? 1 : 0; } catch (err) { r += 100; }
}
assertEq(typeof r, "number");
