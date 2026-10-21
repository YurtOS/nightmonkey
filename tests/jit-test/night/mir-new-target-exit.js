// new.target read after a speculation-failure exit that follows a collection: the
// baseline frame must hold the entry's new.target as the collection
// forwarded it, not the entry parameter's pre-collection copy.
var nt;
var objs = [];
for (var i = 0; i < 2000; i++) objs.push(i < 1990 ? { p: i } : { q: 1, p: i });
function F(k) {
  minorgc();
  var o = objs[k];
  var p = o.p; // a speculation exit follows the minorgc above
  minorgc();
  var t = new.target;
  return [t === nt, p];
}
var seen = 0, sum = 0;
for (var i = 0; i < 2000; i++) {
  nt = function () {}; // a fresh nursery function each time
  var r2 = Reflect.construct(F, [i], nt);
  if (r2[0]) seen++;
  sum += r2[1] - i;
}
assertEq(seen, 2000);
assertEq(sum, 0);
