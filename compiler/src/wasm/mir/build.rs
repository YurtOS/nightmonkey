//! The JSOps-to-MIR builder (`docs/MIR.md` §8), for the M2 subset: locals,
//! args, `this` and the operand stack as SSA values; int32 and double
//! arithmetic and compares, with the generic `js.*` ops as the fallback;
//! branches, loops and returns. Any other op declines the script, which
//! then runs in baseline alone.
//!
//! **Types.** Every frame slot has an abstract type: a raw `I32`, `F64` or
//! `Bool`, or a boxed `Val` with a tag set. Each bytecode basic block
//! starts from fixed entry types (its MIR block's params), and those come
//! from a fixpoint: the builder runs over the whole script, recording the
//! types that flow into every block; where they are wider than the block's
//! current entry types, the entry types widen and the builder runs again.
//! The run in which nothing widens is the result, so the type policy
//! exists once, in the emitting code. The widening lattice is small
//! (`I32 < F64`, and everything else joins to `Val` of the union of tags),
//! so the fixpoint comes quickly.
//!
//! **Guard-at-defs.** Formals with an `arg_types` claim are guarded at the
//! entry root: int32 to `I32`, numeric to `F64`. A failing guard exits to
//! baseline at pc 0.
//!
//! **Exits.** Every fallible op's `fail` edge exits at its own pc with the
//! state from before the op, since nothing observable has happened (§5.1's
//! resume rule), and every throwing op's `err` edge goes to its pc's throw
//! block (§5.3). Both are shared per pc and carry the whole frame, boxed.

use std::collections::BTreeMap;

use crate::bytecode::{BytecodeParser, JSOp, Script};
use crate::facts::Claim;
use crate::ids::{ArgIndex, Pc, ScriptId};
use crate::mir;
use crate::mir::func::{Edge, EdgeArg, FrameShape, LoopDecl, Root, RootKind};
use crate::mir::ops::{
    ArithOp, BindingCheck, BitOp, Cc, ConstVal, F64Op, JsBinop, JsCc, JsUnop, MathFn, NumRepr,
    Opcode, RtOp, UnboxKind,
};
use crate::mir::types::{
    FieldSet, IRange, KeyRange, LayoutClaim, LayoutState, ObjInfo, ObjKind, TagSet, Type as MType,
    I32_MAX, I32_MIN,
};
use crate::opsem::{PRIM_BIGINT, PRIM_INT32, PRIM_NULL, PRIM_UNDEFINED};
use crate::wasm::baseline::layout::{self, FrameLayout, StackDepths};
use crate::wasm::translate::{StampCtorIn, TranslateCtx};

mod calls;
mod core;
mod ctors;
mod dispatch;
mod elems;
mod entry;
mod exits;
mod flow;
mod guards;
mod op_arith;
mod op_calls;
mod op_control;
mod op_elems;
mod op_gen;
mod op_misc;
mod op_names;
mod op_objects;
mod op_stack;
mod op_super;
mod props;

type R<T> = Result<T, String>;

/// A frame slot's abstract type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ty {
    I32,
    F64,
    Bool,
    Val(TagSet),
    /// An object of one of `keys`' layouts, with its TYPES bit if the
    /// first flag says so and its SLOTS bit if the second does: the raw
    /// object (`obj{L…}`), proven by a guard that dominates (§4.3). The
    /// slot keeps the proof across joins and loops, so the field accesses
    /// through it take no guard of their own (an access with a slot
    /// prediction guards SLOTS where it is not yet proven).
    Obj(KeyRange, bool, bool),
    /// An object proven of `keys`' layouts before a fence that may have
    /// changed that (a call, a generic op): the raw object (`obj`). A
    /// field access through it guards the layout again, exiting on a miss
    /// (identity rarely changes), and refines the slot back to `Obj`.
    ObjHint(KeyRange),
    /// An object under construction for layout `key` with its first `n`
    /// fields added, TYPES held if the flag says so (MIR.md §2.3,
    /// `obj{Lkey constructing(n)}`, raw): made by an inlined `new`'s
    /// allocation, advanced by `init_field`, published by `publish_layout`
    /// once every field is there. Its field accesses below `n` are typed;
    /// a fence (anything that may reach the object) demotes it to a boxed
    /// object, which the rest of the constructor treats generically.
    Ctor(u32, FieldSet, bool),
    /// A native object (`obj{Native}`, raw): its elements are addressable
    /// (`load_elem`, `store_elem`). Immutable, so no fence kills it.
    Native,
    /// A typed array of this kind (`obj{TypedArray(k)}`, raw): its
    /// elements are `load_ta`/`store_ta`. Immutable.
    Ta(crate::opsem::TaKind),
    /// A closure of this script (boxed, `val{object}`): made by a
    /// `Lambda` here, or the caller's for a formal of a callee built for
    /// inlining at a call passing one (the context-sensitive target: a
    /// call of it has exactly this callee).
    Fn(ScriptId),
    /// A local, formal or rval that is dead here (never read before it is
    /// next written): no value, no block param. An exit passes it as
    /// `const.val dead`.
    Dead,
}

/// A number's raw representation, for arithmetic.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Num {
    I32,
    F64,
}

impl Ty {
    fn mir(self) -> MType {
        match self {
            Ty::I32 => MType::I32_TOP,
            Ty::F64 => MType::F64_TOP,
            Ty::Bool => MType::Bool,
            Ty::Val(t) => MType::val(t),
            Ty::Fn(_) => MType::val(TagSet::OBJECT),
            Ty::Obj(keys, types, slots) => MType::Obj(ObjInfo {
                layout: Some(LayoutClaim {
                    keys,
                    types,
                    slots,
                    closed: false,
                    state: LayoutState::Published,
                }),
                ..ObjInfo::TOP
            }),
            Ty::ObjHint(_) => MType::OBJ_TOP,
            Ty::Ctor(key, n, types) => MType::Obj(ObjInfo {
                layout: Some(LayoutClaim {
                    keys: KeyRange::one(crate::ids::LayoutKey::new(key)),
                    types,
                    slots: true,
                    closed: false,
                    state: LayoutState::Constructing(n),
                }),
                ..ObjInfo::TOP
            }),
            Ty::Native => MType::Obj(ObjInfo::kind(ObjKind::Native)),
            Ty::Ta(k) => MType::Obj(ObjInfo::kind(ObjKind::TypedArray(k))),
            Ty::Dead => unreachable!("a dead slot has no type"),
        }
    }

