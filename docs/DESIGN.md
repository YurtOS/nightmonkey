# NightMonkey design

NightMonkey is a compiler from SpiderMonkey bytecode to WebAssembly. It runs
ahead of time (the snapshot flow) or inside the running instance (the
in-process flow, which is how Yurt uses it as a JIT). SpiderMonkey and compiled
JavaScript execute in the same Wasm module and linear memory, so compiled
bodies can call engine helpers, inspect engine data structures, and call one
another.

This document describes the current implementation. It separates correctness
contracts from policy: limits and predictions may reduce coverage or
performance, but must never be required for correct execution.

## 1. Scope and deployment contract

NightMonkey consumes a closed snapshot of scripts and selected heap objects.
It does not consume execution profiles. The intended deployment is one
wasm32-wasi SpiderMonkey runtime, one active `JSContext`, and one installed AOT
environment per process.

Those restrictions are currently embedding assumptions, not fully enforced API
properties. Most AOT runtime state is process-global. An embedding must not
install two environments or activate NightMonkey in two runtimes. The
module-wide BigInt optimization also assumes values cannot enter from source or
an embedder outside the captured program. Until enforced or removed, these are
part of the trusted deployment boundary.

A compilation unit is a registered script tree plus selected self-hosted
scripts and regular-expression programs. A script is compiled as a whole or
remains interpreted. Unsupported operations, environments, limits, or
translation failures never produce partially compiled scripts.

## 2. End-to-end flow

Two deployment modes use the same analysis and translator.

### Snapshot flow

1. The wasm32-wasi shell compiles and registers script roots.
2. Registration delazifies reachable function trees and serializes information
   an external memory reader cannot safely derive.
3. Wizer captures the initialized module.
4. The host `nightmonkey` tool reads the registration block and snapshot,
   constructs `Source`, runs analysis, appends compiled functions, lays out AOT
   data, and patches function indices and object stamps.
5. On resume, `JS::NightActivate` installs the environment and enables AOT
   dispatch.

### In-process flow

The compiler runs inside the Wasm instance (the `night-guest` staticlib linked
into the shell); the host only provides table slots and installs a module.

1. The wasm shell captures live scripts and heap objects into `Source`
   (`--night-inprocess`; `NIGHT_INPROC_STRICT=1` makes a failed batch an error
   instead of leaving the scripts interpreted).
2. `night_inproc_build` analyses and translates the batch with the tiers the
   `--night-options` string allows (`Options::parse_str`). Translation uses
   logical table indices until every function exists.
3. The compiler calls back into the in-process host to reserve exactly as many
   funcref-table slots as it emitted blobs (`InprocHostReserve`), binds the
   absolute indices, and serializes the module.
4. The host installs it (`InprocHostInstall`): blob i lands at the reserved
   base + i. Two hosts exist, chosen at build time (`NIGHT_INPROC_HOST`):
   `wasm-jit-runner` (the testing host, via hostcalls) and Yurt's JIT mount ABI
   (a reserved table range and a module the guest assembles itself).
5. The shell installs the environment descriptor and patches scripts to their
   table entries. A failure before the module is mounted releases the
   reservation and the scripts stay interpreted. Once the module is mounted
   nothing is unmounted: if the environment install then fails, the run
   reports the error and the compile output stays alive (the install may
   already have published pointers into its descriptor); a script that cannot
   be armed stays interpreted, and fails the run under `NIGHT_INPROC_STRICT`.

The helper indices, table indices, region addresses, and serialized tables must
describe exactly the module that receives the blobs.

## 3. Compiler inputs and identities

`compiler/src/source.rs` owns the compiler input. `SourceObject` variants
describe scripts, scopes, objects, strings, symbols, and primitive values.
`source/ffi.rs` builds the graph in-process; the snapshot crate builds it from
the captured image.

Typed identifiers in `ids.rs` distinguish scripts, bytecode PCs, program
sites, names, layout keys, and their biased runtime stamp keys. Traversal of
maps that affects emitted identities must be stable; diagnostics and hash-table
iteration must not influence output.

