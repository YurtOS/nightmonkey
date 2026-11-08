// A job queue drained from inside compiled code (drainJobQueue(), as the
// Debugger's runJobs does after a hook) must not leave 159's await-skip
// permission set: an interpreted async generator resumed afterwards from
// that code would skip an await it must not. The generator uses a direct
// eval so the compiler leaves it interpreted. Order must be A (the
// caller continues) then B (the generator's resumption after its await).
// The same on 147, where the engine does not skip awaits this way.

var log = [];
async function* g() { eval(""); await 0; log.push("B"); }

function run() {
  Promise.resolve().then(() => {});
  drainJobQueue();
  g().next();
  log.push("A");
}
run();
drainJobQueue();
assertEq(log.join(","), "A,B");

// The same from a compiled async function resumed by a job (a nested drain
// is ignored there, but the order must hold all the same).
var log2 = [];
async function* g2() { eval(""); await 0; log2.push("B"); }
async function main() {
  await null;
  Promise.resolve().then(() => {});
  drainJobQueue();
  g2().next();
  log2.push("A");
}
main();
drainJobQueue();
assertEq(log2.join(","), "A,B");