    fn tags(self) -> TagSet {
        match self {
            Ty::I32 => TagSet::INT32,
            Ty::F64 => TagSet::NUMBER,
            Ty::Bool => TagSet::BOOLEAN,
            Ty::Val(t) => t,
            Ty::Obj(..) | Ty::ObjHint(_) | Ty::Ctor(..) | Ty::Native | Ty::Ta(_) | Ty::Fn(_) => {
                TagSet::OBJECT
            }
            Ty::Dead => TagSet::NONE,
        }
    }

    fn join(self, o: Ty) -> Ty {
        match (self, o) {
            (a, b) if a == b => a,
            (Ty::Dead, x) | (x, Ty::Dead) => x,
            (Ty::I32, Ty::F64) | (Ty::F64, Ty::I32) => Ty::F64,
            (Ty::Obj(k1, t1, s1), Ty::Obj(k2, t2, s2)) => Ty::Obj(k1.hull(&k2), t1 && t2, s1 && s2),
            (Ty::Ctor(k1, n1, t1), Ty::Ctor(k2, n2, t2)) if k1 == k2 && n1 == n2 => {
                Ty::Ctor(k1, n1, t1 && t2)
            }
            (Ty::Obj(k1, ..) | Ty::ObjHint(k1), Ty::Obj(k2, ..) | Ty::ObjHint(k2)) => {
                Ty::ObjHint(k1.hull(&k2))
            }
            (a, b) => Ty::Val(a.tags().union(b.tags())),
        }
    }

    /// Whether a value of type `self` can be passed where `o` is expected
    /// (after `convert`). Anything fits a dead slot.
    fn fits(self, o: Ty) -> bool {
        o == Ty::Dead || (self != Ty::Dead && self.join(o) == o)
    }

    /// The raw number representation this type unboxes to without a
    /// check, if it is a number.
    fn num(self) -> Option<Num> {
        match self {
            Ty::I32 => Some(Num::I32),
            Ty::F64 => Some(Num::F64),
            Ty::Val(t) if t.is_nonempty_subset_of(TagSet::INT32) => Some(Num::I32),
            Ty::Val(t) if t.is_nonempty_subset_of(TagSet::NUMBER) => Some(Num::F64),
            _ => None,
        }
    }
}

const SWITCH_STR_MIN: usize = 3;

/// The representation a numeric claim is speculated in (§8's guard-at-defs):
/// int32 for an int32-only claim; with `int_first`, also for a number claim
/// with int32 in it that is not flagged double-first, exiting on a double; f64
/// for other number claims. `None` for anything else. Int-first suits formals,
/// element and name reads (indices, digits, counters: crypto's am3 gains 17%);
/// property reads and call results often hold doubles the claim does not flag
/// (raytrace's vectors), and an exit per double costs far more than the
/// ToInt32s int-first saves.
fn num_claim_ty(claim: crate::facts::Claim, int_first: bool) -> Option<Ty> {
    let prims = claim.prims();
    if claim.is_none() || claim.is_object() || prims.is_empty() {
        None
    } else if prims.subset_of(PRIM_INT32)
        || (int_first
            && prims.subset_of(crate::opsem::NUM)
            && prims.intersects(PRIM_INT32)
            && !claim.double_first())
    {
        Some(Ty::I32)
    } else if prims.subset_of(crate::opsem::NUM) {
        Some(Ty::F64)
    } else {
        None
    }
}

impl<'a> Shape<'a> {
    /// Whether callee `k` is small enough to inline at `pc` among
    /// `ntargets` targets: 150 bytes of bytecode for one
    /// target (200 in a loop), 500 each for several.
    /// Layout `cls`'s field `name`: its slot and value claim, from the
    /// layout rows (`this_layouts_in`).
    fn layout_field(
        &self,
        cls: u32,
        name: crate::ids::NameId,
    ) -> Option<(u32, crate::facts::Claim)> {
        let mut cache = self.layout_fields.borrow_mut();
        let m = cache.get_or_insert_with(|| {
            let mut m: BTreeMap<u32, BTreeMap<crate::ids::NameId, (u32, crate::facts::Claim)>> =
                BTreeMap::new();
            for li in self.ctx.this_layouts_in.values() {
                let e = m.entry(li.layout_id).or_default();
                for (i, &n) in li.fields.iter().enumerate() {
                    let claim = li
                        .masks
                        .get(i)
                        .copied()
                        .unwrap_or(crate::facts::Claim::NONE);
                    e.entry(n).or_insert((u32::try_from(i).unwrap(), claim));
                }
            }
            m
        });
        m.get(&cls)?.get(&name).copied()
    }

    /// Every class whose layout types field `name` (a class word's
    /// identity, layout key + 1), with its type for it; made on first use.
    fn name_classes(&self, name: crate::ids::NameId) -> std::rc::Rc<Vec<(u32, TagSet)>> {
        if let Some(v) = self.name_classes.borrow().get(&name) {
            return v.clone();
        }
        let v: Vec<(u32, TagSet)> = crate::wasm::mir::stamp::field_classes(self.ctx, name)
            .into_iter()
            .map(|(k, c)| (k, claim_tags(c)))
            .collect();
        let v = std::rc::Rc::new(v);
        self.name_classes.borrow_mut().insert(name, v.clone());
        v
    }

    /// TYPES where layout `k` predicts a type for any field
    /// (`layout_types_bit`).
    fn types_bit(&self, k: u32) -> u32 {
        crate::wasm::mir::stamp::layout_types_bit(self.ctx, k)
    }

    /// A construct site's allocation word (`typed_alloc_word`).
    fn alloc_word(&self, mono: Option<ScriptId>, site: crate::ids::Site) -> u32 {
        crate::wasm::mir::stamp::typed_alloc_word(self.ctx, mono, site)
    }

    /// Whether a call of script `k` may go straight to its compiled body:
    /// not a class constructor, generator or
    /// async function.
    fn direct_ok(&self, k: ScriptId) -> bool {
        matches!(
            self.ctx.source.object(crate::source::SourceObjectId::new(k.get())),
            crate::source::SourceObject::Script(ks) if !ks.is_class_ctor && !ks.is_generator_or_async
        )
    }