`bytecode.rs` uses a generated `JSOp` enum and generated operand and stack
metadata. Its parser supports direct decoding and the `OpcodeVisitor` interface
used by analysis and prepasses.

## 4. Likely-facts analysis

`likelier` is an optimistic whole-program dataflow analysis. Its results are
predictions, not proofs. Codegen may use them only to select and order guarded
paths; a failed guard must reach a semantically complete path.

### Constraint graph and fixpoint

`scan.rs` walks each script once and builds constraints over an abstract
operand stack. Locals are flow-sensitive. Assignments create new abstract
values, control-flow joins create explicit join values, and loop headers
materialize joins eagerly for back edges.

`engine.rs` owns cells, constraints, subscriptions, provenance, and the
incremental worklist. Constraints are generated once and evaluated in each live
calling context. Cell growth requeues its subscribers.

`types.rs` combines bounded primitive and function sets, numeric magnitude and
interval information, and an object abstraction that widens from one allocation
to a class and then arbitrary object. Joins only weaken information. Every
resource limit must degrade in that direction: merge contexts, mark overflow,
drop a heap prediction, or widen to unknown.

### Calls and heap

`calls.rs` represents context as an interned bounded call string.  Arguments
and receivers flow into context-indexed callee cells; returns flow back to call
sites. Recursion, depth, fanout, and global budget limits fall back to the
generic context.

`heap.rs` models snapshot objects, allocation abstractions, fields, prototypes,
arrays, and constructor classes. Snapshot state seeds the same cells that later
program writes update. Property reads join every prototype level they might
observe. Elements use a separate abstract field to avoid merging unrelated
prototype elements.

The points-to model of the heap is a combination of Andersen points-to (with
points-to sets and membership/subset relations) and Steensgaard points-to (with
a union-find data structure that merges abstractions bidirectionally on any
interaction). Specifically, a typeset describes its object-reference component
as an element in a lattice that contains allocation sites at the bottom tier;
then constructor classes (every allocation site belongs to one constructor
class or a special pseudoclass for object literals); then union-find "regions"
of classes. Flow is still directional (no bidirectional union as in
Steensgaard), but where an Andersen points-to set would grow from one to
multiple elements, our points-to model instead merges the merging classes in
the union-find and represents the merged value by pointing to that region
instead.

Each heap abstraction carries predicted types per field name and predicted slot
number (property/shape order) per field name.

Constructor events record ordered writes and delegation. `likelier/emit.rs`
forms predicted fixed-slot layouts, groups compatible prefixes, assigns stable
layout keys, and emits site facts. `likelier/effects.rs` computes post-fixpoint
effect summaries per function (script).

`facts.rs::LikelyFacts` is the analysis-to-codegen contract. It contains value
claims, call resolutions, heap and element predictions, effect summaries,
global facts, and inlining inputs. Codegen should not reach into solver state.

`opsem.rs` is shared vocabulary for primitive sets, numeric magnitude,
intervals, and modeled operator results. It prevents analysis and codegen from
maintaining separate arithmetic semantics. Predicted results remain untrusted;
an interval originating at a guarded or canonical producer can become a proof
inside codegen.

## 5. Translation: the tiers

`wasm/mod.rs` runs analysis, lays out memory regions, translates scripts and
regexps, places functions in the indirect table, and patches address
placeholders after bases are known.

Each script goes through up to two compiled tiers, then the interpreter
(`Options::pipeline`; `docs/BASELINE.md` section 5 has the selection rules):

