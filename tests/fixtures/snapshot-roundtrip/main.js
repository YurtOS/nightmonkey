// Snapshot round-trip program (scripts/snapshot-roundtrip.sh).
// The top level runs while the shell is wizened, so this state is captured
// in the snapshot image; the restored module calls main(), which reads and
// mutates it. Print nothing at the top level: that output goes to the
// wizening process, not to the restored module.
class Counter {
  constructor() { this.n = 0; }
  bump(k) { this.n += k; return this.n; }
}
var counter = new Counter();
var table = new Map([["a", 1], ["b", 2]]);
var primes = [];
for (let i = 2; primes.length < 10; i++) {
  if (primes.every(p => i % p !== 0)) primes.push(i);
}
var makeAdder = k => x => x + k;
var add5 = makeAdder(5);
function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }

function main() {
  print("primes " + primes.join(","));
  var pairs = [];
  for (var [key, value] of table) pairs.push(key + "=" + value);
  print("table " + pairs.join(","));
  for (let i = 1; i <= 3; i++) counter.bump(i);
  print("counter " + counter.n);
  print("adder " + add5(37));
  print("fib " + fib(20));
  Promise.resolve(7).then(v => print("job " + v));
}