    /// Whether nothing in the script catches or closes on a throw (only
    /// loop notes; not a generator): baseline's landing for any throw is
    /// its error return, which reads nothing of the frame.
    fn throws_uncaught(&self) -> bool {
        !self.script.is_generator_or_async
            && self
                .script
                .try_notes
                .iter()
                .all(|t| t.kind == crate::bytecode::TryNoteKind::Loop)
    }

    /// Callee `k` built for inlining here (§5.5), if it may be: an
    /// inline-eligible script that is not this one, not a constructor
    /// that stamps its `this`, and small enough once built.
    fn callee(&self, k: ScriptId, ictx: InlineCtx) -> Option<std::rc::Rc<super::inline::Callee>> {
        self.callee_ctx(k, &[], None, None, ictx)
    }

    /// `callee`, built knowing which of its formals the call passes a
    /// known closure in (`Ty::Fn`, by formal; missing ones none), and for
    /// a `this` under construction (`Ty::Ctor` of these parts) where the
    /// call passes one: context-sensitive in the closures it passes and
    /// the receiver's construction state, and built where `ictx` says.
    fn callee_ctx(
        &self,
        k: ScriptId,
        fns: &[Option<ScriptId>],
        this_ctor: Option<(u32, FieldSet, bool)>,
        this_obj: Option<ThisObj>,
        ictx: InlineCtx,
    ) -> Option<std::rc::Rc<super::inline::Callee>> {
        let mut fns = fns.to_vec();
        while fns.last() == Some(&None) {
            fns.pop();
        }
        let key = (k, fns, this_ctor, this_obj, ictx);
        if let Some(c) = self.callees.borrow().get(&key) {
            return c.clone();
        }
        let c = self
            .build_callee(k, &key.1, this_ctor, this_obj, ictx)
            .map(std::rc::Rc::new);
        self.callees.borrow_mut().insert(key, c.clone());
        c
    }

    fn build_callee(
        &self,
        k: ScriptId,
        fns: &[Option<ScriptId>],
        this_ctor: Option<(u32, FieldSet, bool)>,
        this_obj: Option<ThisObj>,
        ictx: InlineCtx,
    ) -> Option<super::inline::Callee> {
        if k == self.sid {
            return None;
        }
        let names = self.names?;
        let crate::source::SourceObject::Script(ks) = self
            .ctx
            .source
            .object(crate::source::SourceObjectId::new(k.get()))
        else {
            return None;
        };
        if !super::inline_eligible(self.ctx, ks) {
            return None;
        }
        let nargs = usize::from(ks.nargs);
        let fns: Vec<Option<ScriptId>> = fns.iter().copied().take(nargs).collect();
        let (mut mm, f, this_out, sites) = match build_at(
            self.ctx, names, k, ks, false, ictx, &fns, this_ctor, this_obj,
        ) {
            Ok(r) => r,
            Err(_) => return None,
        };
        mm.script_addrs.insert(k, ks.addr);
        let max_depth = StackDepths::compute(ks).ok()?.max;
        let fenced = super::inline::fenced(&f, &mm);
        Some(super::inline::Callee {
            mm,
            f,
            max_depth,
            fenced,
            this_out,
            sites,
            reads_actuals: layout::reads_actuals(ks),
        })
    }
}

/// A constructor's leading field stores of its formals, as its bytecode
/// has them: after `FunctionThis; SetLocal L; Pop`, a run of `GetLocal L;
/// GetArg j; SetProp name; Pop`. Each entry is (the name's gcthing index,
/// the formal, the `SetProp`'s pc). A `new` inlining the constructor makes
/// the object with these fields (`new_this.init`) and builds the body for
/// it, skipping the stores; both sides read this one scan.
pub(super) fn ctor_formal_prefix(script: &Script) -> Vec<(u32, u16, Pc)> {
    let mut p = script.parser();
    let pc_of =
        |p: &BytecodeParser| Pc::new(u32::try_from(script.bytecode.len() - p.remaining()).unwrap());
    let mut out = vec![];
    if p.next_op() != Some(JSOp::FunctionThis) || p.next_op() != Some(JSOp::SetLocal) {
        return out;
    }
    let Some(local) = p.next_uint24() else {
        return out;
    };
    if p.next_op() != Some(JSOp::Pop) {
        return out;
    }
    loop {
        if p.next_op() != Some(JSOp::GetLocal) || p.next_uint24() != Some(local) {
            return out;
        }
        if p.next_op() != Some(JSOp::GetArg) {
            return out;
        }
        let Some(arg) = p.next_uint16() else {
            return out;
        };
        let pc = pc_of(&p);
        if !matches!(p.next_op(), Some(JSOp::SetProp | JSOp::StrictSetProp)) {
            return out;
        }
        let Some(name) = p.next_uint32() else {
            return out;
        };
        if p.next_op() != Some(JSOp::Pop) {
            return out;
        }
        out.push((name, arg, pc));
    }
}

/// A property access the analysis predicts: the receiver's layouts
/// `[lo, hi]`, and whether the field's predicted type is backed by the
/// stamp's TYPES bit: with it set on a valid stamp, every field holds a
/// value of its layout's predicted type (any type), so a read is of that
/// type unchecked.
#[derive(Clone, Copy, Debug)]
struct TypedSite {
    lo: u32,
    hi: u32,
    types: bool,
    /// The analysis's value claim, to guard at the def when the stamp
    /// does not back it (`!types`).
    claim: crate::facts::Claim,
    /// The field's predicted type over the layouts: what a read is (their
    /// union), and what a store must be to keep TYPES on every one of
    /// them (their intersection).
    load_tags: TagSet,
    store_tags: TagSet,
    /// Whether the field has a slot prediction (its layouts' rows place
    /// it): its access then needs SLOTS proven, and reads the slot; one
    /// without finds the slot through an IC, under TYPES alone.
    slotted: bool,
}

impl TypedSite {
    fn claim_ty(&self) -> MType {
        if self.types {
            MType::val(self.load_tags)
        } else {
            MType::VAL_TOP
        }
    }
}

/// The tags a likelier claim admits.
fn claim_tags(c: crate::facts::Claim) -> TagSet {
    TagSet {
        prims: c.prims(),
        object: c.bits() & crate::facts::Claim::OBJECT.bits() != 0,
        magic: false,
    }
}