- **Baseline** (`wasm/baseline/`): a one-to-one translation of the bytecode
  that keeps the interpreter's frame layout. It is the correctness floor and the
  landing place of every MIR exit, so any script MIR compiles also carries a
  baseline body. It declines a script for `ForceInterpreter`, for ops it does
  not support (direct eval, which needs the caller's interpreter frame), and for
  size limits; the script is then interpreted.
- **MIR** (`mir/`, `wasm/mir/`): an SSA builder over the bytecode that uses the
  likely facts, an optimizer, and a lowering to Wasm. Speculation is guarded:
  a failed guard exits to the baseline body at the same pc. `docs/MIR.md`
  describes the builder, optimizer and lowering; `docs/MIR-MEMORY.md` the memory
  and effects model, including the property-layout stamps.
- **Interpreter**: whatever a tier declines. `--strict-coverage` rejects a
  decline unless the reason is a designed-out one (`ForceInterpreter`, an
  unsupported op).

An optimistic fact is never trusted on its own: it reaches generated code only
through a guard, and the generic path stays correct when a guard fails.

### Representations and proofs

An operand records Wasm representation separately from JavaScript type. Common
representations are boxed `JS::Value`, `i32`, exact integer `i64`, `f64`, object
or string pointer, and boolean.

A codegen fact must originate in a dominating runtime guard, an exact producer,
a helper or canonical-boxing invariant, or preservation across a proven effect
class. An analysis prediction alone may not manufacture an unboxed value or
remove a required check. Generic helpers accept and return full boxed values
and preserve JavaScript exception behavior.

Before a may-GC call, every live GC value must be visible in the traced AOT
stack or another engine root. MIR roots a value in a home slot when it is live
across a may-GC call, and writes each managed local to its frame slot so the GC
sees it (`docs/MIR.md`, rooting). A dead local overwritten by any value
is released when its frame slot is overwritten, as in baseline: the overwrite
writes the new value (or `undefined` for a scalar) to the slot, and the passes
that delete a non-escaping allocation keep that store as `undefined`. Compiled direct
calls return an error result and effect bits; effect bits kill heap, stamp, and
binding facts but do not replace rooting.

### Inlining

MIR inlines small callees into the caller's frame (`wasm/mir/inline.rs`,
`docs/MIR.md`): the callee gets an alternate frame view, its returns rejoin the
caller, and a guard failure inside it exits through `exit.inline` to the
callee's baseline body. Try notes, environments, `arguments`, script-relative
operands and generator state are admission conditions.

## 6. Object layouts and stamps

At runtime, all objects are "stamped" with their class ID according to this
lattice, and have bits indicating whether they still conform to the predicted
types and slot numbers. Emitted specialized code can guard on these ID-stamps
and validity bits to enable the use of raw, unchecked property accesses.
Properties are still always stored in boxed form, for compatibility with the
rest of the runtime.

A 32-bit stamp occupies the wasm32 `JSObject` alignment word at offset 4. Zero
means unstamped. The low 16 bits identify a predicted layout. Upper bits say
which subclaims remain valid. During construction, identity is unpublished and
an early key identifies the layout being built.

| Claim | Meaning | Use | Invalidation |
|---|---|---|---|
| identity | object has one compatible layout key | select a property layout | wholesale clear or restamp |
| TYPES/SHALLOW | guarded fields retain their numeric type property | remove redundant numberness checks | a non-number protected-field store |
| SLOTS | properties occupy predicted slots | bake a fixed-slot offset | an unexpected in-prefix property addition |
| RANGES | protected fields retain predicted intervals | propagate interval proofs | every unchecked engine store or an out-of-range compiled store |

Correctness requires an invalidation cut across all mutation paths: engine
stores call the NightMonkey store check; compiled stores perform equivalent
maintenance inline; property additions call `NightAddPropCheck`; shape and
prototype mutations clear identity when required; and GC forwarding either
updates roots or invalidates raw-pointer caches.

The construction sentinel prevents partial objects passing identity guards.
Constructor exit publishes identity only after checked stores establish the
surviving validity bits. Compatible prefix layouts receive contiguous keys so
one range check can cover a layout *region*. Older comments call a region a
*clump*.

## 7. Caches and fuses

A fuse is a rarely changing condition represented by an armed or blown word.
It is safe only if every operation that can falsify it reaches an invalidation
point. NightMonkey uses fuses or generations for bindings, call and constructor
targets, builtins, prototype-dependent operations, and dynamically compiled
source.

