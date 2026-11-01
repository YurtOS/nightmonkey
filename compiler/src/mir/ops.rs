//! The opcode set (MIR.md §7): per-op signature, result rule, successor
//! shape, and effect summary.
//!
//! Everything here is a function of the opcode, its immediates and its
//! operand *types* (plus the module tables). That is what lets the
//! validator check an op without knowing how it got there, and lets a
//! pass reason about an op it did not create.
//!
//! Terminators that can fail carry their refined outputs to the success
//! edge as *outputs*: the success block's params receive them (see
//! `func::EdgeArg`). [`Sig::outputs`] are the types the op produces; the
//! receiving params may be supertypes.

use crate::ids::{EnvSlot, LayoutKey, Pc, ScriptId};
use crate::mir::entity::{AtomId, BindingId, FuseId, NativeId, SnapObj};
use crate::mir::module::{Module, Region};
use crate::mir::types::{
    is_subtype, join, FactKind, FieldSet, IRange, KeyRange, KillPattern, KillSet, LayoutClaim,
    LayoutState, NumInfo, ObjInfo, ObjKind, RawKind, StrInfo, TagSet, Type, VSet, I32_MAX, I32_MIN,
    INT_LIM,
};
use crate::opsem::{TaKind, PRIM_BIGINT, PRIM_DOUBLE, PRIM_INT32, PRIM_NULL, PRIM_UNDEFINED};

/// A boxed constant.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ConstVal {
    Undefined,
    Null,
    Bool(bool),
    Int32(i32),
    /// Bits of an f64, so the opcode stays `Eq` (and NaNs round-trip).
    Double(u64),
    /// The TDZ sentinel (`JS_UNINITIALIZED_LEXICAL`), a magic value: what an
    /// uninitialized `let`/`const` binding holds.
    Uninitialized,
    /// Any valid value: an exit operand for a frame slot that is dead at
    /// the exit's pc (§5.1's liveness pruning). The lowering leaves such a
    /// slot as the frame already has it.
    Dead,
    /// The `this` placeholder of a `new` (`JS_IS_CONSTRUCTING`), a magic
    /// value.
    IsConstructing,
    /// An array literal's elision (`JS_ELEMENTS_HOLE`), a magic value.
    Hole,
}

/// `RtOp::PushEnv` kinds.
pub const ENV_LEXICAL: u32 = 0;
pub const ENV_CLASS_BODY: u32 = 1;
pub const ENV_VAR: u32 = 2;

/// `RtOp::Check` kinds.
pub const CHECK_OBJ_COERCIBLE: u32 = 0;
pub const CHECK_CLASS_HERITAGE: u32 = 1;
pub const CHECK_THIS: u32 = 2;
pub const CHECK_THIS_REINIT: u32 = 3;

/// The target of an unbox.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum UnboxKind {
    I32,
    F64Num,
    Bool,
    Obj,
    Str,
}

impl UnboxKind {
    pub const ALL: [UnboxKind; 5] = [
        UnboxKind::I32,
        UnboxKind::F64Num,
        UnboxKind::Bool,
        UnboxKind::Obj,
        UnboxKind::Str,
    ];

    pub fn name(self) -> &'static str {
        match self {
            UnboxKind::I32 => "i32",
            UnboxKind::F64Num => "f64num",
            UnboxKind::Bool => "bool",
            UnboxKind::Obj => "obj",
            UnboxKind::Str => "str",
        }
    }

    /// The tags a value must have for this unbox to be infallible.
    pub fn tags(self) -> TagSet {
        match self {
            UnboxKind::I32 => TagSet::INT32,
            UnboxKind::F64Num => TagSet::NUMBER,
            UnboxKind::Bool => TagSet::BOOLEAN,
            UnboxKind::Obj => TagSet::OBJECT,
            UnboxKind::Str => TagSet::STRING,
        }
    }

    /// The raw type unboxing `v` (already within `tags()`) yields.
    fn result(self, v: &VSet) -> Type {
        match self {
            UnboxKind::I32 => Type::I32(v.num.int_range(I32_MIN, I32_MAX)),
            UnboxKind::F64Num => Type::F64(v.num),
            UnboxKind::Bool => Type::Bool,
            UnboxKind::Obj => Type::Obj(v.obj),
            UnboxKind::Str => Type::Str(v.str),
        }
    }
}

/// What a `check.binding` proves of its binding.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum BindingCheck {
    /// Its slot is resolved against the global object's live shape (or
    /// its value fuse is armed): `load_gname` may read it.
    Read,
    /// That, and the slot is a writable data property: `store_gname` may
    /// write it.
    Write,
    /// Its value fuse is in the predicted state: the binding holds a
    /// compiled function of its predicted script (`BindingDef::pred`),
    /// which `load_gname` then yields, typed.
    Fn,
}