/// A frame slot: the SSA value holding it now, and its type.
#[derive(Clone, Copy, Debug)]
struct Slot {
    v: mir::Value,
    ty: Ty,
}

impl Slot {
    /// A dead slot (`Ty::Dead`): its value is never used.
    fn dead() -> Slot {
        Slot {
            v: mir::Value::from_u32(u32::MAX),
            ty: Ty::Dead,
        }
    }
}

/// Build the MIR body for `script`, or decline with a reason.
pub fn build<'a>(
    ctx: &'a TranslateCtx<'a>,
    names: &'a crate::ids::Names,
    sid: ScriptId,
    script: &'a Script,
    is_global: bool,
) -> Result<(mir::Module, mir::Func), String> {
    build_at(
        ctx,
        names,
        sid,
        script,
        is_global,
        InlineCtx::ROOT,
        &[],
        None,
        None,
    )
    .map(|(m, f, _, _)| (m, f))
}

/// How many likely callees a call not inlined gets direct arms for.
const MAX_DIRECT_TARGETS: usize = 4;

/// How many classes a store lists (`field_claim`) before it names only
/// the receiver's predicted ones.
const MAX_STORE_CLASSES: usize = crate::wasm::mir::stamp::MAX_STORE_CLASSES;

/// The Math functions `math_call` types, by property name.
fn math_fn_named(s: &str) -> Option<MathFn> {
    Some(match s {
        "abs" => MathFn::Abs,
        "floor" => MathFn::Floor,
        "ceil" => MathFn::Ceil,
        "trunc" => MathFn::Trunc,
        "sqrt" => MathFn::Sqrt,
        "fround" => MathFn::Fround,
        "min" => MathFn::Min,
        "max" => MathFn::Max,
        "pow" => MathFn::Pow,
        "sin" => MathFn::Sin,
        "cos" => MathFn::Cos,
        _ => return None,
    })
}

fn math_fn_name(m: MathFn) -> &'static str {
    match m {
        MathFn::Abs => "abs",
        MathFn::Floor => "floor",
        MathFn::Ceil => "ceil",
        MathFn::Trunc => "trunc",
        MathFn::Sqrt => "sqrt",
        MathFn::Fround => "fround",
        MathFn::Min => "min",
        MathFn::Max => "max",
        MathFn::Pow => "pow",
        MathFn::Sin => "sin",
        MathFn::Cos => "cos",
        _ => "?",
    }
}

/// How deep inlining nests below a call site in a loop, and below one not
/// in a loop (the root site's).
const MAX_INLINE_DEPTH_LOOP: u32 = 8;
const MAX_INLINE_DEPTH: u32 = 4;
/// The most sites one function splices, its callees' own included,
/// counted depth-first.
const MAX_INLINE_SITES: u32 = crate::constants::MAX_INLINE_SITES;
/// The most targets a call site inlines (a guard chain on the script).
const MAX_INLINE_TARGETS: usize = crate::constants::MAX_INLINE_TARGETS;
/// Splice fuel (100000 waffle values, at the
/// ~4 values a MIR instruction lowers to): past this many instructions a
/// function splices nothing more, but callees of at most
/// `SMALL_SPLICE_BC` bytes, up to the harder line.
const SPLICE_FUEL_INSTS: usize = 25_000;
const SMALL_SPLICE_BC: usize = 160;
const SMALL_SPLICE_FUEL_INSTS: usize = 60_000;

