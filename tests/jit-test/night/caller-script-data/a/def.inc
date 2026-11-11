// Directory a: these functions are compiled by the AOT tier. Each asks the
// engine who called a native (loadRelativeToScript resolves "h.inc" against
// the caller's directory; Function and indirect eval name the caller in the
// script's filename), so "a" in `hits` / this file's name in the filename
// means the engine found THIS function.
function loadA() { loadRelativeToScript("h.inc"); }
function loadASpread() { var args = ["h.inc"]; loadRelativeToScript(...args); }
function chainA() { loadA(); loadA(); }                  // compiled -> compiled -> native
function fnFileA() { return new Function("return new Error().fileName")(); }
function evalFileA() { return (0, eval)("new Error().fileName"); }
function viaA(cb) { return cb(); }                        // compiled -> whatever