impl BindingCheck {
    /// Whether a check of this kind proves one of kind `o` too.
    pub fn implies(self, o: BindingCheck) -> bool {
        self == o || o == BindingCheck::Read
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    /// JS `%` on integers (the dividend's sign). Checked (`.ovf`), it
    /// fails on a zero divisor and on a -0 result (a negative dividend
    /// with remainder 0); `.wrap` and `int` need both ruled out by type.
    Rem,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum F64Op {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum BitOp {
    And,
    Or,
    Xor,
    Shl,
    Shr,
}

/// A raw numeric representation, for compares.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum NumRepr {
    I32,
    Int,
    F64,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Cc {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum MathFn {
    Abs,
    Floor,
    Ceil,
    Round,
    Trunc,
    Sqrt,
    Sign,
    Fround,
    Sin,
    Cos,
    Tan,
    Exp,
    Log,
    Min,
    Max,
    Pow,
    Atan2,
}

impl MathFn {
    pub const ALL: [MathFn; 17] = [
        MathFn::Abs,
        MathFn::Floor,
        MathFn::Ceil,
        MathFn::Round,
        MathFn::Trunc,
        MathFn::Sqrt,
        MathFn::Sign,
        MathFn::Fround,
        MathFn::Sin,
        MathFn::Cos,
        MathFn::Tan,
        MathFn::Exp,
        MathFn::Log,
        MathFn::Min,
        MathFn::Max,
        MathFn::Pow,
        MathFn::Atan2,
    ];

    pub fn arity(self) -> usize {
        match self {
            MathFn::Min | MathFn::Max | MathFn::Pow | MathFn::Atan2 => 2,
            _ => 1,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum JsBinop {
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    BitAnd,
    BitOr,
    BitXor,
    Lsh,
    Rsh,
    Ursh,
}

/// The generic operations `js.rt` runs through their runtime helpers,
/// with their operands (all boxed).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RtOp {
    /// `lhs instanceof rhs` -> boolean.
    Instanceof,
    /// `key in obj` -> boolean.
    In,
    /// `obj.hasOwnProperty(key)` (the `HasOwn` op), `key, obj` -> boolean.
    HasOwn,
    /// `delete obj.name` -> boolean (strict or not).
    DelProp(AtomId, bool),
    /// `delete obj[key]` -> boolean.
    DelElem(bool),
    /// `{}` -> object, with fixed slots for this many fields (the literal's
    /// layout row length; 0 for the engine's default).
    NewObject(u32),
    /// `new Array(len)` for a literal of `len` elements -> object.
    NewArray(u32),
    /// Define own property `name` of `obj` to `v`, with the attributes.
    InitProp(AtomId, u32),
    /// Define own element `key` of `obj` to `v`, with the attributes.
    /// Its attrs, and whether the store owes the array stamp's RANGES
    /// claim a clear (`ranges`: a claim applies and the value is not
    /// proven inside it).
    InitElem(u32, bool),
    /// ToPropertyKey (a string, a symbol or an int32) -> val.
    ToPropertyKey,
    /// The regexp literal the script's gcthing `index` names, cloned ->
    /// object.
    RegExp(u32),
    /// Define getter/setter `name` of `obj` to `f` (`kind`: 1 setter,
    /// 2 hidden).
    InitPropGetSet(AtomId, u32),
    /// Self-hosted intrinsic `name` (`GetIntrinsic`) -> value.
    Intrinsic(AtomId),
    /// Global `name` for `typeof` (`GetGName` before `Typeof`): an unbound
    /// name is undefined, not a ReferenceError -> value.
    GetNameTypeof(AtomId),
    /// For-in's property iterator over `v` (`Iter`) -> object.
    Iter,
    /// A check of `v` that throws or does nothing (`CheckObjCoercible`,
    /// `CheckClassHeritage`, `CheckThis`: `CHECK_*`).
    Check(u32),
    /// Name function `fun` by key `name` (`SetFunName`, the prefix kind).
    SetFunName(u32),
    /// The global `this` (`GlobalThis`) -> object.
    GlobalThis,
    /// The BigInt literal at the script's gcthing index (`BigInt`) -> value.
    BigInt(u32),
    /// Set `obj`'s prototype to `proto` (`MutateProto`, `__proto__:` in a
    /// literal).
    MutateProto,
    /// `#x in obj` style checks (`CheckPrivateField`: condition, kind),
    /// `obj, key` -> boolean.
    CheckPrivateField(u32, u32),
    /// Throw unless `v` is an object (`CheckIsObj`, the message kind).
    CheckIsObj(u32),
    /// Close iterator `it` (`CloseIter`, the completion kind): calls its
    /// `return`.
    CloseIter(u32),
    /// A spread call's argument array, or undefined when the spread value
    /// is not a packed array whose iteration is intact
    /// (`OptimizeSpreadCall`) -> value.
    OptimizeSpreadCall,
    /// `f(...arr)` / `new f(...arr)` (`SpreadCall`/`SpreadNew`: whether it
    /// constructs): `callee, this, arr, new.target` -> value.
    SpreadCall(u32),
    /// A scope's environment over the frame's (`PushLexicalEnv`,
    /// `PushClassBodyEnv`, `PushVarEnv`: `ENV_*`, the scope's pc) ->
    /// object; `env.set` makes it the frame's.
    PushEnv(u32, u32),
    /// A `with` environment over the frame's wrapping `v` (`EnterWith`,
    /// its pc) -> object.
    EnterWith(u32),
    /// A copy of the frame's block environment (`FreshenLexicalEnv`,
    /// `RecreateLexicalEnv` with 1: fresh bindings) -> object.
    FreshenEnv(u32),
    /// A name read through the frame's environment chain (`GetName`; 1 for
    /// `typeof`, where unbound is undefined) -> value.
    GetName(AtomId, u32),
    /// The environment a later `js.rt.setname` stores `name` into, from the
    /// frame's chain (`BindName`, `BindUnqualifiedName` with 1) -> object.
    BindName(AtomId, u32),
    /// `delete name` through the frame's chain (`DelName`) -> boolean.
    DelName(AtomId),
    /// The frame's variable environment (`BindVar`) -> object.
    BindVar,
    /// `env.name = v` for an environment `BindName` found (`SetName`,
    /// strict): `env, v`.
    SetName(AtomId, bool),
    /// A computed-key accessor in a literal or class (`InitElemGetter`
    /// and kin: bit 0 setter, bit 1 hidden): `obj, key, fn`.
    InitElemGetSet(u32),
    /// The home object's prototype (`SuperBase`: home) -> value.
    SuperBase,
    /// The constructor's parent (`SuperFun`: callee) -> value.
    SuperFun,
    /// `super.name` (`GetPropSuper`): `recv, base` -> value.
    GetPropSuper(AtomId),
    /// `super[key]` (`GetElemSuper`): `recv, key, base` -> value.
    GetElemSuper,
    /// `super.name = v` (`SetPropSuper`, strict): `recv, base, v` -> v.
    SetPropSuper(AtomId, bool),
    /// `super[key] = v` (`SetElemSuper`, strict): `recv, key, base, v` -> v.
    SetElemSuper(bool),
    /// Set method `f`'s home object (`InitHomeObject`): `f, home`.
    InitHomeObject,
    /// A class constructor with prototype `proto` (`FunWithProto`, the
    /// function gcthing), closing over the frame's environment -> object.
    FunWithProto(u32),
    /// A derived constructor's result (`CheckReturn`): `this, rval` ->
    /// value (throws for a non-object, non-undefined rval or an
    /// uninitialized `this`).
    CheckReturn,
    /// Register a `using` resource with the frame's environment
    /// (`AddDisposable`, the hint): `v, method, needs_closure`.
    AddDisposable(u32),
    /// The frame's environment's disposal list, taken
    /// (`TakeDisposeCapability`) -> value.
    TakeDisposeCapability,
    /// `SuppressedError(e, suppressed)` (`CreateSuppressedError`) -> object.
    CreateSuppressedError,
    /// `name` read from environment `env` a `BindName` found
    /// (`GetBoundName`): `env` -> value.
    GetBoundName(AtomId),
    /// An object with prototype `proto` (`ObjWithProto`) -> object.
    ObjWithProto,
    /// A fresh private name (`NewPrivateName`) -> symbol.
    NewPrivateName(AtomId),
    /// `import(spec, opts)` (`DynamicImport`) -> object.
    DynamicImport,
    /// A direct eval with spread arguments (`SpreadEval`, its pc):
    /// `callee, this, arr` -> value.
    SpreadEval(u32),
    /// A generator object for the frame's callee and environment
    /// (`Generator`) -> object.
    CreateGenerator,
    /// Mark generator `g` finished (`FinalYieldRval`): `g`.
    GenFinal,
    /// Raise a `throw`/`return` resumption (`CheckResumeKind` with a kind
    /// other than next; always fails): `v, g, kind`. A return stages `v`
    /// as the frame's rval. Built from ff147 bytecode only.
    GenCheckResume,
    /// `AsyncAwait`/`AsyncResolve` (1 for resolve): `v, g` -> value.
    AsyncAwait(u32),
    /// `AsyncReject`: `reason, stack, g` -> value.
    AsyncReject,
    /// `CanSkipAwait`: `v` -> boolean.
    CanSkipAwait,
    /// `MaybeExtractAwaitValue`: `v, can_skip` -> value.
    MaybeExtractAwait,
    /// Resume generator `g` with `v` and resume kind `k` (`Resume`) -> value.
    Resume,
    /// `ToString` of v -> string.
    ToString,
    /// Well-known symbol `code` (`JSOp::Symbol`) -> symbol.
    Symbol(u32),
    /// Builtin object `kind` (`JSOp::BuiltinObject`) -> object.
    BuiltinObject(u32),
}

impl RtOp {
    /// The op with its atoms renamed by `f` (an inlined callee's, into
    /// the caller's table). Exhaustive, with no wildcard: a new variant
    /// carrying an atom must say how it maps.
    pub fn map_atoms(self, f: impl Fn(AtomId) -> AtomId) -> RtOp {
        use RtOp::*;
        match self {
            DelProp(a, s) => DelProp(f(a), s),
            InitProp(a, t) => InitProp(f(a), t),
            InitPropGetSet(a, k) => InitPropGetSet(f(a), k),
            Intrinsic(a) => Intrinsic(f(a)),
            GetNameTypeof(a) => GetNameTypeof(f(a)),
            GetName(a, t) => GetName(f(a), t),
            BindName(a, k) => BindName(f(a), k),
            DelName(a) => DelName(f(a)),
            SetName(a, s) => SetName(f(a), s),
            GetPropSuper(a) => GetPropSuper(f(a)),
            SetPropSuper(a, s) => SetPropSuper(f(a), s),
            GetBoundName(a) => GetBoundName(f(a)),
            NewPrivateName(a) => NewPrivateName(f(a)),
            op @ (Instanceof
            | In
            | HasOwn
            | DelElem(_)
            | NewObject(_)
            | NewArray(_)
            | InitElem(..)
            | ToPropertyKey
            | RegExp(_)
            | Iter
            | Check(_)
            | SetFunName(_)
            | GlobalThis
            | BigInt(_)
            | MutateProto
            | CheckPrivateField(..)
            | CheckIsObj(_)
            | CloseIter(_)
            | OptimizeSpreadCall
            | SpreadCall(_)
            | PushEnv(..)
            | EnterWith(_)
            | FreshenEnv(_)
            | BindVar
            | InitElemGetSet(_)
            | SuperBase
            | SuperFun
            | GetElemSuper
            | SetElemSuper(_)
            | InitHomeObject
            | FunWithProto(_)
            | CheckReturn
            | AddDisposable(_)
            | TakeDisposeCapability
            | CreateSuppressedError
            | ObjWithProto
            | DynamicImport
            | SpreadEval(_)
            | CreateGenerator
            | GenFinal
            | GenCheckResume
            | AsyncAwait(_)
            | AsyncReject
            | CanSkipAwait
            | MaybeExtractAwait
            | Resume
            | ToString
            | Symbol(_)
            | BuiltinObject(_)) => op,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum JsUnop {
    Neg,
    Pos,
    BitNot,
    Inc,
    Dec,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum JsCc {
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    Lt,
    Le,
    Gt,
    Ge,
}

/// A generic numeric op or compare that `Opcode::Prim` does on primitive
/// operands only.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PrimOp {
    Add,
    Binop(JsBinop),
    Unop(JsUnop),
    Compare(JsCc),
    ToNumeric,
}

impl PrimOp {
    /// The generic op it restricts.
    pub fn generic(self) -> Opcode {
        match self {
            PrimOp::Add => Opcode::JsAdd,
            PrimOp::Binop(b) => Opcode::JsBinop(b),
            PrimOp::Unop(u) => Opcode::JsUnop(u),
            PrimOp::Compare(c) => Opcode::JsCompare(c),
            PrimOp::ToNumeric => Opcode::JsToNumeric,
        }
    }

    /// The restriction of a generic op, if it has one.
    pub fn of(op: &Opcode) -> Option<PrimOp> {
        Some(match *op {
            Opcode::JsAdd => PrimOp::Add,
            Opcode::JsBinop(b) => PrimOp::Binop(b),
            Opcode::JsUnop(u) => PrimOp::Unop(u),
            Opcode::JsCompare(c) => PrimOp::Compare(c),
            Opcode::JsToNumeric => PrimOp::ToNumeric,
            _ => return None,
        })
    }
}

/// An opcode with its immediates. Operands are `InstData::args`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Opcode {
    // Constants.
    ConstVal(ConstVal),
    ConstI32(i32),
    /// Bits of the f64.
    ConstF64(u64),
    ConstBool(bool),
    ConstObj(SnapObj),
    ConstStr(AtomId),

    // Conversions.
    Box,
    /// An f64 boxed as the double it is (NaN made canonical), not as an
    /// int32 where it is one: for a store whose readers all admit the
    /// double tag (the `elem_write_sites` rule).
    BoxDouble,
    Unbox(UnboxKind),
    I32ToInt,
    I32ToF64,
    IntToF64,
    /// Identity with a declared weaker result type (§4.2). Every op's
    /// result may be declared weaker than its rule; `weaken` is the op
    /// whose only purpose is that.
    Weaken,

    // Guards and checks (terminators: ok, fail).
    GuardUnbox(UnboxKind),
    GuardTags(TagSet),
    GuardKind(ObjKind),
    GuardLayout {
        keys: KeyRange,
        types: bool,
        slots: bool,
        /// And the CLOSED bit: no own property outside the row.
        closed: bool,
    },
    /// An object under construction for layout `key` (MIR.md §2.3): its
    /// word carries the CONSTRUCTING sentinel with `key`'s early key, and
    /// SLOTS (and TYPES if `types`), and the fields `n` (the word's set
    /// and a span of theirs), exactly (`constructing(n)`).
    GuardCtor {
        key: LayoutKey,
        n: FieldSet,
        types: bool,
    },
    GuardSingleton(SnapObj),
    GuardScript(ScriptId),
    /// A predicted method (`method_sites`): the receiver, of layouts whose
    /// rows lack `atom`, if CLOSED, reads `atom` through its prototype
    /// chain as the function in the method cell at `cell` -- the cell
    /// armed for the receiver's prototype, so the chain resolves the name
    /// to a compiled function of `script`, held by a constant property.
    /// ok: that function; fail: the receiver is not CLOSED, or the cell is
    /// not armed for its prototype and the runtime could not arm it.
    MethodLoad {
        cell: u32,
        atom: AtomId,
        script: ScriptId,
    },
    /// An f64 that is exactly an int32 (not -0) becomes an `I32`.
    F64ToIntExact,
    /// An `int` known to be int32 (T: fails outside int32's range).
    IntToI32,
    /// An int32 in the inclusive range (fails outside it): an onramp's
    /// value brought to the range the loop's entry path proves.
    GuardRange(IRange),
    CheckFuse(FuseId),
    /// The binding is usable as `BindingCheck` says.
    CheckBinding(BindingId, BindingCheck),
    CheckNative(NativeId),
    /// The receiver (boxed) has, for property `name`, the accessor the
    /// runtime's accessor-call cache recorded for its shape (a getter, or
    /// with the flag a setter), on a holder whose shape is still the one
    /// recorded: ok with that accessor function.
    AccessorProbe(AtomId, bool),

    // Control.
    Jump,
    Br,
    /// Dense switch over `0..n`, plus a default.
    Switch(u32),
    /// `switch (x)` over `n` string constants (`x === s_k` for the first
    /// `k` that holds): operands `x`, then `s_0 .. s_{n-1}` (atoms). Edges:
    /// one per case, the default (no case holds, `x` not a string
    /// included), and `fail` for a subject the lowering does not compare
    /// inline (a rope), before anything happens.
    SwitchStr(u32),
    Return,
    /// Operands: `this`, `nargs` args, `nlocals` locals, the rval, then
    /// the operand stack (the rest). All `Val(⊤)`.
    Exit {
        pc: Pc,
        nargs: u32,
        nlocals: u32,
    },
    ExitThrow {
        pc: Pc,
        nargs: u32,
        nlocals: u32,
    },
    /// Suspend the generator at a yield (`InitialYield`, `Yield`, `Await`
    /// at `pc`, resume index `index`): an exit's operands (the frame at
    /// `pc`, whose stack ends with the generator, below it the yielded
    /// value unless `initial`). The lowering writes the frame, saves it
    /// into the generator (baseline's `gen_suspend`) and returns the
    /// yielded value (the generator for `initial`). A resume enters the
    /// function's `Resume` root for `index`.
    GenSuspend {
        pc: Pc,
        index: u32,
        nargs: u32,
        nlocals: u32,
        initial: bool,
    },
    /// Whether `v` is the generator-closing magic (`IsGenClosing`) -> bool.
    /// Built from ff147 bytecode only (159 has neither the op nor the
    /// magic).
    IsGenClosing,
    /// An exit from an inlined callee's code (§5.5), with an `exit`'s
    /// operands for the callee's frame: finish the callee in its baseline
    /// body from `pc` (with its exception pending if `throw`), and take
    /// `ok` with its result or `err` with its exception.
    ExitInline {
        pc: Pc,
        nargs: u32,
        nlocals: u32,
        throw: bool,
    },
    /// Write the inlined callee's frame (§5.5) from `callee, this, args`
    /// (the callee's formals, padded), its other slots as its prologue
    /// would. The instruction's frame is the callee's.
    InlineEnter,
    Unreachable,

    // Numeric.
    I32Ovf(ArithOp),
    I32Wrap(ArithOp),
    IntArith(ArithOp),
    F64Arith(F64Op),
    F64Neg,
    I32Bit(BitOp),
    I32Ushr,
    ToInt32,
    Cmp(NumRepr, Cc),
    Math(MathFn),

    // Generic JS ops.
    JsAdd,
    JsBinop(JsBinop),
    JsUnop(JsUnop),
    JsCompare(JsCc),
    JsTypeof,
    /// `typeof v == type` (or `!=`), the operand byte of `JSOp::TypeofEq`
    /// (`js::TypeofEqOperand`): a leaf.
    JsTypeofEq(u8),
    /// `v === c` for the constant the operand of `JSOp::StrictConstantEq`
    /// encodes (`js::ConstantCompareOperand`): a leaf.
    JsConstantStrictEq(u16),
    /// Formal `n` of the frame's mapped `arguments` object (which the
    /// entry made): `ArgumentsObject::arg`, a leaf.
    ArgsMapped(u32),
    /// Set formal `n` of the frame's mapped `arguments` object: a leaf.
    ArgsMappedSet(u32),
    JsToBool,
    JsToNumeric,
    /// The generic op on operands that run no code in it: primitives (and
    /// for an equality, two objects, or an object and null or undefined),
    /// ok with its result. An operand whose conversion would call user
    /// code (`valueOf`, `toString`, `Symbol.toPrimitive`) fails before
    /// anything happened; a TypeError or RangeError of the op itself
    /// (a Symbol, BigInt mixing) takes `err`. Allocates (concatenation, a
    /// BigInt), writes nothing (MIR-MEMORY.md §1, KICKOFF-8 item 1).
    Prim(PrimOp),
    JsGetProp(AtomId),
    /// `recv.name` where the lookup runs no code: an own or prototype data
    /// property, an absent one (`undefined`), or a pure builtin length; ok
    /// with the value. A lookup that would run code (a getter, a proxy, a
    /// resolve hook, a primitive with no prototype) or throw (`null`,
    /// `undefined`) fails, before anything happened. It reads the field by
    /// name and nothing else (MIR-MEMORY.md §1: a property fallback's
    /// common case, which must not write `Unknown`).
    GetPropData(AtomId),
    /// `recv.name = v` where the set runs no code: an overwrite of an own
    /// writable data property of a native object (or of an array's
    /// writable length), or an add to an
    /// extensible one with no setter, read-only property or hook on its
    /// chain; not on the global or an object Watchtower watches.
    /// `ok_clean` once stored; `ok_dirty` once stored where the store
    /// demoted a claim of the object's published class (a value not of the
    /// field's type, an add off the class's slots); anything else fails
    /// before anything happened; an engine error takes `err`. Its clean
    /// edge writes the field by name and nothing else.
    SetPropData(AtomId),
    /// Strict-mode (`true`) or sloppy assignment.
    JsSetProp(AtomId, bool),
    JsGetElem,
    /// `recv[key]` where the read runs no code: a primitive key (an object
    /// key's ToPropertyKey may call user code), found as a data property,
    /// an element (dense, a typed array's, an arguments object's, a
    /// string's char), or absent (`undefined`); ok with the value. A read
    /// that would run code (a getter, a proxy, a resolve hook) or throw
    /// fails before anything happened; an engine error (OOM, interning the
    /// key) takes `err`. Reads anything, writes nothing.
    GetElemData,
    /// `recv[key] = v` where the set runs no code, with the RANGES duty
    /// (as `JsSetElem`'s): an element of a native object or array (an
    /// overwrite of a writable one; an add with no setter or read-only
    /// element on the chain; a writable length), or a typed array's of a
    /// number. Edges as `setprop.data`'s. With an int32 key its clean edge
    /// writes elements and lengths only.
    SetElemData(bool),
    /// Strictness, and the RANGES duty (as `InitElem`'s).
    JsSetElem(bool, bool),
    JsGetName(AtomId),
    /// Sloppy-mode `this` that is not an object: the global `this` for
    /// null/undefined, a wrapper object for a primitive.
    JsBoxThis,
    /// The binding object for an unqualified global assignment
    /// (`BindUnqualifiedGName`).
    JsBindGName(AtomId),
    /// `env.name = v` for a global or name assignment (`SetGName`), strict
    /// or sloppy.
    JsSetName(AtomId, bool),
    /// A generic operation through its runtime helper (see [`RtOp`]).
    JsRt(RtOp),
    /// The function's (unmapped) arguments object: the frame's cached
    /// one, else a new one, cached. Only in the function's own frame.
    ArgsObject,
    /// A rest-parameter array of the actuals past the first `n`.
    RestArray(u32),
    /// The actual argument count, an i32.
    ArgsLength,
    /// The frame's `new.target` (the op's frame: an inlined construct's
    /// frame holds its own) -> value.
    FrameNewTarget,
    /// The frame's callee (`Callee`) -> value.
    FrameCallee,
    /// For-in: the iterator's next property name, or the NO_ITER magic
    /// when it is exhausted (`MoreIter`; advances the iterator).
    IterMore,
    /// Whether a value is the NO_ITER magic (`IsNoIter`) -> bool.
    IterIsDone,
    /// For-in: close the iterator (`EndIter`).
    IterEnd,
    /// Whether `v` iterates as a packed array with the iteration protocol
    /// intact (`OptimizeGetIterator`) -> bool.
    IterOptimizable,
    /// Actual argument `args[0]` (an i32 index below the count).
    ActualArg,
    /// Actual argument `k`, or undefined if there are not that many.
    ActualArgOr(u32),
    /// Whether boxed `args[0]` is the builtin in cell `k` (the runtime's
    /// pristine-builtin cells, e.g. `Function.prototype.apply`).
    JsIsBuiltin(u32),
    /// `target.apply(this, arguments)` forwarding the frame's own actuals
    /// (operands `apply`, `target`, `this`), through the runtime.
    ApplyFwd,
    /// `throw args[0]`: the only successor is `err`.
    JsThrow,
    /// Write `args[0]` (a `Val`, or an i32, f64 or bool, stored as the
    /// Value it is) to baseline frame slot `k` (0 `this`, then the
    /// formals, the locals, the rval), where it stands. Baseline writes every
    /// assignment to a local there, so the slot holds the local's latest
    /// value (a dead local keeps its object until it is assigned again, and
    /// the object an assignment replaces is released); the builder writes it
    /// at an assignment where that matters (`frame_set`), and a mapped
    /// `arguments` object's formals are written through.
    FrameStore(u32),
    /// A closure of the script's inner function `index` (a gcthing index)
    /// over environment `args[0]` (`Lambda`).
    JsLambda(u32),

    // Objects.
    LoadField(AtomId),
    /// `load_field` through a receiver whose type proves the field's slot
    /// (its layout claim with SLOTS, one slot across the claim's keys):
    /// the slot's value, of the field's claimed type. Not a terminator:
    /// it cannot miss, GC, throw or run JS (MIR-MEMORY.md §1.1), so
    /// value numbering and LICM treat it as a pure read of its field.
    LoadSlot(AtomId),
    /// `store_field` through a receiver whose type proves the field's slot
    /// and TYPES, of a value conforming to the field's claim: the slot
    /// written (RANGES dropped, as every store does). Not a terminator: it
    /// cannot miss, demote, GC, throw or run JS.
    StoreSlot(AtomId),
    StoreField(AtomId),
    InitField(AtomId),
    PublishLayout,
    NewObject(LayoutKey),
    /// An object literal's allocation (`NewInit`/`NewObject`) at a site
    /// the analysis stamps: `n` slots from the site's alloc cell (ok: the
    /// fresh object, boxed; err: the helper's OOM). Not a fence: an
    /// allocation writes nothing that exists (MIR-MEMORY.md §4).
    LitNew(u32),
    /// An `InitProp` of a literal whose row is layout `K` (its site's):
    /// the value into the property's slot, by the site's add transition
    /// (ok; err: OOM). It writes `Field(K, name)` of an object nothing
    /// else can reach yet: not a fence.
    LitInit(AtomId, LayoutKey),
    NewArray,
    LoadElem,
    /// The RANGES duty (as `InitElem`'s).
    /// ... and whether it also appends (past the initialized length, or
    /// into a hole, where the prototypes' proof allows: it then writes
    /// the lengths too), else fails; without, only an overwrite.
    StoreElem(bool, bool),
    LoadTa,
    StoreTa,
    LengthArray,
    LengthString,
    LengthTa,
    ElementsPtr,
    StrCharCodeAt,

    // Globals and environments.
    LoadGName(BindingId),
    StoreGName(BindingId),
    /// The global object (the realm's, fixed for the program) -> object.
    GlobalObject,
    EnvCurrent,
    /// The callee of the function environment `hops` links up the frame's
    /// chain (`EnvCallee`, `super` in an arrow or eval) -> object.
    EnvCallee(u32),
    /// The script's object gcthing `index` (`Object`, `CallSiteObj`: a
    /// template or singleton, not a copy) -> object.
    ObjectLit(u32),
    /// Make environment `e` (boxed) the frame's current one: a scope's
    /// entry (write-through, as baseline's frame keeps it).
    EnvSet,
    /// Make the frame's current environment's enclosing one current: a
    /// scope's exit (`PopLexicalEnv`, `LeaveWith`).
    EnvPop,
    EnvParent,
    EnvLoad(EnvSlot),
    EnvStore(EnvSlot),

    // Calls. Operands: callee (or new.target pair), `this`, args.
    Call,
    /// A call of an iterator method (`CallIter`): `call`, whose uncallable
    /// callee throws the iterator protocol's error.
    CallIter,
    /// A direct `eval` (`Eval`/`StrictEval` at pc): `call`'s operands; the
    /// code runs in the frame's environment.
    CallEval(u32),
    CallDirect,
    /// `new`: operands callee, `this` (the IS_CONSTRUCTING magic), args,
    /// new.target. The site's sized-allocation slot count and early stamp
    /// word (`stamp::construct_nslots`/`construct_alloc_word`).
    Construct(u32, u32),
    /// The `this` of a construct (`callee, new.target`): a fresh object
    /// sized `nslots` and seeded with the alloc word, as a direct construct
    /// makes it (the site's construct cell, else `create_this`).
    CreateThis(u32, u32),
    /// `create_this` of a proven scripted constructor that is its own
    /// new.target, with its `prototype` (`callee, proto`): a fresh object
    /// sized `nslots`, seeded with the alloc word, whose prototype is
    /// `proto` (or Object.prototype if `proto` is no object). Runs no code;
    /// an allocation (MIR-MEMORY.md §6).
    NewThis(u32, u32),
    /// `new_this` with the first `n` fields of layout `key` added, their
    /// values the operands after `callee, proto` (a constructor whose body
    /// starts by storing them: MIR.md M5q): the object under construction
    /// with `n` fields, as that many `init_field`s would leave it. Runs no
    /// code; `fail` where an add would not be plain (a setter or read-only
    /// property of the name on the prototype chain), before anything
    /// observable.
    NewThisInit {
        nslots: u32,
        word: u32,
        key: LayoutKey,
        types: bool,
        n: u32,
    },
    /// Whether function `f` is a constructor (its flags): an arrow
    /// function or a method is not.
    FnIsCtor,
    /// Whether object `o` emulates `undefined` (`document.all`): what
    /// loose equality with null or undefined asks of an object.
    ObjEmulatesUndef,
    /// Advance boxed `v`'s prefix-stamped object to its full layout key
    /// when its bits and shape allow (the module's restamp descriptor
    /// `i`): the two-phase restamp at an init delegate's or fill script's
    /// returns, or after a fill sequence's last add.
    Restamp(u32),
    /// Write class word `w` into a freshly allocated object literal or
    /// array (`lit_stamps_in`, `array_stamp_in`): nothing can have read
    /// the old word, and the claims it seeds hold vacuously.
    StampFresh(u32),
    /// A layout constructor's first stamp of its completed `this`
    /// (layout, field count, kept bits): a no-op unless `this` is an
    /// object of this constructor still under construction.
    CtorStamp(u32, u32, u32),
    /// `publish_layout` at its earliest point (MIR.md §2.3): the same
    /// stamp before a call made while `this` may still be under
    /// construction (`ctor_publish`), only for an object carrying this
    /// constructor's own early key (an unkeyed one waits for the return).
    CtorPublish(u32, u32, u32),
    CallNative(NativeId),
}

/// What a successor edge means to its terminator.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SuccRole {
    Target,
    Then,
    Else,
    Case(u32),
    Default,
    Ok,
    Fail,
    OkClean,
    OkDirty,
    Err,
}

impl SuccRole {
    pub fn name(self) -> String {
        match self {
            SuccRole::Target => "target".into(),
            SuccRole::Then => "then".into(),
            SuccRole::Else => "else".into(),
            SuccRole::Case(n) => format!("case{n}"),
            SuccRole::Default => "default".into(),
            SuccRole::Ok => "ok".into(),
            SuccRole::Fail => "fail".into(),
            SuccRole::OkClean => "ok_clean".into(),
            SuccRole::OkDirty => "ok_dirty".into(),
            SuccRole::Err => "err".into(),
        }
    }

    /// Whether this edge may carry the op's outputs.
    pub fn carries_outputs(self) -> bool {
        matches!(self, SuccRole::Ok | SuccRole::OkClean | SuccRole::OkDirty)
    }
}

const OK_FAIL: &[SuccRole] = &[SuccRole::Ok, SuccRole::Fail];
const CLEAN_DIRTY_ERR: &[SuccRole] = &[SuccRole::OkClean, SuccRole::OkDirty, SuccRole::Err];
const OK_ERR: &[SuccRole] = &[SuccRole::Ok, SuccRole::Err];
const OK_FAIL_ERR: &[SuccRole] = &[SuccRole::Ok, SuccRole::Fail, SuccRole::Err];

impl Opcode {
    /// The successor edges this op has, in order. Empty for non-terminators
    /// and for the exiting terminators.
    pub fn roles(&self) -> Vec<SuccRole> {
        use Opcode::*;
        match self {
            Jump => vec![SuccRole::Target],
            Br => vec![SuccRole::Then, SuccRole::Else],
            Switch(n) => (0..*n)
                .map(SuccRole::Case)
                .chain([SuccRole::Default])
                .collect(),
            SwitchStr(n) => (0..*n)
                .map(SuccRole::Case)
                .chain([SuccRole::Default, SuccRole::Fail])
                .collect(),
            GuardUnbox(_)
            | GuardTags(_)
            | GuardKind(_)
            | GuardLayout { .. }
            | GuardCtor { .. }
            | GuardSingleton(_)
            | GuardScript(_)
            | MethodLoad { .. }
            | F64ToIntExact
            | IntToI32
            | GuardRange(_)
            | CheckFuse(_)
            | CheckBinding(..)
            | CheckNative(_)
            | AccessorProbe(..)
            | I32Ovf(_)
            | LoadElem
            | StoreElem(..)
            | LoadTa
            | StoreTa
            | StrCharCodeAt
            | GetPropData(_)
            | InitField(_) => OK_FAIL.to_vec(),
            JsAdd | JsBinop(_) | JsUnop(_) | JsCompare(_) | JsToNumeric | JsGetProp(_)
            | JsSetProp(..) | JsGetElem | JsSetElem(..) | JsBindGName(_) | JsSetName(..)
            | LoadField(_) | StoreField(_) | Call | CallIter | CallEval(_) | CallDirect
            | Construct(..) | CallNative(_) | JsGetName(_) | CreateThis(..) => {
                CLEAN_DIRTY_ERR.to_vec()
            }
            // Clean if the callee's baseline rest demoted nothing.
            ExitInline { .. } => CLEAN_DIRTY_ERR.to_vec(),
            // Allocations: no kill, so no effect report.
            JsLambda(_) | LitNew(_) | LitInit(..) => OK_ERR.to_vec(),
            // The global `this`, or a primitive's wrapper: no user code.
            JsBoxThis | NewThis(..) => OK_ERR.to_vec(),
            NewThisInit { .. } => OK_FAIL_ERR.to_vec(),
            JsRt(_) | ApplyFwd => CLEAN_DIRTY_ERR.to_vec(),
            ArgsObject | RestArray(_) => OK_ERR.to_vec(),
            JsThrow => vec![SuccRole::Err],
            Prim(_) | GetElemData => OK_FAIL_ERR.to_vec(),
            SetPropData(_) | SetElemData(_) => {
                vec![
                    SuccRole::OkClean,
                    SuccRole::OkDirty,
                    SuccRole::Fail,
                    SuccRole::Err,
                ]
            }
            _ => vec![],
        }
    }

    pub fn is_terminator(&self) -> bool {
        matches!(
            self,
            Opcode::Return
                | Opcode::Exit { .. }
                | Opcode::ExitThrow { .. }
                | Opcode::GenSuspend { .. }
                | Opcode::Unreachable
        ) || !self.roles().is_empty()
    }

    /// The frame-state split of the operands of an exit, a throw or an
    /// inline exit.
    pub fn frame_operands(&self) -> Option<(Pc, u32, u32)> {
        match *self {
            Opcode::ExitInline {
                pc, nargs, nlocals, ..
            }
            | Opcode::GenSuspend {
                pc, nargs, nlocals, ..
            } => Some((pc, nargs, nlocals)),
            _ => self.exit_shape(),
        }
    }

    /// The frame-state split of an exit's operands, if this is one: an
    /// op that leaves the function for its own baseline body.
    pub fn exit_shape(&self) -> Option<(Pc, u32, u32)> {
        match *self {
            Opcode::Exit { pc, nargs, nlocals } | Opcode::ExitThrow { pc, nargs, nlocals } => {
                Some((pc, nargs, nlocals))
            }
            _ => None,
        }
    }
}

/// An exit's (or onramp root's) operands split into the frame's parts.
#[derive(Clone, Copy, Debug)]
pub struct FrameParts<'a, T> {
    pub this: &'a T,
    pub args: &'a [T],
    pub locals: &'a [T],
    pub rval: &'a T,
    pub stack: &'a [T],
}

/// Split `ops` (`this`, args, locals, rval, stack) by `nargs` and
/// `nlocals`; `None` if there are too few.
pub fn frame_parts<T>(ops: &[T], nargs: u32, nlocals: u32) -> Option<FrameParts<'_, T>> {
    let (na, nl) = (nargs as usize, nlocals as usize);
    if ops.len() < 2 + na + nl {
        return None;
    }
    Some(FrameParts {
        this: &ops[0],
        args: &ops[1..1 + na],
        locals: &ops[1 + na..1 + na + nl],
        rval: &ops[1 + na + nl],
        stack: &ops[2 + na + nl..],
    })
}

/// An op's typing: the types of its (non-terminator) results and of its
/// (terminator) outputs.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Sig {
    pub results: Vec<Type>,
    pub outputs: Vec<Type>,
}

impl Sig {
    fn result(t: Type) -> Sig {
        Sig {
            results: vec![t],
            outputs: vec![],
        }
    }

    fn output(t: Type) -> Sig {
        Sig {
            results: vec![],
            outputs: vec![t],
        }
    }

    fn none() -> Sig {
        Sig::default()
    }
}

mod effects;
mod signature;
pub use self::effects::*;
pub use self::signature::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::module::{FieldDef, Layout};

    fn m() -> Module {
        let mut m = Module::default();
        let x = m.intern_atom(&crate::ids::JsString::from("x"));
        let o = m.intern_atom(&crate::ids::JsString::from("o"));
        m.layouts.insert(
            LayoutKey::new(3),
            Layout {
                fields: vec![
                    Some(FieldDef {
                        name: x,
                        claim: Type::val(TagSet::INT32),
                    }),
                    Some(FieldDef {
                        name: o,
                        claim: Type::Val(VSet::new(
                            TagSet::OBJECT,
                            NumInfo::TOP,
                            ObjInfo {
                                kind: ObjKind::Plain,
                                singleton: None,
                                layout: Some(LayoutClaim {
                                    keys: KeyRange::one(LayoutKey::new(4)),
                                    types: true,
                                    slots: true,
                                    closed: false,
                                    state: LayoutState::Published,
                                }),
                            },
                            StrInfo::TOP,
                        )),
                    }),
                ],
                named: vec![],
                elements: None,
                closed: false,
            },
        );
        m
    }

    fn k3(types: bool) -> Type {
        Type::Obj(ObjInfo {
            kind: ObjKind::Plain,
            singleton: None,
            layout: Some(LayoutClaim {
                keys: KeyRange::one(LayoutKey::new(3)),
                types,
                slots: true,
                closed: false,
                state: LayoutState::Published,
            }),
        })
    }

    #[test]
    fn every_opcode_has_a_shape() {
        // Terminators with successors, exiting terminators, and plain ops
        // are disjoint, and the kill site follows from the shape.
        let m = m();
        let ops = [
            Opcode::Jump,
            Opcode::GuardTags(TagSet::INT32),
            Opcode::Call,
            Opcode::JsGetName(AtomId::from_u32(0)),
            Opcode::StoreGName(BindingId::from_u32(0)),
            Opcode::Box,
        ];
        let sites: Vec<_> = ops
            .iter()
            .map(|op| op.kill_site(&effects(op, &[], &m)))
            .collect();
        assert_eq!(
            sites,
            [
                KillSite::None,
                KillSite::None,
                KillSite::DirtyEdge,
                KillSite::DirtyEdge,
                KillSite::Op,
                KillSite::None
            ]
        );
        assert!(Opcode::Return.is_terminator());
        assert!(!Opcode::Box.is_terminator());
    }

    #[test]
    fn unbox_requires_proof() {
        let m = m();
        let n = Type::val(TagSet::NUMBER);
        assert!(signature(&Opcode::Unbox(UnboxKind::I32), &[n], &m).is_err());
        assert!(signature(&Opcode::Unbox(UnboxKind::F64Num), &[n], &m).is_ok());
        let g = signature(&Opcode::GuardUnbox(UnboxKind::I32), &[n], &m).unwrap();
        assert_eq!(g.outputs, vec![Type::I32_TOP]);
        let b = signature(&Opcode::Box, &[Type::i32_range(0, 9)], &m).unwrap();
        let u = signature(&Opcode::Unbox(UnboxKind::I32), &b.results, &m).unwrap();
        assert_eq!(u.results, vec![Type::i32_range(0, 9)]);
    }

    #[test]
    fn magic_is_boundary_only() {
        // `Val(⊤)` includes magic values (a frame slot can hold one):
        // nothing unboxes it, and a tag guard without `magic` removes it.
        let m = m();
        assert!(TagSet::MAGIC.subset_of(VSet::TOP.tags));
        let js = Type::val(TagSet::JS);
        assert!(is_subtype(&js, &Type::VAL_TOP));
        assert!(!is_subtype(&Type::VAL_TOP, &js));
        let g = signature(&Opcode::GuardTags(TagSet::JS), &[Type::VAL_TOP], &m).unwrap();
        assert_eq!(g.outputs, vec![js]);
        for k in UnboxKind::ALL {
            assert!(signature(&Opcode::Unbox(k), &[Type::val(TagSet::MAGIC)], &m).is_err());
            let g = signature(&Opcode::GuardUnbox(k), &[Type::VAL_TOP], &m).unwrap();
            assert!(!matches!(g.outputs[0], Type::Val(v) if v.tags.magic));
        }
        // Boxing never yields magic.
        let b = signature(&Opcode::Box, &[Type::I32_TOP], &m).unwrap();
        assert!(!matches!(b.results[0], Type::Val(v) if v.tags.magic));
    }

    #[test]
    fn arith_ranges() {
        let m = m();
        let s = signature(
            &Opcode::I32Ovf(ArithOp::Add),
            &[Type::i32_range(0, 10), Type::i32_range(-5, 5)],
            &m,
        )
        .unwrap();
        assert_eq!(s.outputs, vec![Type::i32_range(-5, 15)]);
        let big = Type::int_range(0, 1 << 52);
        assert!(signature(&Opcode::IntArith(ArithOp::Add), &[big, big], &m).is_ok());
        assert!(signature(&Opcode::IntArith(ArithOp::Mul), &[big, big], &m).is_err());
    }

    #[test]
    fn field_ops_follow_the_claim() {
        let m = m();
        let x = AtomId::from_u32(0);
        let o = AtomId::from_u32(1);
        let s = signature(&Opcode::LoadField(x), &[k3(true)], &m).unwrap();
        assert_eq!(s.outputs, vec![Type::val(TagSet::INT32)]);
        // Shallow: the child's layout claim is not carried.
        let s = signature(&Opcode::LoadField(o), &[k3(true)], &m).unwrap();
        assert_eq!(s.outputs[0].obj_info().unwrap().layout, None);
        assert_eq!(s.outputs[0].obj_info().unwrap().kind, ObjKind::Plain);
        // Without `types`, the identity alone: any val.
        let s = signature(&Opcode::LoadField(x), &[k3(false)], &m).unwrap();
        assert_eq!(s.outputs, vec![Type::VAL_TOP]);
        // Stores require conformance.
        assert!(signature(
            &Opcode::StoreField(x),
            &[k3(true), Type::val(TagSet::INT32)],
            &m
        )
        .is_ok());
        assert!(signature(
            &Opcode::StoreField(x),
            &[k3(true), Type::val(TagSet::NUMBER)],
            &m
        )
        .is_err());
    }
}