/// `build`, as the callee of an inlining `depth` levels down.
fn build_at<'a>(
    ctx: &'a TranslateCtx<'a>,
    names: &'a crate::ids::Names,
    sid: ScriptId,
    script: &'a Script,
    is_global: bool,
    ictx: InlineCtx,
    formal_fns: &[Option<ScriptId>],
    this_ctor: Option<(u32, FieldSet, bool)>,
    this_obj: Option<ThisObj>,
) -> Result<(mir::Module, mir::Func, Option<(u32, FieldSet, bool)>, u32), String> {
    if is_global {
        return Err("global script".into());
    }
    // Direct eval reads the caller's interpreter frame: the script is
    // interpreted, and no tier lowers its op (`baseline::is_direct_eval`).
    if let Some(op) = script
        .parser()
        .opcodes()
        .find(|&op| crate::wasm::baseline::is_direct_eval(op))
    {
        return Err(format!(
            "{} ({op:?})",
            crate::wasm::baseline::UNSUPPORTED_OP
        ));
    }
    // A MIR script carries two bodies (MIR and baseline). An
    // Emscripten-sized function would dominate the batch's memory and
    // compile time for little gain, so it stays in baseline.
    const MAX_MIR_BYTECODE: usize = 32 * 1024;
    if script.bytecode.len() > MAX_MIR_BYTECODE {
        return Err(format!(
            "too large for MIR ({} bytecode bytes)",
            script.bytecode.len()
        ));
    }
    let fl = FrameLayout::of(script);
    // A resume re-enters with no actuals (`EnterNightResume`), which a
    // body reading them past its formals cannot rebase over.
    if script.is_generator_or_async && fl.rebase_vp {
        return Err("generator reading its actuals".into());
    }
    // The env chain is fixed for the whole activation (§5.1): the callee's
    // own environment, or the one the entry makes (`env_setup`); an op
    // that pushes a scope declines on its own.
    let depths = StackDepths::compute(script).map_err(|e| format!("stack depths ({e})"))?;
    let mut shape = Shape::of(ctx, sid, script, fl, depths)?;
    shape.names = Some(names);
    shape.apply_fwd =
        crate::wasm::translate::compute_apply_fwd_pcs(script, &ctx.facts.apply_sites, sid.get())
            .filter(|s| !s.is_empty());
    if shape.apply_fwd.is_none() {
        let length: Vec<u16> = "length".encode_utf16().collect();
        let is_length = |n: u32| {
            script.gcthings.get(n as usize).is_some_and(|&gc| {
                !gc.is_other()
                    && matches!(ctx.source.object(gc), crate::source::SourceObject::String(s) if s.chars() == length.as_slice())
            })
        };
        // A closed-over formal lives in the environment, where a mapped
        // arguments object would see another function's write to it.
        let formals_closed_over = script
            .body_scope
            .is_none_or(|b| match ctx.source.object(b) {
                crate::source::SourceObject::Scope(sd) => {
                    sd.bindings
                        .iter()
                        .take(usize::from(script.nargs))
                        .any(|&(_, _, slot)| slot.is_some())
                        || sd.bindings.len() < usize::from(script.nargs)
                }
                _ => true,
            });
        shape.args_reads =
            crate::wasm::translate::compute_args_reads(script, &is_length, formals_closed_over);
    }
    shape.inline_depth = ictx.depth;
    shape.ictx = ictx;
    shape.formal_fns = formal_fns.to_vec();
    shape.this_ctor = this_ctor;
    shape.this_obj = this_obj;
    // Its own layout's `this` with fields already: only a `new` that made
    // them from the leading stores (`inline_construct`); no other builder
    // enters a constructor's body with its own object under construction
    // past its start.
    if let Some((key, n, _)) = this_ctor.filter(|&(_, n, _)| n != FieldSet::EMPTY) {
        let n = n.as_prefix().map_or(0, |n| n as usize);
        if n > 0
            && ctx
                .stamp_ctors_in
                .get(&sid)
                .is_some_and(|si| si.layout_id == key)
        {
            let pre = ctor_formal_prefix(script);
            if pre.len() >= n {
                shape.prefill_pcs = pre[..n].iter().map(|&(_, _, pc)| pc).collect();
            }
        }
    }
    for (i, &gc) in script.gcthings.iter().enumerate() {
        if gc.is_other() {
            continue;
        }
        if let crate::source::SourceObject::String(s) = ctx.source.object(gc) {
            let Some(n) = names.lookup(s.chars()) else {
                continue;
            };
            let i = u32::try_from(i).unwrap();
            if let Some(fg) = ctx.fused_gnames.get(&n) {
                shape.fused.insert(i, *fg);
            }
            if let Some(c) = ctx.facts.gname_types.get(&n) {
                shape.gname_types.insert(i, *c);
            }
            if let Some(&bid) = ctx.syn_gnames.get(&n) {
                shape.syn_bids.insert(i, bid);
            }
        }
    }
    let mut table: BTreeMap<Pc, Vec<Ty>> = BTreeMap::new();
    // Loop-header slots guarded on the way into their loop (§10.2).
    let mut hoist: BTreeMap<(Pc, usize), Ty> = BTreeMap::new();
    // Within a run, a block's entry types join every forward edge into it
    // (`Run::pending`), so a run misses only what a loop's back edges
    // bring. Each rerun widens some loop header's entry types, and there
    // are finitely many widenings; the bound is a backstop.
    for _ in 0..256 {
        let mut run = Run::new(&shape, &table, &hoist);
        run.build()?;
        let new_hoists: Vec<((Pc, usize), Ty)> = run
            .hoist_req
            .iter()
            .filter(|(k, _)| !hoist.contains_key(k))
            .map(|(&k, &t)| (k, t))
            .collect();
        if !run.widen && new_hoists.is_empty() {
            let mm = std::mem::take(&mut run.mm);
            let this_out = run.this_out.flatten();
            let sites = run.inline_sites;
            return Ok((mm, run.finish(), this_out, sites));
        }
        let out = std::mem::take(&mut run.out);
        drop(run);
        if !new_hoists.is_empty() {
            // Entry types only widen across runs: a narrower header starts
            // them over (the hoist set only grows, so this terminates).
            hoist.extend(new_hoists);
            table.clear();
            continue;
        }
        for (pc, tys) in out {
            let e = table.entry(pc).or_insert_with(|| tys.clone());
            for (a, b) in e.iter_mut().zip(&tys) {
                *a = a.join(*b);
            }
        }
    }
    Err("BUG: type fixpoint did not converge".into())
}

/// A decoded op.
#[derive(Clone, Copy, Debug)]
struct Op {
    pc: Pc,
    op: JSOp,
}

/// The `Add`/`Sub` ops whose result is consumed, directly on the operand
/// stack within its basic block, only by truncating ops (ToInt32 of it)
/// or by other such adds and subs. A sum of int32s, and of such sums, is
/// exact as a double well within 2^53, and ToInt32 of it is the sum
/// modulo 2^32: the int32 wrapping add. (Not `Mul`: a double product can
/// round, which is what `Math.imul` is for.)
fn int32_demand(script: &Script, ops: &[Op]) -> std::collections::BTreeSet<Pc> {
    use std::collections::{BTreeMap, BTreeSet};
    let leaders = layout::leaders(script);
    // Per producer pc: its consumers' pcs, or `None` for a use the model
    // does not follow (a stack shuffle, a block boundary, a second use).
    let mut consumers: BTreeMap<Pc, Vec<Option<Pc>>> = BTreeMap::new();
    let mut stack: Vec<Option<Pc>> = vec![];
    for o in ops {
        if leaders.contains(&o.pc) {
            for p in stack.drain(..).flatten() {
                consumers.entry(p).or_default().push(None);
            }
        }
        let (Some(nuses), ndefs) = (o.op.nuses(), o.op.ndefs()) else {
            for p in stack.drain(..).flatten() {
                consumers.entry(p).or_default().push(None);
            }
            continue;
        };
        let nuses = nuses as usize;
        let simple = !matches!(
            o.op,
            JSOp::Dup | JSOp::Dup2 | JSOp::DupAt | JSOp::Swap | JSOp::Pick | JSOp::Unpick
        );
        if stack.len() < nuses {
            // Entered mid-stack (after a leader): the unknown values below.
            let missing = nuses - stack.len();
            stack.splice(0..0, std::iter::repeat_n(None, missing));
        }
        for p in stack.drain(stack.len() - nuses..).flatten() {
            consumers
                .entry(p)
                .or_default()
                .push(if simple { Some(o.pc) } else { None });
        }
        for _ in 0..ndefs {
            stack.push(if ndefs == 1 && simple {
                Some(o.pc)
            } else {
                None
            });
        }
    }
    for p in stack.drain(..).flatten() {
        consumers.entry(p).or_default().push(None);
    }
    let op_at: BTreeMap<Pc, JSOp> = ops.iter().map(|o| (o.pc, o.op)).collect();
    let truncating = |op: JSOp| {
        matches!(
            op,
            JSOp::BitOr
                | JSOp::BitAnd
                | JSOp::BitXor
                | JSOp::Lsh
                | JSOp::Rsh
                | JSOp::Ursh
                | JSOp::BitNot
        )
    };
    let mut ok: BTreeSet<Pc> = BTreeSet::new();
    // Consumers come after producers: walk backward so a consuming add's
    // verdict is known first.
    for o in ops.iter().rev() {
        if !matches!(o.op, JSOp::Add | JSOp::Sub) {
            continue;
        }
        let Some(cs) = consumers.get(&o.pc) else {
            continue;
        };
        let all = !cs.is_empty()
            && cs.iter().all(|c| match c {
                Some(c) => truncating(op_at[c]) || ok.contains(c),
                None => false,
            });
        if all {
            ok.insert(o.pc);
        }
    }
    ok
}

