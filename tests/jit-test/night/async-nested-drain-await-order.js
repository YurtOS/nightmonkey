// A job queue drained from inside compiled code (drainJobQueue(), as the
// Debugger's runJobs does after a hook) must not leave 159's await-skip
// permission set: an interpreted async generator resumed afterwards from
// that code would skip an await it must not. The generator uses a direct
// eval so the compiler leaves it interpreted. Order must be A (the
// caller continues) then B (the generator's resumption after its await).
// The same on 147, where the engine does not skip awaits this way.
// Not covered (engine-side fix needed, issue #24): a drain inside the
// resumed generator itself, or inside a helper after its entry.

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