Global property hooks invalidate binding fuses for interpreted writes. Property
cache rows publish their validity word last so generated code cannot see a
partial row. Major GC clears raw shape/prototype caches; minor GC clears or
rebuilds subsets that can hold nursery pointers.

The dynamic-source fuse is intended to be monotone after capture. Re-arming is
sound only when registration accounts for every source compiled so far and no
unregistered compilation is interleaved.

## 8. Runtime ABI and memory regions

`NightEnv.h` defines the ordered environment descriptor shared by compiler and
runtime; `compiler/build.rs` generates its Rust mirror. `NightRegionShape.h`
similarly shares cache sizes and row strides. These are the preferred pattern
for cross-language ABI data.

The environment contains atom, layout, binding, fuse, builtin, regex, and cache
tables plus mutable cells. Snapshot region words are absolute addresses.
In-process table words are descriptor-relative offsets and are rebased during
installation; address and length words remain absolute.

`NightHelperList.h` is the C++ helper manifest. Rust's `Helpers` structure and
resolution logic manually duplicate it and should be generated from the same
source. `wasm/mir/abi.rs` records SpiderMonkey offsets and selector values baked into
Wasm. `NightInlineHeap.cpp` statically asserts the engine-layout half. New ABI
data should use generated shared definitions rather than paired literals.

## 9. Entry, stack, and GC

`NightStack` is a separately allocated array of boxed values owned by
`JSContext`; its live prefix is traced as roots. `AutoNightReentry` restores the
old top after interpreter-to-AOT reentry.

An entry frame contains callee, receiver, actual/formal slots, locals, and
operand storage. Missing actuals are padded with `undefined`. The complete frame
must fit before entry. Bounds checks must compare integer slot counts before
forming an end pointer, because an already-out-of-range C++ pointer is undefined
even if it is used only in a comparison.

Generated code may retain non-GC scalars in SSA across calls. GC pointers that
survive a may-GC operation must be in the live stack prefix or another root.
Inline heap writes mirror SpiderMonkey barriers using offsets pinned by static
assertions.

Most installed state, caches, persistent roots, and callbacks are process-global
and have no teardown. This matches the single-image shell deployment but must
be enforced or redesigned before runtime destruction and recreation.

## 10. Generators, async, exceptions, and regexps

Generator and async bodies are supported by the baseline tier
(`wasm/baseline/codegen/resume.rs`) and `NightGenerator.cpp`. Suspension stores
locals and operands in an engine object using an AOT-owned layout; resume
re-enters the same compiled script. An AOT-suspended generator cannot resume in
the interpreter. A generator that reads its actuals after suspension is declined. The MIR tier
compiles generators and async functions with guarded resume roots (a guard that
fails at a resume exits to baseline at the landing).

Try notes feed CFG and frame construction. Calls and helpers propagate pending
exceptions through the error result. Inlining rejects contexts whose exception
or frame semantics cannot be represented by a splice.

`wasm/regex.rs` translates supported irregexp bytecode to Wasm. Runtime matching
selects installed matchers by pattern and flags. A matcher returns success,
failure, or retry; retry and unsupported programs use irregexp's established
fallback. Exhausting fixed backtracking storage must return retry.

## 11. Limits and policy

`constants.rs` is intended to collect analysis, specialization, inlining,
translation, and diagnostic policy. Structural ABI limits stay with their
representations. Some limits are scattered throughout the implementations of
various heuristics; cleanup is ongoing.

Size limits keep the guest compile within the guest's memory (a Rust
allocation failure aborts the guest; it does not return): the MIR tier declines
a script whose (bytecode bytes x frame slots) exceeds `MAX_LIVENESS_CELLS`
(16M; MIR value and parameter counts scale with the same product) or whose
bytecode is over 32 KiB, a script of over 1 MiB of bytecode is declined, and one assembled batch holds at
most about 100,000 blobs (wasmparser's 1,000,000 effective-type-size limit; a
larger program installs nothing and stays interpreted). A function's lowered
body may not exceed wasmparser's 7,654,321 bytes either; inner-loop onramps,
which make waffle copy code, are what grows it (`wants_onramp`).

