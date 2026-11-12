// Directory b. The AOT tier compiles only the primary script, so everything
// loaded from here runs on the interpreter (the direct eval would force that
// anyway): these are the INTERPRETED frames of caller-script.js's shapes.
function loadB() { eval(""); loadRelativeToScript("h.inc"); }
function callMainFromB() { eval(""); loadMain(); }           // interpreted -> compiled -> native
function forEachB() { eval(""); [1].forEach(loadMain); }     // interpreted -> self-hosted -> compiled callback -> native
function fnFileB() { eval(""); return new Function("return new Error().fileName")(); }
