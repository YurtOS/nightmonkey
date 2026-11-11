// A native that asks the engine who called it (loadRelativeToScript; indirect
// eval and `new Function`, which name the caller in the new script's filename)
// must be told the innermost frame, whichever tier it runs on. Compiled frames
// sit on no engine activation, so the engine's own frame iterator cannot see
// them (patch 0015, the innermostFrame hook): before it, a compiled caller got
// "unable to resolve path" and an interpreted frame below a compiled one was
// reported in its place. Needs the engine hook (159); the same assertions are
// the language's answer in the interpreter lane. See issue #28.
var hits = [];
loadRelativeToScript("caller-script-data/a/def.inc");
loadRelativeToScript("caller-script-data/b/def.inc");

function expect(name, want, fn) {
  hits.length = 0;
  fn();
  assertEq(hits.join(), want, name);
}

// The innermost frame is the answer, for every mix of tiers.
expect("compiled caller", "a", () => loadA());
expect("compiled spread call", "a", () => loadASpread());
expect("compiled -> compiled -> native, twice", "a,a", () => chainA());
expect("interpreted caller", "b", () => loadB());
// The guard must pick the compiled frame over the interpreted one BELOW it ...
expect("interpreted -> compiled", "a", () => callAFromB());
expect("interpreted -> self-hosted -> compiled callback", "a", () => forEachB());
// ... and the interpreted frame ABOVE a compiled one over it.
expect("compiled -> interpreted", "b", () => viaA(loadB));
expect("compiled -> compiled -> interpreted", "b", () => viaA(() => viaA(loadB)));

// Many in a row: the record is restored after each nested script, not left
// pointing at one that has exited.
for (var i = 0; i < 20; i++) {
  expect("repeat compiled " + i, "a", () => loadA());
  expect("repeat interpreted " + i, "b", () => loadB());
}

// Function and indirect eval name the caller in the new script's filename.
function names(file, dir) {
  assertEq(file.includes("caller-script-data/" + dir + "/def.inc"), true, file);
}
names(fnFileA(), "a");
names(evalFileA(), "a");
names(fnFileB(), "b");
names(viaA(fnFileB), "b");
names(viaA(fnFileA), "a");
