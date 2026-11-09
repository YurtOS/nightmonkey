// A job queue drained from compiled code or from the generator itself
// (drainJobQueue(), as the Debugger's runJobs does after a hook) must not
// make an interpreted async generator skip an await. 159's js::CanSkipAwait
// skips when the job queue looks empty and the awaiting frame is alone on
// the context; compiled frames sit on no engine activation, so without the
// externalFramesLive hook (patch 0013) a generator resumed from one looked
// alone. Every case here must log A (the caller continues) before B (the
// generator's resumption after its await). The generators use a direct eval
// so the compiler leaves them interpreted. The same on 147, where the engine
// does not skip awaits this way.

function check(name, log) { assertEq(log.join(","), "A,B", name); }

// 1. The drain returns before the generator is resumed.
var log1 = [];
async function* g1() { eval(""); await 0; log1.push("B"); }
function run1() {
  Promise.resolve().then(() => {});
  drainJobQueue();
  g1().next();
  log1.push("A");
}
run1();
drainJobQueue();
check("drain before resume", log1);

// 2. The drain runs inside the resumed generator, before its await.
var log2 = [];
async function* g2() { eval(""); drainJobQueue(); await 0; log2.push("B"); }
function run2() {
  Promise.resolve().then(() => {});
  g2().next();
  log2.push("A");
}
run2();
drainJobQueue();
check("drain inside the generator", log2);

// 3. A compiled callee drains and returns into the running generator.
var log3 = [];
function drainer() { Promise.resolve().then(() => {}); drainJobQueue(); }
async function* g3() { eval(""); drainer(); await 0; log3.push("B"); }
function run3() {
  g3().next();
  log3.push("A");
}
run3();
drainJobQueue();
check("compiled callee drains", log3);

// 4. The drain happens in a getter a helper runs, then the same helper
// resumes the generator (through a bound next).
var log4 = [], it4;
async function* g4() { eval(""); await 0; log4.push("B"); }
var o4 = {
  get [Symbol.toPrimitive]() {
    var f = it4.next.bind(it4);
    Promise.resolve().then(() => {});
    drainJobQueue();
    return f;
  }
};
function run4() {
  it4 = g4();
  try { "" + o4; } catch (e) {}
  log4.push("A");
}
run4();
drainJobQueue();
check("getter drains then resumes", log4);
