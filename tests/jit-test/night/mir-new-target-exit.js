// new.target read after a guard-failure exit (the baseline frame must hold
// the entry's new.target, as forwarded by any collection since).
function F(x) {
  var a = x + 1;
  minorgc();
  var t = new.target;
  if (typeof x === "string") a = x + "!";
  minorgc();
  return [t === G, a];
}
function G() {}
var seen = 0;
for (var i = 0; i < 2000; i++) {
  var r = Reflect.construct(F, [i < 1990 ? i : "s"], G);
  var o = Array.isArray(r) ? r : [true, 0];
  if (o[0]) seen++;
}
assertEq(seen, 2000);