/// What the runs share: the script's decoded shape.
/// A callee build's context (`callee_in`): the script, the known closures
/// its formals receive, and the constructing `this` it receives, if any.
type CalleeKey = (
    ScriptId,
    Vec<Option<ScriptId>>,
    Option<(u32, FieldSet, bool)>,
    Option<ThisObj>,
    InlineCtx,
);

/// A published object the inlining call passes as `this`, of a type its
/// guards proved: layouts `lo..=hi` (ids), TYPES, SLOTS. The callee is
/// built for it, its own `this` guard (the method's `this_layouts`)
/// subsumed where the type proves it.
type ThisObj = (u32, u32, bool, bool);

/// Where a callee is built for inlining: its
/// depth, the sites it may splice itself, whether the root call site is
/// in a loop (which sets the depth cap), and the loop nest around it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct InlineCtx {
    depth: u32,
    budget: u32,
    root_in_loop: Option<bool>,
    outer_nest: u32,
    /// The call site entering a callee whose forward sites the analysis
    /// resolves per entry (`apply_targets_in`: a shared wrapper's
    /// `this.initialize.apply(this, arguments)`), for them; else `None`,
    /// so other callees are built once whatever the site.
    entry: Option<crate::ids::Site>,
}

impl InlineCtx {
    const ROOT: InlineCtx = InlineCtx {
        depth: 0,
        budget: MAX_INLINE_SITES,
        root_in_loop: None,
        outer_nest: 0,
        entry: None,
    };
}

struct Shape<'a> {
    ctx: &'a TranslateCtx<'a>,
    sid: ScriptId,
    script: &'a Script,
    nargs: u32,
    nlocals: u32,
    depths: StackDepths,
    ops: Vec<Op>,
    leaders: std::collections::BTreeSet<Pc>,
    /// Loop intervals `[header, end)`.
    loops: BTreeMap<Pc, Pc>,
    /// Per pc, which of the frame's first `frame_len` slots (`this`,
    /// formals, locals, rval) are live on entry to the op there: read
    /// before they are next written, on some path.
    live: BTreeMap<Pc, Vec<bool>>,
    /// The `Add`/`Sub` ops whose result only ever reaches ToInt32 (`|`,
    /// `&`, `^`, shifts, or another such add or sub): their int32 form can
    /// wrap instead of checking for overflow (§10.6's numeric demand).
    wrap_ok: std::collections::BTreeSet<Pc>,
    /// Per gcthing index naming a fused global: its fuse and literal.
    fused: BTreeMap<u32, crate::wasm::translate::FusedGname>,
    /// Per gcthing index naming a global: the analysis's likely type
    /// (`gname_types`), guarded at the def.
    gname_types: BTreeMap<u32, crate::facts::Claim>,
    /// Per gcthing index naming a syntactic global: its binding row
    /// (`syn_gnames`), for `check.binding`/`load_gname`/`store_gname`.
    syn_bids: BTreeMap<u32, u32>,
    names: Option<&'a crate::ids::Names>,
    /// The `T.apply(this, arguments)` sites whose arguments object is
    /// never observed (`compute_apply_fwd_pcs`), if the script has
    /// them.
    apply_fwd: Option<rustc_hash::FxHashSet<Pc>>,
    /// The `.length` reads and `GetElem`s of an arguments object that has
    /// no other use (`compute_args_reads`), so it is never made either: a
    /// read is the frame's actual count or one of its actuals.
    args_reads: Option<rustc_hash::FxHashSet<Pc>>,
    /// How deep in inlining this build is (0: a script's own).
    inline_depth: u32,
    /// Where this build is inlined (`InlineCtx::ROOT` for a script's own).
    ictx: InlineCtx,
    /// Callees built for inlining, by script; `None` if one cannot be.
    callees: std::cell::RefCell<BTreeMap<CalleeKey, Option<std::rc::Rc<super::inline::Callee>>>>,
    /// For a build for inlining at a call passing known closures: the
    /// formals holding one (`Ty::Fn`), by formal.
    formal_fns: Vec<Option<ScriptId>>,
    /// The object under construction the inlining call passes as `this`
    /// (`Ty::Ctor`: layout key, fields added, TYPES): the body is built
    /// for it, with no entry guard, as a context-sensitive callee build.
    this_ctor: Option<(u32, FieldSet, bool)>,
    /// The published object of a known type the inlining call passes as
    /// `this` (`ThisObj`).
    this_obj: Option<ThisObj>,
    /// A constructor built for its own `this` already holding its first
    /// fields (`new_this.init` made them from its leading stores): the pcs
    /// of those stores, which this build skips (`ctor_formal_prefix`).
    prefill_pcs: Vec<Pc>,
    /// `name_classes`, by name, made on first use.
    #[allow(clippy::type_complexity)]
    name_classes: std::cell::RefCell<BTreeMap<crate::ids::NameId, std::rc::Rc<Vec<(u32, TagSet)>>>>,
    /// The layout rows' fields, by layout (`layout_field`), made on first
    /// use.
    #[allow(clippy::type_complexity)]
    layout_fields: std::cell::RefCell<
        Option<BTreeMap<u32, BTreeMap<crate::ids::NameId, (u32, crate::facts::Claim)>>>,
    >,
}

