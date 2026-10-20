// Prologue for scripts/test-guest-tier-options.py: user accessors on the
// builtins the compiler resolves at compile time (the self-hosted allowlist).
// Resolving them must not run these.
var getCount = 0;
Object.defineProperty(String.prototype, "split", { configurable: true, get() { getCount++; return function(){ return ["x"]; }; } });
Object.defineProperty(Array.prototype, "map", { configurable: true, get() { getCount++; return undefined; } });
