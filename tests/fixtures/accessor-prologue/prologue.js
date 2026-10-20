// Prologue for scripts/test-guest-tier-options.py: user accessors on the
// builtins the compiler resolves at compile time (the self-hosted allowlist).
// Resolving them must not run these.
var getCount = 0;
Object.defineProperty(String.prototype, "split", { configurable: true, get() { getCount++; return function(){ return ["x"]; }; } });
Object.defineProperty(Array.prototype, "map", { configurable: true, get() { getCount++; return undefined; } });
// The pristine builtins the install step reads for its fast paths.
var defineProperty = Object.defineProperty;
function counting(obj, key) {
  defineProperty(obj, key, { configurable: true, get() { getCount++; return function () {}; } });
}
counting(Array.prototype, "push");
counting(Array.prototype, "pop");
counting(Function.prototype, "call");
counting(Function.prototype, "apply");
counting(Object.prototype, "hasOwnProperty");
counting(RegExp.prototype, "exec");
counting(RegExp.prototype, "test");
counting(String.prototype, "charCodeAt");
counting(Math, "imul");
counting(Math, "floor");
counting(Map.prototype, "get");
counting(Object, "defineProperty");
counting(Reflect, "defineProperty");
counting(globalThis, "parseInt");
