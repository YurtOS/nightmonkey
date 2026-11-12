// A native that asks the engine who called it (loadRelativeToScript; indirect
// eval and `new Function`, which name the caller in the new script's filename)
// must be told the innermost frame, whichever tier it runs on. Compiled frames
// sit on no engine activation, so the engine's own frame iterator cannot see
// them (patch 0015, the innermostFrame hook): before it, a compiled caller got
// "unable to resolve path" and an interpreted frame below a compiled one was
// reported in its place. Needs the engine hook (159); the same assertions are
// the language's answer in the interpreter lane. A 147 build has no hook, so
// tests/night-tests-excludes-ff147.txt skips this test there. See issue #28.
//
// The AOT tier compiles only the primary script (this file); what it loads at
// run time stays interpreted. So the COMPILED callers are defined here, and the
// INTERPRETED ones in caller-script-data/b/def.inc. A compiled caller resolves
// "h.inc" against this file's directory ("main"), an interpreted one against
// b/ ("b"): which hits are pushed says which frame the engine reported.
var hits = [];
loadRelativeToScript("caller-script-data/b/def.inc");

function loadMain() { loadRelativeToScript("caller-script-data/main/h.inc"); }
function loadMainSpread() { var a = ["caller-script-data/main/h.inc"]; loadRelativeToScript(...a); }
function chainMain() { loadMain(); loadMain(); }              // compiled -> compiled -> native
function fnFileMain() { return new Function("return new Error().fileName")(); }
function evalFileMain() { return (0, eval)("new Error().fileName"); }
function viaMain(cb) { return cb(); }                         // compiled -> whatever
function* genMain() { yield 1; loadRelativeToScript("caller-script-data/main/h.inc"); yield 2; }  // a resumed compiled generator calling out itself

function expect(name, want, fn) {
  hits.length = 0;
  fn();
  assertEq(hits.join(), want, name);
}
function names(file, needle) { assertEq(file.includes(needle), true, file); }

// The innermost frame is the answer, for every mix of tiers.
expect("compiled caller", "main", () => loadMain());
expect("compiled spread call", "main", () => loadMainSpread());
expect("compiled -> compiled -> native, twice", "main,main", () => chainMain());
expect("interpreted caller", "b", () => loadB());
// The guard must pick the compiled frame over the interpreted one BELOW it ...
expect("interpreted -> compiled", "main", () => callMainFromB());
expect("interpreted -> self-hosted -> compiled callback", "main", () => forEachB());
// ... and the interpreted frame ABOVE a compiled one over it.
expect("compiled -> interpreted", "b", () => viaMain(loadB));
expect("compiled -> compiled -> interpreted", "b", () => viaMain(() => viaMain(loadB)));
// A compiled generator resumed from C++ (its frame is entered by the resume).
var it = genMain();
it.next();
expect("resumed compiled generator", "main", () => it.next());
// A native called by a compiled self-hosted builtin (map is on the compile
// list) is attributed to the user frame below it, not to map.
names(["new Error().fileName"].map(eval)[0], "caller-script.js");

// Many in a row: the record is restored after each nested script, not left
// pointing at one that has exited.
for (var i = 0; i < 20; i++) {
  expect("repeat compiled " + i, "main", () => loadMain());
  expect("repeat interpreted " + i, "b", () => loadB());
}

// Function and indirect eval name the caller in the new script's filename.
names(fnFileMain(), "caller-script.js");
names(evalFileMain(), "caller-script.js");
names(fnFileB(), "caller-script-data/b/def.inc");
names(viaMain(fnFileB), "caller-script-data/b/def.inc");
names(viaMain(fnFileMain), "caller-script.js");