impl<'a> Shape<'a> {
    fn of(
        ctx: &'a TranslateCtx<'a>,
        sid: ScriptId,
        script: &'a Script,
        fl: FrameLayout,
        depths: StackDepths,
    ) -> R<Shape<'a>> {
        let mut ops = vec![];
        let len = script.bytecode.len();
        let mut p = script.parser();
        loop {
            let pc = Pc::new(u32::try_from(len - p.remaining()).unwrap());
            let Some(op) = p.next_op() else { break };
            ops.push(Op { pc, op });
            let imm = usize::try_from(op.len()).unwrap() - 1;
            if imm > 0 && p.advance(imm).is_none() {
                return Err(format!("truncated {op:?} at {pc}"));
            }
        }
        let loops = crate::wasm::translate::scan_loop_intervals(script)
            .into_iter()
            .map(|(h, e)| (Pc::new(h), Pc::new(e)))
            .collect();
        if !layout::liveness_fits(script, fl.nargs, fl.nlocals) {
            return Err(format!(
                "script too large for the MIR tier ({} bytes x {} frame slots)",
                script.bytecode.len(),
                2 + fl.nargs + fl.nlocals
            ));
        }
        let live = layout::frame_liveness(script, fl.nargs, fl.nlocals);
        let wrap_ok = int32_demand(script, &ops);
        Ok(Shape {
            ctx,
            sid,
            script,
            nargs: fl.nargs,
            nlocals: fl.nlocals,
            depths,
            ops,
            leaders: layout::leaders(script),
            loops,
            live,
            wrap_ok,
            fused: BTreeMap::new(),
            gname_types: BTreeMap::new(),
            syn_bids: BTreeMap::new(),
            names: None,
            apply_fwd: None,
            args_reads: None,
            inline_depth: 0,
            ictx: InlineCtx::ROOT,
            callees: Default::default(),
            formal_fns: vec![],
            this_ctor: None,
            this_obj: None,
            prefill_pcs: vec![],
            name_classes: Default::default(),
            layout_fields: Default::default(),
        })
    }

    /// The op at `pc`, if one starts there.
    fn op_at(&self, pc: Pc) -> Option<JSOp> {
        let i = self.ops.binary_search_by_key(&pc, |o| o.pc).ok()?;
        Some(self.ops[i].op)
    }

    /// A parser positioned just after the opcode at `pc`.
    fn imms(&self, pc: Pc) -> BytecodeParser<'a> {
        let mut p = self.script.parser();
        p.advance(usize::try_from(pc.get()).unwrap() + 1)
            .expect("op in range");
        p
    }

    /// Whether the op after the one at `pc` consumes its result as
    /// `typeof` does (a name lookup then does not throw when unbound).
    fn next_is_typeof(&self, pc: Pc, op: JSOp) -> bool {
        let next = usize::try_from((pc + op.len()).get()).unwrap();
        self.script
            .bytecode
            .get(next)
            .and_then(|&b| JSOp::from_byte(b))
            .is_some_and(|o| matches!(o, JSOp::Typeof | JSOp::TypeofEq | JSOp::TypeofExpr))
    }

    /// The entry type the analysis predicts for formal `i` (guard-at-defs).
    /// A formal some call leaves out takes none: its `undefined` would
    /// fail the entry guard on every such call.
    fn arg_claim(&self, i: u32) -> Ty {
        if self.ctx.facts.omitted_formals.contains(&(self.sid, i)) {
            return Ty::Val(TagSet::ALL);
        }
        let claim = self
            .ctx
            .facts
            .arg_types
            .get(&(self.sid, ArgIndex::new(i + 1)))
            .copied()
            .unwrap_or(Claim::NONE);
        num_claim_ty(claim, true).unwrap_or(Ty::Val(TagSet::ALL))
    }
}

/// One run of the builder over the script.
struct Run<'s, 'a> {
    s: &'s Shape<'a>,
    table: &'s BTreeMap<Pc, Vec<Ty>>,
    /// Loop-header slots this run guards on the way into their loop, to
    /// their narrow type (`Native`, `Ta`).
    hoist: &'s BTreeMap<(Pc, usize), Ty>,
    /// Loop-header slots whose back edges bring a narrow object type where
    /// the header has a `Val`: guarded on entry in the next run.
    hoist_req: BTreeMap<(Pc, usize), Ty>,
    /// The types flowing into each block this run (joined).
    out: BTreeMap<Pc, Vec<Ty>>,
    f: mir::Func,
    /// The module tables the function references (its atoms).
    mm: mir::Module,
    blocks: BTreeMap<Pc, mir::Block>,
    preheaders: BTreeMap<Pc, mir::Block>,
    /// The entry types of every block entered so far this run.
    entry_types: BTreeMap<Pc, Vec<Ty>>,
    /// Edges to blocks not entered yet: a trampoline block per edge, with
    /// the frame's types there and the edge's source pc. Entering the
    /// block joins them into its entry types, then fills each trampoline
    /// with the conversions and the jump.
    pending: BTreeMap<Pc, Vec<(mir::Block, Vec<Ty>, Option<Pc>)>>,
    /// Whether a back edge brought types wider than its loop header's: the
    /// run is not the last.
    widen: bool,
    cur: mir::Block,
    /// Whether `cur` is open.
    live: bool,
    /// The frame: `this`, formals, locals, rval, then the operand stack.
    st: Vec<Slot>,
    /// The value standing for the elided arguments object at an apply
    /// forward site (`apply_forward`), if `Arguments` has run.
    args_placeholder: Option<mir::Value>,
    /// The locals the placeholder was stored into (the `arguments`
    /// binding, a `var a = arguments`: each written once,
    /// `compute_apply_fwd_pcs` / `compute_args_reads`); tracked by slot,
    /// since block params rename the value, and an onramp finds the real
    /// object there.
    args_locals: std::collections::BTreeSet<usize>,
    /// Values read from those locals onto the operand stack (renamed
    /// placeholders, or an onramp's real object): an exit before the op
    /// consuming one makes the object for it too.
    args_copies: std::collections::BTreeSet<mir::Value>,
    /// The op being built, its state before it, and its shared exit and
    /// throw blocks.
    pc: Pc,
    pre: Vec<Slot>,
    exit_blk: Option<mir::Block>,
    throw_blk: Option<mir::Block>,
    /// Call sites this run has inlined into (`MAX_INLINE_SITES`), and the
    /// callee instructions they spliced (`MAX_INLINE_TOTAL_INSTS`).
    inline_sites: u32,
    /// The context the last admitted site's callees were built in.
    admitted: InlineCtx,
    /// This op's fence renamings of `Obj` values (`fence_params`), for
    /// `repush`.
    renames: Vec<(mir::Value, Slot)>,
    /// The pc after the op being built (`js_keep`'s exits).
    next_pc: Option<Pc>,
    /// A builder invariant that failed where the code cannot return an error
    /// (`enter`); the run reports it when it ends.
    bug: Option<String>,
    /// The likely callees the next generic `call` gets (`attach_targets`).
    likely_targets: Vec<ScriptId>,
    /// Values read from a global by name, and the Math functions read off
    /// `Math` (`math_call`).
    gname_vals: BTreeMap<mir::Value, mir::entity::AtomId>,
    math_fns: BTreeMap<mir::Value, MathFn>,
    /// The element op being built is at an `elem_poly_sites` site.
    ta_poly_site: bool,
    /// The predicted type of the field the property store being built
    /// writes, for its TYPES maintenance (`field_mask` attachments).
    store_mask: Option<(Vec<(u32, TagSet)>, bool)>,
    /// Advisory classes (`hinted_site`): of values, and of the frame
    /// slots whose writes carry them.
    val_cls: BTreeMap<mir::Value, u32>,
    /// Literals of a stamped site under construction: the object (as
    /// `lit.new` made it) -> its row's layout key, for its `lit.init`s.
    lit_rows: BTreeMap<mir::Value, u32>,
    /// Boxed objects known to have been under construction for a layout
    /// (key, fields added, TYPES) when a fence demoted them from `Ctor`
    /// (§2.3), or as an inlined callee left them: a field add or a method
    /// call through one guards the construction state again
    /// (`guard.ctor`), and continues typed.
    ctor_hint: BTreeMap<mir::Value, (u32, FieldSet, bool)>,
    /// Raw objects already held boxed (a receiver a guard refined): their
    /// boxed value, for `boxed` to reuse rather than box again.
    boxed_as: BTreeMap<mir::Value, mir::Value>,
    /// Each constructed layout's fields and claims, registered in the
    /// module (`ctor_layout`); `None` where the table cannot hold them.
    ctor_layouts: BTreeMap<u32, Option<std::rc::Rc<Vec<(mir::entity::AtomId, MType)>>>>,
    /// The state `this` (a `Ctor`) is in at every return seen so far
    /// (`Callee::this_out`): `None` before the first, `Some(None)` once
    /// two disagree or one is anything else.
    this_out: Option<Option<(u32, FieldSet, bool)>>,
    slot_cls: BTreeMap<usize, u32>,
    /// Values that are the frame's `this` (`FunctionThis`'s results),
    /// and the frame slots a write carries that to (`.this`).
    this_vals: std::collections::BTreeSet<mir::Value>,
    this_slots: std::collections::BTreeSet<usize>,
}