## 12. Capabilities and fallback

Baseline's lowering is the run of arms in `wasm/baseline/codegen/op_*.rs`; an op
no run handles declines the script with a `BUG:` reason. Explicit declines are
`ForceInterpreter` and the eval variants (an `unsupported op`, because eval reads
the caller's interpreter frame). MIR declines anything its builder does not
accept, and the script falls back to baseline.
Compiled frames are not interpreter frames, and some observable behavior
follows from that. These are designed-out capabilities, kept covered by the
interpreter lane (`tests/jit-test-excludes.txt`):

- The Debugger does not see compiled frames: hooks such as `onExceptionUnwind`
  are not called for exceptions thrown in compiled code, and `evalInFrame`
  finds no frame. This is why direct eval is declined: it reads the caller's
  interpreter frame.
- A compiled frame carries no pc, so an error message that names an expression
  prints the value instead (`[] is not ...` where the interpreter prints
  `arr is not ...`). The text is implementation-defined.
- Anything that walks the interpreter's frames sees nothing for compiled ones:
  `new Error().stack` is empty and `lineNumber` is 0 for an error made in
  compiled code, `f.caller` is `null`, and an uncaught exception's report loses
  its `file:line:col` and `Stack:` lines. The jstests that depend on these are
  listed in `tests/jstests-excludes.txt`.
- A formal of a script that reads its actuals later (`Rest`, `arguments`,
  spread forwarding) keeps the caller's value in its frame slot, which those
  reads need, so the compiled tiers do not write an assignment to it there:
  an object assigned to such a formal is released once nothing else holds it,
  where baseline holds it until the formal is assigned again.
- An object literal the optimizer scalar-replaces (it never escapes) does not
  exist at run time, so a local assigned one holds nothing in its frame slot:
  what only that object would have kept alive (a field's value nothing else
  uses) is released as soon as nothing else holds it, where baseline holds it
  until the local is assigned again. Keeping it would mean building the
  object, which is what scalar replacement avoids.
- An `arguments` element that the interpreter keeps alive after `delete` or a
  strict `defineProperty` may be released earlier. The specification allows it.

Environment gates reject frame shapes the runtime cannot model.

Opcode knowledge is also duplicated in analysis transfer, effects, splice
safety, visualization, and auxiliary scans. An exhaustive lowering match does
not make those classifications exhaustive. A single opcode capability
description should state stack and immediate shape, analysis transfer, effects,
frame/script relativity, splice safety, and lowering support. New opcodes should
fail tests until every dimension is classified.

Fallback has three levels:

1. failed specialization continues in generic compiled code;
2. helpers and regex matchers use established engine slow paths;
3. untranslatable scripts have no AOT entry and remain interpreted.

Tests must distinguish these. Passing a language test through a shell that can
silently interpret a declined script does not establish generated-code coverage.

## 13. Validation

Validation includes Rust unit tests, jit-tests and jstests in AOT and
interpreter-only wasm lanes, differential application runs, Wasm validation,
post-translation reducibility checks, C++ assertions for baked layouts, and
diagnostics for degradation, skipped scripts, guards, effects, and caches.

## 14. Glossary

- **likely fact**: an untrusted analysis prediction.
- **codegen fact**: a property proved by a guard or sound producer and carried
  as a MIR value type.
- **tier**: baseline, MIR, or the interpreter; the translation a script gets.
- **exit**: leaving MIR code for the script's baseline body at a pc, with the
  frame written; the landing for every failed guard.
- **side arm**: a guarded alternative inside one lowering.
- **splice**: an inlined callee represented in the caller's MIR function.
- **stamp**: the object word identifying layout and valid subclaims.
- **region**: a compatible contiguous family of layout keys.
- **fuse**: an armed condition with a complete invalidation cut.
- **choke/chokepoint**: an invalidation point. Prefer that standard term in new
  code.
- **on-ramp**: a guarded edge from baseline code into an optimistic MIR
  context at a loop head.
