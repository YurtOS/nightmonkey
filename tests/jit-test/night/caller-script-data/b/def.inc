// Directory b: the direct eval keeps each of these on the interpreter.
function loadB() { eval(""); loadRelativeToScript("h.inc"); }
function callAFromB() { eval(""); loadA(); }              // interpreted -> compiled -> native
function forEachB() { eval(""); [1].forEach(loadA); }     // interpreted -> self-hosted -> compiled callback -> native
function fnFileB() { eval(""); return new Function("return new Error().fileName")(); }