impl<'s, 'a> Run<'s, 'a> {
    fn op(&mut self, op: JSOp) -> R<()> {
        let pc = self.pc;
        self.next_pc = Some(pc + op.len());
        self.ta_poly_site = false;
        self.store_mask = None;
        let mut p = self.s.imms(pc);
        let family = dispatch::op_family(op);
        let built = match family {
            dispatch::OpFamily::Stack => self.op_stack(op, pc, &mut p)?,
            dispatch::OpFamily::Arith => self.op_arith(op, pc)?,
            dispatch::OpFamily::Control => self.op_control(op, pc, &mut p)?,
            dispatch::OpFamily::Names => self.op_names(op, pc, &mut p)?,
            dispatch::OpFamily::Elems => self.op_elems(op, pc, &mut p)?,
            dispatch::OpFamily::Objects => self.op_objects(op, pc, &mut p)?,
            dispatch::OpFamily::Misc => self.op_misc(op, pc, &mut p)?,
            dispatch::OpFamily::Super => self.op_super(op, pc, &mut p)?,
            dispatch::OpFamily::Gen => self.op_gen(op, pc, &mut p)?,
            dispatch::OpFamily::Calls => self.op_calls(op, pc, &mut p)?,
            dispatch::OpFamily::Unsupported => return Err(format!("{op:?}")),
        };
        if built {
            Ok(())
        } else {
            Err(format!(
                "BUG: {op:?} is classified {family:?} but that family does not build it"
            ))
        }
    }

    fn js_binop(&mut self, k: JsBinop, a: Slot, b: Slot) {
        let (x, y) = (self.boxed(a), self.boxed(b));
        // A BigInt result needs both operands BigInt after ToNumeric; an
        // operand that is no BigInt and no object (whose valueOf might
        // give one) makes it a Number, or a TypeError for mixing.
        let big_free = |s: Slot| {
            s.ty.tags()
                .intersect(TagSet::prims(PRIM_BIGINT).union(TagSet::OBJECT))
                .is_empty()
        };
        let no_big = big_free(a) || big_free(b);
        let tags = match k {
            JsBinop::Ursh => TagSet::NUMBER,
            JsBinop::BitAnd | JsBinop::BitOr | JsBinop::BitXor | JsBinop::Lsh | JsBinop::Rsh
                if no_big =>
            {
                TagSet::INT32
            }
            JsBinop::BitAnd | JsBinop::BitOr | JsBinop::BitXor | JsBinop::Lsh | JsBinop::Rsh => {
                TagSet::prims(PRIM_INT32 | PRIM_BIGINT)
            }
            _ if no_big => TagSet::NUMBER,
            _ => TagSet::prims(crate::opsem::NUM | PRIM_BIGINT),
        };
        let r = self.prim_or_js(Opcode::JsBinop(k), vec![x, y], MType::val(tags));
        if tags == TagSet::INT32 {
            // Always an int32: unboxed, the guard never fails.
            let i = self.guard(Opcode::GuardUnbox(UnboxKind::I32), vec![r], MType::I32_TOP);
            self.push(i, Ty::I32);
        } else {
            self.push(r, Ty::Val(tags));
        }
    }

    fn js_unop(&mut self, u: JsUnop, a: Slot) {
        let x = self.boxed(a);
        let tags = match u {
            JsUnop::Pos => TagSet::NUMBER,
            _ => TagSet::prims(crate::opsem::NUM | PRIM_BIGINT),
        };
        let r = self.prim_or_js(Opcode::JsUnop(u), vec![x], MType::val(tags));
        self.push(r, Ty::Val(tags));
    }
}
