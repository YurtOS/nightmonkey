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
mod elems;
mod entry;
mod exits;
mod flow;
mod guards;
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
        use JSOp::*;
        let pc = self.pc;
        self.next_pc = Some(pc + op.len());
        self.ta_poly_site = false;
        self.store_mask = None;
        let mut p = self.s.imms(pc);
        let int_ty = Ty::I32;
        match op {
            // (`DebugLeaveLexicalEnv` only matters to a debugger; so does
            // `Debugger`, a no-op with none attached, as in baseline.)
            Nop | Lineno | JumpTarget | LoopHead | NopDestructuring | NopIsAssignOp
            | DebugLeaveLexicalEnv | Debugger => {}
            // A try block's code is ordinary code: a throw in it exits
            // (`exit.throw`) and baseline takes the pc's handler. The catch
            // code, entered only by a throw, is never reached here.
            Try | TryDestructuring => {}
            // A finally block's start: a marker. MIR runs only its normal
            // entry (a throw exits, and baseline's unwind enters it
            // throwing), whose rethrow arm is therefore baseline's too.
            Finally => {}
            ThrowWithStack => {
                let b = self.exit_block(false);
                self.term(Opcode::Jump, vec![], vec![Self::goto(b)]);
            }

            Undefined => {
                let v = self.const_val(ConstVal::Undefined);
                self.push(v, Ty::Val(TagSet::prims(PRIM_UNDEFINED)));
            }
            Null => {
                let v = self.const_val(ConstVal::Null);
                self.push(v, Ty::Val(TagSet::prims(PRIM_NULL)));
            }
            True | False => {
                let v = self.inst(Opcode::ConstBool(op == True), vec![], Some(MType::Bool));
                self.push(v, Ty::Bool);
            }
            Zero | One | Int8 | Uint16 | Uint24 | Int32 => {
                let n: i32 = match op {
                    Zero => 0,
                    One => 1,
                    Int8 => p.next_int8().unwrap().into(),
                    Uint16 => p.next_uint16().unwrap().into(),
                    Uint24 => p.next_uint24().unwrap() as i32,
                    _ => p.next_int32().unwrap(),
                };
                let v = self.const_i32(n);
                self.push(v, int_ty);
            }
            Double => {
                let x = f64::from_bits(p.next_uint64().unwrap());
                let v = self.const_f64(x);
                self.push(v, Ty::F64);
            }
            Uninitialized => {
                let v = self.const_val(ConstVal::Uninitialized);
                self.push(v, Ty::Val(TagSet::MAGIC));
            }
            Void => {
                self.pop();
                let v = self.const_val(ConstVal::Undefined);
                self.push(v, Ty::Val(TagSet::prims(PRIM_UNDEFINED)));
            }

            // --- frame ---
            GetLocal => {
                let n = p.next_uint24().unwrap();
                let ix = self.local_ix(n);
                let x = self.st[ix];
                // A read of the elided arguments object's local: an exit
                // before the op consuming it makes the object for it too.
                if self.args_locals.contains(&ix) {
                    self.args_copies.insert(x.v);
                }
                if let Some(&c) = self.slot_cls.get(&ix) {
                    self.val_cls.insert(x.v, c);
                }
                if self.this_slots.contains(&ix) {
                    self.this_vals.insert(x.v);
                }
                self.st.push(x);
            }
            SetLocal | InitLexical => {
                let n = p.next_uint24().unwrap();
                let ix = self.local_ix(n);
                let x = self.top();
                self.set_frame_slot(ix, x);
            }
            // With an unmapped `arguments` (or none to map), a formal's
            // frame slot is the formal.
            // A mapped `arguments` object aliases the formals: they are
            // its (baseline's reads and writes too).
            GetArg if self.mapped() => {
                let n = u32::from(p.next_uint16().unwrap());
                let v = self.inst(Opcode::ArgsMapped(n), vec![], Some(MType::VAL_TOP));
                self.push(v, Ty::Val(TagSet::ALL));
            }
            GetArg | GetFrameArg => {
                let n = u32::from(p.next_uint16().unwrap());
                let ix = self.arg_ix(n);
                let x = self.st[ix];
                if let Some(&c) = self.slot_cls.get(&ix) {
                    self.val_cls.insert(x.v, c);
                }
                self.st.push(x);
            }
            ToString => {
                // A string is its own ToString.
                let v = self.top();
                if !v.ty.tags().is_nonempty_subset_of(TagSet::STRING) {
                    let v = self.pop();
                    let x = self.boxed(v);
                    let r = self.js(
                        Opcode::JsRt(RtOp::ToString),
                        vec![x],
                        MType::val(TagSet::STRING),
                    );
                    self.push(r, Ty::Val(TagSet::STRING));
                }
            }
            BuiltinObject => {
                let kind = u32::from(p.next_uint8().unwrap());
                let r = self.js(
                    Opcode::JsRt(RtOp::BuiltinObject(kind)),
                    vec![],
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            Symbol => {
                let code = u32::from(p.next_uint8().unwrap());
                let sym = TagSet::prims(crate::opsem::PRIM_SYMBOL);
                let r = self.js(Opcode::JsRt(RtOp::Symbol(code)), vec![], MType::val(sym));
                self.push(r, Ty::Val(sym));
            }
            GetIntrinsic => {
                // A realm constant, from its cell once armed.
                let a = self.atom(p.next_uint32().unwrap())?;
                let r = self.js(Opcode::JsRt(RtOp::Intrinsic(a)), vec![], MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
            }
            RegExp => {
                let idx = p.next_uint32().unwrap();
                let r = self.js(
                    Opcode::JsRt(RtOp::RegExp(idx)),
                    vec![],
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            InitPropGetter | InitHiddenPropGetter | InitPropSetter | InitHiddenPropSetter => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let kind = u32::from(matches!(op, InitPropSetter | InitHiddenPropSetter))
                    | (u32::from(matches!(op, InitHiddenPropGetter | InitHiddenPropSetter)) << 1);
                let f = self.pop();
                let o = self.top();
                let (x, y) = (self.boxed(o), self.boxed(f));
                self.js_void(Opcode::JsRt(RtOp::InitPropGetSet(a, kind)), vec![x, y]);
            }
            SetArg if self.mapped() => {
                let n = u32::from(p.next_uint16().unwrap());
                let x = self.top();
                let v = self.boxed(x);
                self.inst(Opcode::ArgsMappedSet(n), vec![v], None);
            }
            SetArg => {
                let n = u32::from(p.next_uint16().unwrap());
                let ix = self.arg_ix(n);
                let x = self.top();
                self.set_frame_slot(ix, x);
            }
            GetRval => {
                let x = self.st[self.rval_ix()];
                self.st.push(x);
            }
            SetRval => {
                let x = self.pop();
                let ix = self.rval_ix();
                self.set_frame_slot(ix, x);
            }
            GetAliasedVar | GetAliasedDebugVar => {
                let hops = p.next_uint16().unwrap();
                let slot = p.next_uint24().unwrap();
                let e = self.env_at(hops);
                let v = self.inst(
                    Opcode::EnvLoad(crate::ids::EnvSlot::new(slot)),
                    vec![e],
                    Some(MType::VAL_TOP),
                );
                self.push(v, Ty::Val(TagSet::ALL));
                let claim = self.s.ctx.facts.aliased_sites.get(&self.site(pc)).copied();
                self.guard_result(claim.unwrap_or_default(), pc + op.len(), true);
            }
            SetAliasedVar | InitAliasedLexical => {
                let hops = p.next_uint16().unwrap();
                let slot = p.next_uint24().unwrap();
                let x = self.top();
                let v = self.boxed(x);
                let e = self.env_at(hops);
                self.inst(
                    Opcode::EnvStore(crate::ids::EnvSlot::new(slot)),
                    vec![e, v],
                    None,
                );
            }
            Lambda => {
                let index = p.next_uint32().unwrap();
                let e = self.env_at(0);
                let r =
                    self.js_static(Opcode::JsLambda(index), vec![e], MType::val(TagSet::OBJECT));
                let ty = match self.s.script.gcthings.get(index as usize) {
                    Some(&gc) if !gc.is_other() => {
                        match self.s.ctx.source.object(gc) {
                            // The function gcthing names its script.
                            crate::source::SourceObject::Object(o) => match o.script {
                                Some(k) => Ty::Fn(ScriptId::new(k.id())),
                                None => Ty::Val(TagSet::OBJECT),
                            },
                            crate::source::SourceObject::Script(_) => {
                                Ty::Fn(ScriptId::new(gc.id()))
                            }
                            _ => Ty::Val(TagSet::OBJECT),
                        }
                    }
                    _ => Ty::Val(TagSet::OBJECT),
                };
                self.push(r, ty);
            }
            CheckLexical | CheckAliasedLexical => {
                // The top may be the TDZ sentinel only if its type admits
                // magic; then guard it away (baseline throws on failure).
                let x = self.top();
                if let Ty::Val(t) = x.ty {
                    if t.magic {
                        let js = TagSet { magic: false, ..t };
                        if js.is_empty() {
                            // Always uninitialized: baseline throws the
                            // ReferenceError at this pc.
                            let b = self.exit_block(false);
                            self.term(Opcode::Jump, vec![], vec![Self::goto(b)]);
                            return Ok(());
                        }
                        let v = self.guard(Opcode::GuardTags(js), vec![x.v], MType::val(js));
                        self.st.pop();
                        self.push(v, Ty::Val(js));
                    }
                }
            }

            // --- stack ---
            Pop => {
                self.pop();
            }
            PopN => {
                let n = p.next_uint16().unwrap();
                for _ in 0..n {
                    self.pop();
                }
            }
            Dup if self.string_switch()? => {}
            Dup => {
                let x = self.top();
                self.st.push(x);
            }
            Dup2 => {
                let n = self.st.len();
                let (a, b) = (self.st[n - 2], self.st[n - 1]);
                self.st.push(a);
                self.st.push(b);
            }
            DupAt => {
                let n = p.next_uint24().unwrap() as usize;
                let x = self.st[self.st.len() - 1 - n];
                self.st.push(x);
            }
            Swap => {
                let n = self.st.len();
                self.st.swap(n - 1, n - 2);
            }
            Pick => {
                let n = usize::from(p.next_uint8().unwrap());
                let ix = self.st.len() - 1 - n;
                let x = self.st.remove(ix);
                self.st.push(x);
            }
            Unpick => {
                let n = usize::from(p.next_uint8().unwrap());
                let x = self.pop();
                let ix = self.st.len() - n;
                self.st.insert(ix, x);
            }

            // --- arithmetic ---
            Add | Sub | Mul => {
                let b = self.pop();
                let a = self.pop();
                let arith = match op {
                    Add => ArithOp::Add,
                    Sub => ArithOp::Sub,
                    _ => ArithOp::Mul,
                };
                match (a.ty.num(), b.ty.num()) {
                    (Some(Num::I32), Some(Num::I32))
                        if arith != ArithOp::Mul && self.s.wrap_ok.contains(&pc) =>
                    {
                        // Only ToInt32 of the result is ever observed.
                        let (x, y) = (self.as_i32(a), self.as_i32(b));
                        let r = self.inst(Opcode::I32Wrap(arith), vec![x, y], Some(MType::I32_TOP));
                        self.push(r, Ty::I32);
                    }
                    (Some(Num::I32), Some(Num::I32)) => {
                        let (x, y) = (self.as_i32(a), self.as_i32(b));
                        let r = self.int_arith(arith, x, y);
                        self.push(r, Ty::I32);
                    }
                    (Some(_), Some(_)) => {
                        let (x, y) = (self.as_f64(a), self.as_f64(b));
                        let fop = match arith {
                            ArithOp::Add => F64Op::Add,
                            ArithOp::Sub => F64Op::Sub,
                            ArithOp::Mul => F64Op::Mul,
                            ArithOp::Rem => unreachable!("`%` has its own arm"),
                        };
                        let r = self.inst(Opcode::F64Arith(fop), vec![x, y], Some(MType::F64_TOP));
                        self.push(r, Ty::F64);
                    }
                    _ => {
                        let (x, y) = (self.boxed(a), self.boxed(b));
                        // Neither operand a BigInt or an object (whose
                        // valueOf might give one): a Number, or a TypeError.
                        // For `+`, neither a string or an object either (no
                        // concatenation) for a numeric result.
                        let free = |s: Slot, t: TagSet| s.ty.tags().intersect(t).is_empty();
                        let big = TagSet::prims(PRIM_BIGINT).union(TagSet::OBJECT);
                        let no_big = free(a, big) || free(b, big);
                        let strish = TagSet::STRING.union(TagSet::OBJECT);
                        let numeric = TagSet::prims(crate::opsem::NUM | PRIM_BIGINT);
                        let arith = if no_big { TagSet::NUMBER } else { numeric };
                        let (jop, tags) = match op {
                            Add if free(a, strish) && free(b, strish) => (Opcode::JsAdd, arith),
                            Add => (Opcode::JsAdd, numeric.union(TagSet::STRING)),
                            Sub => (Opcode::JsBinop(JsBinop::Sub), arith),
                            _ => (Opcode::JsBinop(JsBinop::Mul), arith),
                        };
                        let r = self.prim_or_js(jop, vec![x, y], MType::val(tags));
                        self.push(r, Ty::Val(tags));
                    }
                }
            }
            Div => {
                let b = self.pop();
                let a = self.pop();
                if a.ty.num().is_some() && b.ty.num().is_some() {
                    let (x, y) = (self.as_f64(a), self.as_f64(b));
                    let r = self.inst(
                        Opcode::F64Arith(F64Op::Div),
                        vec![x, y],
                        Some(MType::F64_TOP),
                    );
                    self.push(r, Ty::F64);
                } else {
                    self.js_binop(JsBinop::Div, a, b);
                }
            }
            Mod => {
                let b = self.pop();
                let a = self.pop();
                match (a.ty.num(), b.ty.num()) {
                    (Some(Num::I32), Some(Num::I32)) => {
                        let (x, y) = (self.as_i32(a), self.as_i32(b));
                        let r = self.int_arith(ArithOp::Rem, x, y);
                        self.push(r, Ty::I32);
                    }
                    (Some(_), Some(_)) => {
                        let (x, y) = (self.as_f64(a), self.as_f64(b));
                        let r = self.inst(
                            Opcode::F64Arith(F64Op::Mod),
                            vec![x, y],
                            Some(MType::F64_TOP),
                        );
                        self.push(r, Ty::F64);
                    }
                    _ => self.js_binop(JsBinop::Mod, a, b),
                }
            }
            Pow => {
                let b = self.pop();
                let a = self.pop();
                self.js_binop(JsBinop::Pow, a, b);
            }
            BitAnd | BitOr | BitXor | Lsh | Rsh => {
                let b = self.pop();
                let a = self.pop();
                if a.ty.num().is_some() && b.ty.num().is_some() {
                    let (x, y) = (self.to_int32(a), self.to_int32(b));
                    let bop = match op {
                        BitAnd => BitOp::And,
                        BitOr => BitOp::Or,
                        BitXor => BitOp::Xor,
                        Lsh => BitOp::Shl,
                        _ => BitOp::Shr,
                    };
                    let t = self.sig_result(&Opcode::I32Bit(bop), &[x, y]);
                    let r = self.inst(Opcode::I32Bit(bop), vec![x, y], Some(t));
                    self.push(r, Ty::I32);
                } else {
                    let k = match op {
                        BitAnd => JsBinop::BitAnd,
                        BitOr => JsBinop::BitOr,
                        BitXor => JsBinop::BitXor,
                        Lsh => JsBinop::Lsh,
                        _ => JsBinop::Rsh,
                    };
                    self.js_binop(k, a, b);
                }
            }
            Ursh => {
                let b = self.pop();
                let a = self.pop();
                if a.ty.num().is_some() && b.ty.num().is_some() {
                    let (x, y) = (self.to_int32(a), self.to_int32(b));
                    let t = MType::int_range(0, u32::MAX.into());
                    let r = self.inst(Opcode::I32Ushr, vec![x, y], Some(t));
                    let f = self.inst(Opcode::IntToF64, vec![r], Some(MType::F64_TOP));
                    self.push(f, Ty::F64);
                } else {
                    self.js_binop(JsBinop::Ursh, a, b);
                }
            }
            Inc | Dec => {
                let a = self.pop();
                match a.ty.num() {
                    Some(Num::I32) => {
                        let x = self.as_i32(a);
                        let one = self.const_i32(1);
                        let k = if op == Inc {
                            ArithOp::Add
                        } else {
                            ArithOp::Sub
                        };
                        let r = self.guard(Opcode::I32Ovf(k), vec![x, one], MType::I32_TOP);
                        self.push(r, Ty::I32);
                    }
                    Some(Num::F64) => {
                        let x = self.as_f64(a);
                        let one = self.const_f64(1.0);
                        let k = if op == Inc { F64Op::Add } else { F64Op::Sub };
                        let r = self.inst(Opcode::F64Arith(k), vec![x, one], Some(MType::F64_TOP));
                        self.push(r, Ty::F64);
                    }
                    None => {
                        let u = if op == Inc { JsUnop::Inc } else { JsUnop::Dec };
                        self.js_unop(u, a);
                    }
                }
            }
            Neg => {
                let a = self.pop();
                match a.ty.num() {
                    // -x as x * -1: fails on INT32_MIN and on 0 (-0).
                    Some(Num::I32) => {
                        let x = self.as_i32(a);
                        let m1 = self.const_i32(-1);
                        let r =
                            self.guard(Opcode::I32Ovf(ArithOp::Mul), vec![x, m1], MType::I32_TOP);
                        self.push(r, Ty::I32);
                    }
                    Some(Num::F64) => {
                        let x = self.as_f64(a);
                        let r = self.inst(Opcode::F64Neg, vec![x], Some(MType::F64_TOP));
                        self.push(r, Ty::F64);
                    }
                    None => self.js_unop(JsUnop::Neg, a),
                }
            }
            BitNot => {
                let a = self.pop();
                if a.ty.num().is_some() {
                    let x = self.to_int32(a);
                    let m1 = self.const_i32(-1);
                    let r = self.inst(
                        Opcode::I32Bit(BitOp::Xor),
                        vec![x, m1],
                        Some(MType::I32_TOP),
                    );
                    self.push(r, Ty::I32);
                } else {
                    self.js_unop(JsUnop::BitNot, a);
                }
            }
            Pos | ToNumeric => {
                let a = self.top();
                if a.ty.num().is_none() {
                    self.pop();
                    if op == Pos {
                        self.js_unop(JsUnop::Pos, a);
                    } else {
                        let tags = TagSet::prims(crate::opsem::NUM | PRIM_BIGINT);
                        let x = self.boxed(a);
                        let r = self.prim_or_js(Opcode::JsToNumeric, vec![x], MType::val(tags));
                        self.push(r, Ty::Val(tags));
                    }
                }
            }

            // --- compares ---
            Lt | Le | Gt | Ge | Eq | Ne | StrictEq | StrictNe => {
                let b = self.pop();
                let a = self.pop();
                let (cc, jcc) = match op {
                    Lt => (Cc::Lt, JsCc::Lt),
                    Le => (Cc::Le, JsCc::Le),
                    Gt => (Cc::Gt, JsCc::Gt),
                    Ge => (Cc::Ge, JsCc::Ge),
                    Eq => (Cc::Eq, JsCc::Eq),
                    Ne => (Cc::Ne, JsCc::Ne),
                    StrictEq => (Cc::Eq, JsCc::StrictEq),
                    _ => (Cc::Ne, JsCc::StrictNe),
                };
                // Against a null or undefined constant: tag tests, which
                // run no user code (no fence).
                let nullish = |t: Ty| {
                    let tags = t.tags();
                    !tags.is_empty() && tags.subset_of(TagSet::prims(PRIM_NULL | PRIM_UNDEFINED))
                };
                let equality = matches!(op, Eq | Ne | StrictEq | StrictNe);
                let konst = if equality && nullish(b.ty) {
                    Some((a, b))
                } else if equality && nullish(a.ty) {
                    Some((b, a))
                } else {
                    None
                };
                if let Some((x, k)) = konst {
                    let negate = matches!(op, Ne | StrictNe);
                    let exact = if matches!(op, StrictEq | StrictNe) {
                        Some(k.ty.tags())
                    } else if x.ty.tags().intersect(TagSet::OBJECT).is_empty() {
                        Some(TagSet::prims(PRIM_NULL | PRIM_UNDEFINED))
                    } else {
                        None
                    };
                    if exact.is_some_and(|t| self.fuse_test(x, t, negate)) {
                        return Ok(());
                    }
                    let r = if matches!(op, StrictEq | StrictNe) {
                        let v = self.boxed(x);
                        self.tag_test(v, k.ty.tags())
                    } else {
                        self.loose_nullish(x)
                    };
                    let r = if matches!(op, Ne | StrictNe) {
                        self.not(r)
                    } else {
                        r
                    };
                    self.push(r, Ty::Bool);
                    return Ok(());
                }
                let r = match (a.ty.num(), b.ty.num()) {
                    (Some(Num::I32), Some(Num::I32)) => {
                        let (x, y) = (self.as_i32(a), self.as_i32(b));
                        self.inst(Opcode::Cmp(NumRepr::I32, cc), vec![x, y], Some(MType::Bool))
                    }
                    (Some(_), Some(_)) => {
                        let (x, y) = (self.as_f64(a), self.as_f64(b));
                        self.inst(Opcode::Cmp(NumRepr::F64, cc), vec![x, y], Some(MType::Bool))
                    }
                    _ => {
                        let (x, y) = (self.boxed(a), self.boxed(b));
                        self.prim_or_js(Opcode::JsCompare(jcc), vec![x, y], MType::Bool)
                    }
                };
                self.push(r, Ty::Bool);
            }
            Not => {
                let a = self.pop();
                let t = self.truthy(a);
                let r = self.not(t);
                self.push(r, Ty::Bool);
            }

            // --- control ---
            Goto => {
                let off = p.next_int32().unwrap();
                self.jump_to(pc.branch(off), Some(pc));
            }
            JumpIfFalse | JumpIfTrue => {
                let off = p.next_int32().unwrap();
                let c = self.pop();
                let t = self.truthy(c);
                let (target, next) = (pc.branch(off), pc + op.len());
                if op == JumpIfTrue {
                    self.branch(t, target, next);
                } else {
                    self.branch(t, next, target);
                }
            }
            And | Or => {
                let off = p.next_int32().unwrap();
                let c = self.top();
                let t = self.truthy(c);
                let (target, next) = (pc.branch(off), pc + op.len());
                if op == Or {
                    self.branch(t, target, next);
                } else {
                    self.branch(t, next, target);
                }
            }
            Return => {
                let x = self.pop();
                let v = self.boxed(x);
                self.note_this_out();
                self.ctor_stamp();
                self.return_restamps();
                self.term(Opcode::Return, vec![v], vec![]);
            }
            RetRval => {
                let x = self.st[self.rval_ix()];
                let v = self.boxed(x);
                self.note_this_out();
                self.ctor_stamp();
                self.return_restamps();
                self.term(Opcode::Return, vec![v], vec![]);
            }

            FunctionThis => {
                let x = self.st[0];
                if self.s.script.strict || x.ty.tags().is_nonempty_subset_of(TagSet::OBJECT) {
                    self.st.push(x);
                } else {
                    // Sloppy: an object `this` is itself; anything else is
                    // boxed (null and undefined become the global `this`).
                    self.demote_for_generic_arm(1);
                    let obj = TagSet::OBJECT;
                    let (t, e, j) = (self.new_block(), self.new_block(), self.new_block());
                    let p = self.f.add_param(t, MType::val(obj));
                    let r = self.f.add_param(j, MType::val(obj));
                    let v = self.boxed(x);
                    self.term(
                        Opcode::GuardTags(obj),
                        vec![v],
                        vec![
                            Edge {
                                block: t,
                                args: vec![EdgeArg::Out(0)],
                            },
                            Self::goto(e),
                        ],
                    );
                    self.at(t);
                    self.term(
                        Opcode::Jump,
                        vec![],
                        vec![Edge {
                            block: j,
                            args: vec![EdgeArg::Value(p)],
                        }],
                    );
                    self.at(e);
                    let b = self.js_static(Opcode::JsBoxThis, vec![v], MType::val(obj));
                    self.term(
                        Opcode::Jump,
                        vec![],
                        vec![Edge {
                            block: j,
                            args: vec![EdgeArg::Value(b)],
                        }],
                    );
                    self.at(j);
                    self.push(r, Ty::Val(obj));
                }
                self.guard_this_layout();
            }
            String => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let s = self.inst(
                    Opcode::ConstStr(a),
                    vec![],
                    Some(MType::Str(mir::types::StrInfo { atom: Some(a) })),
                );
                let x = Slot {
                    v: s,
                    ty: Ty::Val(TagSet::STRING),
                };
                let t = mir::ops::box_type(&self.f.ty(s)).expect("strings box");
                let v = self.inst(Opcode::Box, vec![x.v], Some(t));
                let v = self.weaken(v, MType::val(TagSet::STRING));
                self.push(v, Ty::Val(TagSet::STRING));
            }
            BindUnqualifiedGName => {
                let index = p.next_uint32().unwrap();
                let a = self.atom(index)?;
                if let Some(&slot) = self.s.syn_bids.get(&index) {
                    // The target of an assignment to a syntactic global:
                    // while the binding is the global's writable data
                    // property (the check the store makes, which then
                    // folds into this one), the global object; else exit
                    // here.
                    let b = self.binding(a, slot);
                    self.guard(
                        Opcode::CheckBinding(b, BindingCheck::Write),
                        vec![],
                        MType::Fact(mir::types::FactKind::Binding(b)),
                    );
                    let r = self.inst(
                        Opcode::GlobalObject,
                        vec![],
                        Some(MType::val(TagSet::OBJECT)),
                    );
                    self.push(r, Ty::Val(TagSet::OBJECT));
                    return Ok(());
                }
                let r = self.js(Opcode::JsBindGName(a), vec![], MType::val(TagSet::OBJECT));
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            SetGName | StrictSetGName => {
                let index = p.next_uint32().unwrap();
                let a = self.atom(index)?;
                let v = self.pop();
                let env = self.pop();
                let (e, y) = (self.boxed(env), self.boxed(v));
                match self.s.syn_bids.get(&index) {
                    Some(&slot) => {
                        // §3: `check.binding.write`, then a leaf store of
                        // the slot; a miss exits here.
                        let b = self.binding(a, slot);
                        let fact = self.guard(
                            Opcode::CheckBinding(b, BindingCheck::Write),
                            vec![],
                            MType::Fact(mir::types::FactKind::Binding(b)),
                        );
                        let (t, _) = self.f.add_inst(
                            self.cur,
                            Opcode::StoreGName(b),
                            vec![fact, y],
                            &[],
                            vec![],
                        );
                        self.f.witnesses[t] = Some(mir::func::Witness {
                            may_kill: mir::types::KillPattern::of(mir::ops::store_gname_kills(
                                b, &self.mm,
                            )),
                        });
                    }
                    _ => {
                        self.js_void_keep(Opcode::JsSetName(a, op == StrictSetGName), vec![e, y], v)
                    }
                }
                self.repush(v);
            }
            TableSwitch => {
                let default_off = p.next_int32().unwrap();
                let low = p.next_int32().unwrap();
                let high = p.next_int32().unwrap();
                let first = p.next_uint24().unwrap() as usize;
                let x = self.pop();
                let n = usize::try_from((i64::from(high) - i64::from(low) + 1).max(0)).unwrap();
                let default = pc.branch(default_off);
                let mut targets = vec![];
                for k in 0..n {
                    let t = *self
                        .s
                        .script
                        .resume_offsets
                        .get(first + k)
                        .ok_or("TableSwitch resume index out of range")?;
                    targets.push(t);
                }
                // An int32, or a double that is exactly one, selects a case;
                // anything else takes the default.
                let i = match x.ty {
                    Ty::I32 => x.v,
                    Ty::F64 | Ty::Val(_) | Ty::Fn(_) => {
                        let f = match x.ty {
                            Ty::F64 => x.v,
                            _ => {
                                let d = self.new_block();
                                let o = self.f.add_param(d, MType::F64_TOP);
                                let Some(de) = self.edge_to(default, Some(pc)) else {
                                    self.term(Opcode::Unreachable, vec![], vec![]);
                                    return Ok(());
                                };
                                self.term(
                                    Opcode::GuardUnbox(UnboxKind::F64Num),
                                    vec![x.v],
                                    vec![
                                        Edge {
                                            block: d,
                                            args: vec![EdgeArg::Out(0)],
                                        },
                                        de,
                                    ],
                                );
                                self.at(d);
                                o
                            }
                        };
                        // -0 selects case 0 as +0 does: `x + 0` is +0 for
                        // either zero and `x` otherwise.
                        let z = self.const_f64(0.0);
                        let f = self.inst(
                            Opcode::F64Arith(F64Op::Add),
                            vec![f, z],
                            Some(MType::F64_TOP),
                        );
                        let d = self.new_block();
                        let o = self.f.add_param(d, MType::I32_TOP);
                        let Some(de) = self.edge_to(default, Some(pc)) else {
                            self.term(Opcode::Unreachable, vec![], vec![]);
                            return Ok(());
                        };
                        self.term(
                            Opcode::F64ToIntExact,
                            vec![f],
                            vec![
                                Edge {
                                    block: d,
                                    args: vec![EdgeArg::Out(0)],
                                },
                                de,
                            ],
                        );
                        self.at(d);
                        o
                    }
                    Ty::Bool
                    | Ty::Obj(..)
                    | Ty::ObjHint(_)
                    | Ty::Ctor(..)
                    | Ty::Native
                    | Ty::Ta(_) => {
                        self.jump_to(default, Some(pc));
                        return Ok(());
                    }
                    Ty::Dead => unreachable!("a dead slot is never read"),
                };
                let lo = self.const_i32(low);
                let idx = self.inst(
                    Opcode::I32Wrap(ArithOp::Sub),
                    vec![i, lo],
                    Some(MType::I32_TOP),
                );
                let mut edges = vec![];
                for t in targets.into_iter().chain([default]) {
                    match self.edge_to(t, Some(pc)) {
                        Some(e) => edges.push(e),
                        None => {
                            self.term(Opcode::Unreachable, vec![], vec![]);
                            return Ok(());
                        }
                    }
                }
                self.term(Opcode::Switch(u32::try_from(n).unwrap()), vec![idx], edges);
            }
            Case => {
                // [lval, cond]: true pops both and jumps; false keeps lval.
                let off = p.next_int32().unwrap();
                let c = self.pop();
                let t = self.truthy(c);
                let (target, next) = (pc.branch(off), pc + op.len());
                let ne = self.edge_to(next, Some(pc));
                let lval = self.pop();
                let te = self.edge_to(target, Some(pc));
                self.st.push(lval);
                match (te, ne) {
                    (Some(te), Some(ne)) => self.term(Opcode::Br, vec![t], vec![te, ne]),
                    _ => self.term(Opcode::Unreachable, vec![], vec![]),
                }
            }
            Default => {
                let off = p.next_int32().unwrap();
                self.pop();
                self.jump_to(pc.branch(off), Some(pc));
            }
            StrictConstantEq | StrictConstantNe => {
                let operand = p.next_uint16().unwrap();
                let a = self.pop();
                // Against undefined or null: a tag test.
                let exact = match (operand >> 8) & 0xFF {
                    3 => Some(TagSet::prims(PRIM_UNDEFINED)),
                    4 => Some(TagSet::prims(PRIM_NULL)),
                    _ => None,
                };
                if exact.is_some_and(|t| self.fuse_test(a, t, op == StrictConstantNe)) {
                    return Ok(());
                }
                let x = self.boxed(a);
                let mut r = self.inst(
                    Opcode::JsConstantStrictEq(operand),
                    vec![x],
                    Some(MType::Bool),
                );
                if op == StrictConstantNe {
                    r = self.not(r);
                }
                self.push(r, Ty::Bool);
            }
            IsNullOrUndefined => {
                // [v] -> [v, v is null or undefined], decided by the tags
                // when they say, else by a tag test merging both ways.
                let a = self.top();
                let nullish = TagSet::prims(PRIM_NULL | PRIM_UNDEFINED);
                let tags = a.ty.tags();
                let r = if tags.subset_of(nullish) {
                    self.inst(Opcode::ConstBool(true), vec![], Some(MType::Bool))
                } else if tags.intersect(nullish).is_empty() {
                    self.inst(Opcode::ConstBool(false), vec![], Some(MType::Bool))
                } else {
                    if self.fuse_test(a, nullish, false) {
                        return Ok(());
                    }
                    let x = self.boxed(a);
                    self.tag_test(x, nullish)
                };
                self.push(r, Ty::Bool);
            }
            TypeofEq => {
                let operand = p.next_uint8().unwrap();
                let a = self.pop();
                // A type whose values are exactly some tags (no object is
                // one): a tag test.
                let exact = match operand & 0x0f {
                    3 => Some(TagSet::STRING),
                    4 => Some(TagSet::NUMBER),
                    5 => Some(TagSet::BOOLEAN),
                    6 => Some(TagSet::prims(crate::opsem::PRIM_SYMBOL)),
                    7 => Some(TagSet::prims(PRIM_BIGINT)),
                    _ => None,
                };
                if exact.is_some_and(|t| self.fuse_test(a, t, operand & 0x80 != 0)) {
                    return Ok(());
                }
                let x = self.boxed(a);
                let r = self.inst(Opcode::JsTypeofEq(operand), vec![x], Some(MType::Bool));
                self.push(r, Ty::Bool);
            }

            // --- generic names, properties, elements and calls ---
            GetGName => {
                let index = p.next_uint32().unwrap();
                if self.s.next_is_typeof(pc, op) {
                    // `typeof name`: an unbound global reads as undefined.
                    let a = self.atom(index)?;
                    let r = self.js(Opcode::JsRt(RtOp::GetNameTypeof(a)), vec![], MType::VAL_TOP);
                    self.push(r, Ty::Val(TagSet::ALL));
                    return Ok(());
                }
                let a = self.atom(index)?;
                if let Some(&fg) = self.s.fused.get(&index) {
                    if self.fused_gname(a, fg) {
                        return Ok(());
                    }
                }
                let r = match self.s.syn_bids.get(&index) {
                    Some(&slot) => {
                        let b = self.binding(a, slot);
                        if let Some(k) = self.mm.bindings[b].pred {
                            // A predicted function: while the binding's
                            // value fuse says it holds a compiled function
                            // of script `k`, the read is that function,
                            // typed (a call through it needs no callee
                            // guard); else exit here.
                            let fact = self.guard(
                                Opcode::CheckBinding(b, BindingCheck::Fn),
                                vec![],
                                MType::Fact(mir::types::FactKind::BindingFn(b)),
                            );
                            let t = MType::Val(mir::types::VSet::new(
                                TagSet::OBJECT,
                                mir::types::NumInfo::TOP,
                                ObjInfo::kind(ObjKind::Function(Some(k))),
                                mir::types::StrInfo::TOP,
                            ));
                            let r = self.inst(Opcode::LoadGName(b), vec![fact], Some(t));
                            self.gname_vals.insert(r, a);
                            self.push(r, Ty::Fn(k));
                            return Ok(());
                        }
                        // §3: `check.binding`, then a leaf load of the
                        // slot. A syntactic global is a non-configurable
                        // data property, so the check holds once resolved;
                        // a miss exits here.
                        let fact = self.guard(
                            Opcode::CheckBinding(b, BindingCheck::Read),
                            vec![],
                            MType::Fact(mir::types::FactKind::Binding(b)),
                        );
                        self.inst(Opcode::LoadGName(b), vec![fact], Some(MType::VAL_TOP))
                    }
                    _ => self.js(Opcode::JsGetName(a), vec![], MType::VAL_TOP),
                };
                self.gname_vals.insert(r, a);
                self.push(r, Ty::Val(TagSet::ALL));
                if let Some(&claim) = self.s.gname_types.get(&index) {
                    self.guard_result(claim, pc + op.len(), true);
                }
            }
            GetProp => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let recv = self.pop();
                self.get_prop_op(pc, pc + op.len(), a, recv)?;
            }
            SetProp | StrictSetProp => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let v = self.pop();
                let recv = self.pop();
                self.set_prop_op(pc, pc + op.len(), a, recv, v, op == StrictSetProp);
            }
            GetElem if self.s.args_reads.as_ref().is_some_and(|r| r.contains(&pc)) => {
                // An element of the elided arguments object: an int32 index
                // within the actual count reads that actual; anything else
                // exits here, where the object is made (`exit_operands`)
                // and baseline reads it.
                let key = *self.st.last().unwrap();
                let ki = if key.ty == Ty::I32 {
                    key.v
                } else {
                    let b = self.boxed(key);
                    self.guard(Opcode::GuardUnbox(UnboxKind::I32), vec![b], MType::I32_TOP)
                };
                let n = self.inst(
                    Opcode::ArgsLength,
                    vec![],
                    Some(MType::i32_range(0, i64::from(i32::MAX))),
                );
                let zero = self.const_i32(0);
                let exit = self.exit_block(false);
                let nonneg = self.inst(
                    Opcode::Cmp(NumRepr::I32, Cc::Ge),
                    vec![ki, zero],
                    Some(MType::Bool),
                );
                let (b1, b2) = (self.new_block(), self.new_block());
                self.term(
                    Opcode::Br,
                    vec![nonneg],
                    vec![Self::goto(b1), Self::goto(exit)],
                );
                self.at(b1);
                let below = self.inst(
                    Opcode::Cmp(NumRepr::I32, Cc::Lt),
                    vec![ki, n],
                    Some(MType::Bool),
                );
                self.term(
                    Opcode::Br,
                    vec![below],
                    vec![Self::goto(b2), Self::goto(exit)],
                );
                self.at(b2);
                self.pop();
                self.pop();
                let v = self.inst(Opcode::ActualArg, vec![ki], Some(MType::VAL_TOP));
                self.push(v, Ty::Val(TagSet::ALL));
            }
            GetElem
                if self
                    .st
                    .last()
                    .and_then(|k| self.const_key_atom(*k))
                    .is_some() =>
            {
                // `o["name"]` (a key MIR knows is that string): `o.name`.
                let key = self.pop();
                let a = self.const_key_atom(key).unwrap();
                let recv = self.pop();
                self.get_prop_op(pc, pc + op.len(), a, recv)?;
            }
            GetElem => {
                self.ta_poly_site = self.s.ctx.facts.elem_poly_sites.contains(&self.site(pc));
                let key = self.pop();
                let recv = self.pop();
                let ta = self.ta_elem(pc, recv, key);
                let r = if let Some((o, i, k)) = ta {
                    // The element inline, as a boxed number; out of
                    // bounds, the generic op (a fence unless it keeps, or
                    // a data read).
                    let (ok, generic, join) =
                        (self.new_block(), self.new_block(), self.new_block());
                    let raw = if k.is_float() {
                        MType::F64_TOP
                    } else {
                        MType::I32_TOP
                    };
                    let p = self.f.add_param(ok, raw);
                    let jr = self.f.add_param(join, MType::VAL_TOP);
                    self.term(
                        Opcode::LoadTa,
                        vec![o, i],
                        vec![
                            Edge {
                                block: ok,
                                args: vec![EdgeArg::Out(0)],
                            },
                            Self::goto(generic),
                        ],
                    );
                    self.at(ok);
                    let ty = if k.is_float() { Ty::F64 } else { Ty::I32 };
                    let b = self.boxed(Slot { v: p, ty });
                    let b = self.weaken(b, MType::VAL_TOP);
                    self.term(
                        Opcode::Jump,
                        vec![],
                        vec![Edge {
                            block: join,
                            args: vec![EdgeArg::Value(b)],
                        }],
                    );
                    self.at(generic);
                    let (x, kb) = (self.boxed(recv), self.boxed(key));
                    let r = self.get_elem(x, kb);
                    self.term(
                        Opcode::Jump,
                        vec![],
                        vec![Edge {
                            block: join,
                            args: vec![EdgeArg::Value(r)],
                        }],
                    );
                    self.at(join);
                    Some(jr)
                } else {
                    None
                };
                let r = if let Some(r) = r {
                    r
                } else {
                    // A dense element load's miss (a hole, out of bounds) exits
                    // rather than running the generic read and rejoining, where no
                    // typed array can reach the site (`elem_poly_sites`): a native
                    // receiver is then one with dense elements and a miss is rare.
                    // Where one can, the generic read stays: a typed array the
                    // analysis took for an array misses every time, and an exit per
                    // execution costs far more than the facts the rejoin kills
                    // (pdfjs's FlateStream_getCode: 1.5M exits).
                    match self.native_elem(pc, recv, key) {
                        Some((o, i)) if !self.ta_poly_site => {
                            // A dense element inline; out of bounds or a
                            // hole, an exit (nothing has happened yet): no
                            // generic op rejoins, so what the code after it
                            // knows about memory survives it.
                            self.guard(Opcode::LoadElem, vec![o, i], MType::VAL_TOP)
                        }
                        Some((o, i)) => {
                            // A dense element inline; out of bounds or a
                            // hole, the generic op (a fence unless it keeps,
                            // or a data read).
                            let (ok, generic, join) =
                                (self.new_block(), self.new_block(), self.new_block());
                            let p = self.f.add_param(ok, MType::VAL_TOP);
                            let jr = self.f.add_param(join, MType::VAL_TOP);
                            self.term(
                                Opcode::LoadElem,
                                vec![o, i],
                                vec![
                                    Edge {
                                        block: ok,
                                        args: vec![EdgeArg::Out(0)],
                                    },
                                    Self::goto(generic),
                                ],
                            );
                            self.at(ok);
                            self.term(
                                Opcode::Jump,
                                vec![],
                                vec![Edge {
                                    block: join,
                                    args: vec![EdgeArg::Value(p)],
                                }],
                            );
                            self.at(generic);
                            let (x, k) = (self.boxed(recv), self.boxed(key));
                            let r = self.get_elem(x, k);
                            self.term(
                                Opcode::Jump,
                                vec![],
                                vec![Edge {
                                    block: join,
                                    args: vec![EdgeArg::Value(r)],
                                }],
                            );
                            self.at(join);
                            jr
                        }
                        None => {
                            let (x, k) = (self.boxed(recv), self.boxed(key));
                            self.get_elem(x, k)
                        }
                    }
                };
                self.ta_poly_site = false;
                self.push(r, Ty::Val(TagSet::ALL));
                let claim = self.s.ctx.facts.elem_sites.get(&self.site(pc)).copied();
                self.guard_result(claim.unwrap_or_default(), pc + op.len(), true);
            }
            SetElem | StrictSetElem
                if self.st.len() >= 2
                    && self.const_key_atom(self.st[self.st.len() - 2]).is_some() =>
            {
                // `o["name"] = v`: `o.name = v`.
                let v = self.pop();
                let key = self.pop();
                let a = self.const_key_atom(key).unwrap();
                let recv = self.pop();
                self.set_prop_op(pc, pc + op.len(), a, recv, v, op == StrictSetElem);
            }
            SetElem | StrictSetElem => {
                self.ta_poly_site = self.s.ctx.facts.elem_poly_sites.contains(&self.site(pc));
                let v = self.pop();
                let key = self.pop();
                let recv = self.pop();
                let duty = self.ranges_duty(pc, v);
                // The value as the kind stores it, raw: an int32 for an
                // integer kind (Uint8Clamped clamps it), a number for a
                // float one. A value of unknown type is unboxed at run
                // time; a miss (or a
                // double into an integer kind) takes the generic op.
                let ta = self.ta_elem(pc, recv, key).and_then(|(o, i, k)| {
                    match (k.is_float(), v.ty.num()) {
                        (true, Some(_)) => Some((o, i, k, Some(self.as_f64(v)))),
                        (false, Some(Num::I32)) => Some((o, i, k, Some(self.as_i32(v)))),
                        (_, None) if !matches!(v.ty, Ty::Dead) => Some((o, i, k, None)),
                        _ => None,
                    }
                });
                if let Some((o, i, k, raw)) = ta {
                    let v = self.demote_value_for_generic_arm(v);
                    let (ok, generic, join) =
                        (self.new_block(), self.new_block(), self.new_block());
                    let raw = match raw {
                        Some(r) => r,
                        None => {
                            let (kind, ty) = if k.is_float() {
                                (UnboxKind::F64Num, MType::F64_TOP)
                            } else {
                                (UnboxKind::I32, MType::I32_TOP)
                            };
                            let unboxed = self.new_block();
                            let r = self.f.add_param(unboxed, ty);
                            let y = self.boxed(v);
                            self.term(
                                Opcode::GuardUnbox(kind),
                                vec![y],
                                vec![
                                    Edge {
                                        block: unboxed,
                                        args: vec![EdgeArg::Out(0)],
                                    },
                                    Self::goto(generic),
                                ],
                            );
                            self.at(unboxed);
                            r
                        }
                    };
                    self.term(
                        Opcode::StoreTa,
                        vec![o, i, raw],
                        vec![Self::goto(ok), Self::goto(generic)],
                    );
                    self.at(ok);
                    self.term(Opcode::Jump, vec![], vec![Self::goto(join)]);
                    self.at(generic);
                    let (x, kb, y) = (self.boxed(recv), self.boxed(key), self.boxed(v));
                    self.set_elem(op == StrictSetElem, duty, x, kb, y, v);
                    self.term(Opcode::Jump, vec![], vec![Self::goto(join)]);
                    self.at(join);
                    self.st.push(v);
                } else {
                    match self.native_elem(pc, recv, key) {
                        Some((o, i)) if !self.ta_poly_site => {
                            // A dense overwrite or append inline (the lowering's
                            // append arm); a store it cannot do (growth, a hole
                            // past the prototypes' proof) exits before it.
                            let y = if v.ty == Ty::F64
                                && self
                                    .s
                                    .ctx
                                    .facts
                                    .elem_write_sites
                                    .get(&self.site(pc))
                                    .is_some_and(|m| {
                                        m.prims().intersects(crate::opsem::PRIM_DOUBLE)
                                    }) {
                                let t = MType::val(TagSet::DOUBLE);
                                self.inst(Opcode::BoxDouble, vec![v.v], Some(t))
                            } else {
                                self.boxed(v)
                            };
                            let ok = self.new_block();
                            let fail = self.exit_block(false);
                            self.term(
                                Opcode::StoreElem(duty, true),
                                vec![o, i, y],
                                vec![Self::goto(ok), Self::goto(fail)],
                            );
                            self.at(ok);
                            self.st.push(v);
                        }
                        Some((o, i)) => {
                            // A dense overwrite inline; else the generic op.
                            let v = self.demote_value_for_generic_arm(v);
                            // A double under a write claim that admits Double
                            // goes in as the double it is: every
                            // read of the element admits the tag.
                            let y = if v.ty == Ty::F64
                                && self
                                    .s
                                    .ctx
                                    .facts
                                    .elem_write_sites
                                    .get(&self.site(pc))
                                    .is_some_and(|m| {
                                        m.prims().intersects(crate::opsem::PRIM_DOUBLE)
                                    }) {
                                let t = MType::val(TagSet::DOUBLE);
                                self.inst(Opcode::BoxDouble, vec![v.v], Some(t))
                            } else {
                                self.boxed(v)
                            };
                            let (ok, generic, join) =
                                (self.new_block(), self.new_block(), self.new_block());
                            self.term(
                                Opcode::StoreElem(duty, false),
                                vec![o, i, y],
                                vec![Self::goto(ok), Self::goto(generic)],
                            );
                            self.at(ok);
                            self.term(Opcode::Jump, vec![], vec![Self::goto(join)]);
                            self.at(generic);
                            let (x, k) = (self.boxed(recv), self.boxed(key));
                            self.set_elem(op == StrictSetElem, duty, x, k, y, v);
                            self.term(Opcode::Jump, vec![], vec![Self::goto(join)]);
                            self.at(join);
                            self.st.push(v);
                        }
                        None => {
                            let (x, k, y) = (self.boxed(recv), self.boxed(key), self.boxed(v));
                            self.set_elem(op == StrictSetElem, duty, x, k, y, v);
                            self.repush(v);
                        }
                    }
                }
            }
            // Generic operations through their runtime helpers.
            Instanceof | In | HasOwn => {
                let b = self.pop();
                let a = self.pop();
                let (x, y) = (self.boxed(a), self.boxed(b));
                let r = match op {
                    Instanceof => RtOp::Instanceof,
                    In => RtOp::In,
                    _ => RtOp::HasOwn,
                };
                let v = self.js(Opcode::JsRt(r), vec![x, y], MType::val(TagSet::BOOLEAN));
                self.push(v, Ty::Val(TagSet::BOOLEAN));
                if op == Instanceof {
                    self.narrow_instanceof(a);
                }
            }
            DelProp | StrictDelProp => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let o = self.pop();
                let x = self.boxed(o);
                let r = RtOp::DelProp(a, op == StrictDelProp);
                let v = self.js(Opcode::JsRt(r), vec![x], MType::val(TagSet::BOOLEAN));
                self.push(v, Ty::Val(TagSet::BOOLEAN));
            }
            DelElem | StrictDelElem => {
                let k = self.pop();
                let o = self.pop();
                let (x, y) = (self.boxed(o), self.boxed(k));
                let r = RtOp::DelElem(op == StrictDelElem);
                let v = self.js(Opcode::JsRt(r), vec![x, y], MType::val(TagSet::BOOLEAN));
                self.push(v, Ty::Val(TagSet::BOOLEAN));
            }
            NewInit | NewObject => {
                let n = self
                    .s
                    .ctx
                    .lit_nslots_in
                    .get(&self.site(self.pc))
                    .copied()
                    .unwrap_or(0);
                let lid = self.s.ctx.lit_stamps_in.get(&self.site(self.pc)).copied();
                // A stamped site's allocation is typed (`lit.new`, no
                // fence: MIR-MEMORY.md §4); another's, the generic op.
                let v = match lid {
                    Some(_) => {
                        self.js_static(Opcode::LitNew(n), vec![], MType::val(TagSet::OBJECT))
                    }
                    _ => self.js(
                        Opcode::JsRt(RtOp::NewObject(n)),
                        vec![],
                        MType::val(TagSet::OBJECT),
                    ),
                };
                // An object-literal stamp site: the layout idx with SLOTS
                // (the inits land at the row's slots by construction).
                if let Some(lid) = lid {
                    let w = (lid + 1) | crate::wasm::mir::abi::CLASS_WORD_SLOTS;
                    self.inst(Opcode::StampFresh(w), vec![v], None);
                    self.val_cls.insert(v, lid);
                    self.lit_rows.insert(v, lid);
                }
                self.push(v, Ty::Val(TagSet::OBJECT));
            }
            NewArray => {
                let len = p.next_uint32().unwrap();
                let v = self.js(
                    Opcode::JsRt(RtOp::NewArray(len)),
                    vec![],
                    MType::val(TagSet::OBJECT),
                );
                // An array stamp site: the key with TYPES and RANGES, which
                // an empty array's elements satisfy vacuously; element
                // stores keep them.
                if let Some(&w) = self.s.ctx.array_stamp_in.get(&self.site(self.pc)) {
                    self.inst(Opcode::StampFresh(w), vec![v], None);
                }
                self.push(v, Ty::Val(TagSet::OBJECT));
            }
            InitProp | InitHiddenProp | InitLockedProp => {
                // [obj, v] -> [obj]
                use crate::wasm::mir::abi::{
                    INIT_ATTR_ENUMERATE, INIT_ATTR_HIDDEN, INIT_ATTR_LOCKED,
                };
                let a = self.atom(p.next_uint32().unwrap())?;
                let attrs = match op {
                    InitProp => INIT_ATTR_ENUMERATE,
                    InitHiddenProp => INIT_ATTR_HIDDEN,
                    _ => INIT_ATTR_LOCKED,
                };
                let v = self.pop();
                let o = self.top();
                let (x, y) = (self.boxed(o), self.boxed(v));
                match self.lit_rows.get(&x) {
                    Some(&lid) if attrs == INIT_ATTR_ENUMERATE => {
                        let ok = self.new_block();
                        let err = self.exit_block(true);
                        let key = crate::ids::LayoutKey::new(lid);
                        self.term(
                            Opcode::LitInit(a, key),
                            vec![x, y],
                            vec![Self::goto(ok), Self::goto(err)],
                        );
                        self.at(ok);
                    }
                    _ => self.js_void(Opcode::JsRt(RtOp::InitProp(a, attrs)), vec![x, y]),
                }
            }
            InitElem | InitHiddenElem | InitLockedElem => {
                // [obj, key, v] -> [obj]
                use crate::wasm::mir::abi::{
                    INIT_ATTR_ENUMERATE, INIT_ATTR_HIDDEN, INIT_ATTR_LOCKED,
                };
                let attrs = match op {
                    InitElem => INIT_ATTR_ENUMERATE,
                    InitHiddenElem => INIT_ATTR_HIDDEN,
                    _ => INIT_ATTR_LOCKED,
                };
                let v = self.pop();
                let k = self.pop();
                let o = self.top();
                let r = self.ranges_duty(pc, v);
                let (x, y, z) = (self.boxed(o), self.boxed(k), self.boxed(v));
                self.js_void(Opcode::JsRt(RtOp::InitElem(attrs, r)), vec![x, y, z]);
            }
            InitElemArray => {
                // [obj, v] -> [obj]
                let index = p.next_uint32().unwrap();
                let v = self.pop();
                let o = self.top();
                let k = self.const_val(ConstVal::Int32(index as i32));
                let r = self.ranges_duty(pc, v);
                let (x, z) = (self.boxed(o), self.boxed(v));
                let e = crate::wasm::mir::abi::INIT_ATTR_ENUMERATE;
                self.js_void(Opcode::JsRt(RtOp::InitElem(e, r)), vec![x, k, z]);
            }
            InitElemInc => {
                // [obj, idx, v] -> [obj, idx + 1]: an array literal's
                // spread, its index an int32 (the literal's positions).
                let v = self.pop();
                let i = self.pop();
                if i.ty.num() != Some(Num::I32) {
                    return Err("InitElemInc index not int32".into());
                }
                let o = self.top();
                let r = self.ranges_duty(pc, v);
                let (x, k, z) = (self.boxed(o), self.boxed(i), self.boxed(v));
                let e = crate::wasm::mir::abi::INIT_ATTR_ENUMERATE;
                self.js_void(Opcode::JsRt(RtOp::InitElem(e, r)), vec![x, k, z]);
                let iv = self.as_i32(i);
                let one = self.const_i32(1);
                let n = self.inst(
                    Opcode::I32Wrap(ArithOp::Add),
                    vec![iv, one],
                    Some(MType::I32_TOP),
                );
                self.push(n, Ty::I32);
            }
            ToPropertyKey => {
                // An int32, a string or a symbol is its own key.
                let x = self.top();
                let keyish = TagSet::INT32
                    .union(TagSet::STRING)
                    .union(TagSet::prims(crate::opsem::PRIM_SYMBOL));
                if !x.ty.tags().is_nonempty_subset_of(keyish) {
                    let x = self.pop();
                    let v = self.boxed(x);
                    let r = self.js(Opcode::JsRt(RtOp::ToPropertyKey), vec![v], MType::VAL_TOP);
                    self.push(r, Ty::Val(TagSet::ALL));
                }
            }
            Typeof | TypeofExpr => {
                let a = self.pop();
                let x = self.boxed(a);
                let v = self.inst(Opcode::JsTypeof, vec![x], Some(MType::val(TagSet::STRING)));
                self.push(v, Ty::Val(TagSet::STRING));
            }
            Throw => {
                let a = self.pop();
                let x = self.boxed(a);
                let err = self.exit_block(true);
                self.retain_locals(&Opcode::JsThrow);
                let (t, _) = self.f.add_inst(
                    self.cur,
                    Opcode::JsThrow,
                    vec![x],
                    &[],
                    vec![Self::goto(err)],
                );
                self.live = false;
                self.f.witnesses[t] = Some(mir::func::Witness {
                    may_kill: mir::types::KillPattern::ALL,
                });
            }
            // The actuals (the frame's variable region is past them: the
            // lowering's `vp`).
            Arguments => {
                if self.s.apply_fwd.is_some() || self.s.args_reads.is_some() {
                    // Only forwarded (`apply_forward`): never made, unless an
                    // exit needs it (`exit_operands`). A value of its own,
                    // so no other undefined is mistaken for it.
                    let v = self.const_val(ConstVal::Undefined);
                    self.args_placeholder = Some(v);
                    self.push(v, Ty::Val(TagSet::prims(PRIM_UNDEFINED)));
                } else {
                    let v = self.js_static(Opcode::ArgsObject, vec![], MType::val(TagSet::OBJECT));
                    self.push(v, Ty::Val(TagSet::OBJECT));
                }
            }
            Rest => {
                let nformal = self.s.nargs.saturating_sub(1);
                let v = self.js_static(
                    Opcode::RestArray(nformal),
                    vec![],
                    MType::val(TagSet::OBJECT),
                );
                self.push(v, Ty::Val(TagSet::OBJECT));
            }
            ArgumentsLength => {
                let v = self.inst(Opcode::ArgsLength, vec![], Some(MType::I32_TOP));
                self.push(v, Ty::I32);
            }
            GetActualArg => {
                let i = self.pop();
                if i.ty != Ty::I32 {
                    return Err("GetActualArg of a non-int32 index".into());
                }
                let v = self.inst(Opcode::ActualArg, vec![i.v], Some(MType::VAL_TOP));
                self.push(v, Ty::Val(TagSet::ALL));
            }
            Iter => {
                // For-in (baseline's helpers): the property iterator; the
                // loop's names from it; its close. A throw inside exits,
                // and baseline's unwind closes it.
                let x = self.pop();
                let v = self.boxed(x);
                let r = self.js(
                    Opcode::JsRt(RtOp::Iter),
                    vec![v],
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            MoreIter => {
                let it = self.top();
                let v = self.boxed(it);
                let r = self.inst(Opcode::IterMore, vec![v], Some(MType::VAL_TOP));
                self.push(r, Ty::Val(TagSet::ALL));
            }
            IsNoIter => {
                let x = self.top();
                let v = self.boxed(x);
                let b = self.inst(Opcode::IterIsDone, vec![v], Some(MType::Bool));
                self.push(b, Ty::Bool);
            }
            EndIter => {
                self.pop();
                let it = self.pop();
                let v = self.boxed(it);
                self.inst(Opcode::IterEnd, vec![v], None);
            }
            Callee => {
                let v = self.inst(
                    Opcode::FrameCallee,
                    vec![],
                    Some(MType::val(TagSet::OBJECT)),
                );
                self.push(v, Ty::Val(TagSet::OBJECT));
            }
            CheckObjCoercible | CheckClassHeritage | CheckThis => {
                // The runtime's check of the value, which stays.
                let k = match op {
                    CheckObjCoercible => mir::ops::CHECK_OBJ_COERCIBLE,
                    CheckClassHeritage => mir::ops::CHECK_CLASS_HERITAGE,
                    _ => mir::ops::CHECK_THIS,
                };
                let x = self.top();
                let v = self.boxed(x);
                self.js_void(Opcode::JsRt(RtOp::Check(k)), vec![v]);
            }
            SetFunName => {
                let prefix = u32::from(p.next_uint8().unwrap());
                let name = self.pop();
                let fun = self.top();
                let (f, n) = (self.boxed(fun), self.boxed(name));
                self.js_void(Opcode::JsRt(RtOp::SetFunName(prefix)), vec![f, n]);
            }
            GlobalThis => {
                let r = self.js(
                    Opcode::JsRt(RtOp::GlobalThis),
                    vec![],
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            BigInt => {
                let idx = p.next_uint32().unwrap();
                let r = self.js(Opcode::JsRt(RtOp::BigInt(idx)), vec![], MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
            }
            MutateProto => {
                let proto = self.pop();
                let obj = self.top();
                let (o, pr) = (self.boxed(obj), self.boxed(proto));
                self.js_void(Opcode::JsRt(RtOp::MutateProto), vec![o, pr]);
            }
            CheckPrivateField => {
                let cond = u32::from(p.next_uint8().unwrap());
                let kind = u32::from(p.next_uint8().unwrap());
                let n = self.st.len();
                let (obj, key) = (self.st[n - 2], self.st[n - 1]);
                let (o, k) = (self.boxed(obj), self.boxed(key));
                let r = self.js(
                    Opcode::JsRt(RtOp::CheckPrivateField(cond, kind)),
                    vec![o, k],
                    MType::val(TagSet::BOOLEAN),
                );
                self.push(r, Ty::Val(TagSet::BOOLEAN));
            }
            Hole => {
                let v = self.const_val(ConstVal::Hole);
                self.push(v, Ty::Val(TagSet::MAGIC));
            }
            // Always throws: baseline does, from here.
            ThrowMsg | ThrowSetConst => {
                let b = self.exit_block(false);
                self.term(Opcode::Jump, vec![], vec![Self::goto(b)]);
            }
            NewTarget => {
                // The frame's new.target (undefined unless constructed).
                let v = self.inst(Opcode::FrameNewTarget, vec![], Some(MType::VAL_TOP));
                self.push(v, Ty::Val(TagSet::ALL));
            }
            IsConstructing => {
                let v = self.const_val(ConstVal::IsConstructing);
                self.push(v, Ty::Val(TagSet::MAGIC));
            }
            New | NewContent => {
                // [callee, this, args…, new.target] -> [object]
                self.publish_this();
                let argc = usize::from(p.next_uint16().unwrap());
                let n = self.st.len();
                let operands: Vec<Slot> = self.st.drain(n - argc - 3..).collect();
                let vals: Vec<mir::Value> = operands.into_iter().map(|x| self.boxed(x)).collect();
                let site = self.site(pc);
                let mono = match self.s.ctx.facts.scripted_targets(site) {
                    [s] => Some(*s),
                    _ => None,
                };
                let nslots = crate::wasm::mir::stamp::construct_nslots(self.s.ctx, mono, site);
                let word = self.s.alloc_word(mono, site);
                let callee = match mono {
                    Some(k) => self
                        .admit(&[k], true, &|s, k, ictx| s.callee(k, ictx))
                        .map(|mut t| t.remove(0)),
                    _ => None,
                };
                let r = match callee {
                    Some((k, c)) => self.inline_construct(k, &c, &vals, nslots, word),
                    _ => self.js(
                        Opcode::Construct(nslots, word),
                        vals,
                        MType::val(TagSet::OBJECT),
                    ),
                };
                // The object the site's constructor builds: its layout, as
                // an advisory class (checked at use).
                let built = mono
                    .and_then(|f| self.s.ctx.stamp_ctors_in.get(&f))
                    .or_else(|| self.s.ctx.construct_sites_in.get(&site));
                if let Some(si) = built {
                    self.val_cls.insert(r, si.layout_id);
                }
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            PushLexicalEnv | PushClassBodyEnv | PushVarEnv => {
                // A scope's environment, made the frame's (write-through:
                // an exit's baseline frame, and its unwind, see it).
                let kind = match op {
                    PushLexicalEnv => mir::ops::ENV_LEXICAL,
                    PushClassBodyEnv => mir::ops::ENV_CLASS_BODY,
                    _ => mir::ops::ENV_VAR,
                };
                let e = self.js(
                    Opcode::JsRt(RtOp::PushEnv(kind, pc.get())),
                    vec![],
                    MType::val(TagSet::OBJECT),
                );
                self.inst(Opcode::EnvSet, vec![e], None);
            }
            EnterWith => {
                let x = self.pop();
                let v = self.boxed(x);
                let e = self.js(
                    Opcode::JsRt(RtOp::EnterWith(pc.get())),
                    vec![v],
                    MType::val(TagSet::OBJECT),
                );
                self.inst(Opcode::EnvSet, vec![e], None);
            }
            FreshenLexicalEnv | RecreateLexicalEnv => {
                let recreate = u32::from(op == RecreateLexicalEnv);
                let e = self.js(
                    Opcode::JsRt(RtOp::FreshenEnv(recreate)),
                    vec![],
                    MType::val(TagSet::OBJECT),
                );
                self.inst(Opcode::EnvSet, vec![e], None);
            }
            PopLexicalEnv | LeaveWith => {
                self.inst(Opcode::EnvPop, vec![], None);
            }
            GetName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let t = u32::from(self.s.next_is_typeof(pc, op));
                let r = self.js(Opcode::JsRt(RtOp::GetName(a, t)), vec![], MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
            }
            BindName | BindUnqualifiedName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let k = u32::from(op == BindUnqualifiedName);
                let r = self.js(
                    Opcode::JsRt(RtOp::BindName(a, k)),
                    vec![],
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            DelName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let r = self.js(
                    Opcode::JsRt(RtOp::DelName(a)),
                    vec![],
                    MType::val(TagSet::BOOLEAN),
                );
                self.push(r, Ty::Val(TagSet::BOOLEAN));
            }
            BindVar => {
                let r = self.js(
                    Opcode::JsRt(RtOp::BindVar),
                    vec![],
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            SetName | StrictSetName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let v = self.pop();
                let env = self.pop();
                let (e, y) = (self.boxed(env), self.boxed(v));
                self.js_void(
                    Opcode::JsRt(RtOp::SetName(a, op == StrictSetName)),
                    vec![e, y],
                );
                self.repush(v);
            }
            Eval | StrictEval => {
                let argc = usize::from(p.next_uint16().unwrap());
                self.publish_this();
                let n = self.st.len();
                let operands: Vec<Slot> = self.st.drain(n - argc - 2..).collect();
                let vals: Vec<mir::Value> = operands.into_iter().map(|x| self.boxed(x)).collect();
                let r = self.js(Opcode::CallEval(pc.get()), vals, MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
            }
            Coalesce => {
                // `a ?? b`: on to `b` when the top is null or undefined,
                // else keep it and jump past.
                let off = p.next_int32().unwrap();
                let a = self.top();
                let nullish = TagSet::prims(PRIM_NULL | PRIM_UNDEFINED);
                let tags = a.ty.tags();
                let b = if tags.subset_of(nullish) {
                    self.inst(Opcode::ConstBool(true), vec![], Some(MType::Bool))
                } else if tags.intersect(nullish).is_empty() {
                    self.inst(Opcode::ConstBool(false), vec![], Some(MType::Bool))
                } else {
                    let x = self.boxed(a);
                    self.tag_test(x, nullish)
                };
                let (target, next) = (pc.branch(off), pc + op.len());
                self.branch(b, next, target);
            }
            Object | CallSiteObj => {
                let index = p.next_uint32().unwrap();
                let r = self.inst(
                    Opcode::ObjectLit(index),
                    vec![],
                    Some(MType::val(TagSet::OBJECT)),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            EnvCallee => {
                let hops = match op.len() {
                    2 => u32::from(p.next_uint8().unwrap()),
                    _ => u32::from(p.next_uint16().unwrap()),
                };
                let r = self.inst(
                    Opcode::EnvCallee(hops),
                    vec![],
                    Some(MType::val(TagSet::OBJECT)),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            InitElemGetter | InitHiddenElemGetter | InitElemSetter | InitHiddenElemSetter => {
                let kind = u32::from(matches!(op, InitElemSetter | InitHiddenElemSetter))
                    | (u32::from(matches!(op, InitHiddenElemGetter | InitHiddenElemSetter)) << 1);
                let f = self.pop();
                let k = self.pop();
                let o = self.top();
                let (x, y, z) = (self.boxed(o), self.boxed(k), self.boxed(f));
                self.js_void(Opcode::JsRt(RtOp::InitElemGetSet(kind)), vec![x, y, z]);
            }
            SuperCall => {
                // `super(args)`: the runtime's construct of the parent
                // (`[callee, IS_CONSTRUCTING, args…, new.target]`), which
                // makes `this`; no site sizing, as in baseline.
                self.publish_this();
                let argc = usize::from(p.next_uint16().unwrap());
                let n = self.st.len();
                let operands: Vec<Slot> = self.st.drain(n - argc - 3..).collect();
                let vals: Vec<mir::Value> = operands.into_iter().map(|x| self.boxed(x)).collect();
                let nslots = crate::wasm::mir::abi::NO_NSLOTS;
                let r = self.js(
                    Opcode::Construct(nslots, 0),
                    vals,
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            SuperBase | SuperFun => {
                let x = self.pop();
                let v = self.boxed(x);
                let r = if op == SuperBase {
                    RtOp::SuperBase
                } else {
                    RtOp::SuperFun
                };
                let r = self.js(Opcode::JsRt(r), vec![v], MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
            }
            GetPropSuper => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let base = self.pop();
                let recv = self.pop();
                let (x, y) = (self.boxed(recv), self.boxed(base));
                let r = self.js(
                    Opcode::JsRt(RtOp::GetPropSuper(a)),
                    vec![x, y],
                    MType::VAL_TOP,
                );
                self.push(r, Ty::Val(TagSet::ALL));
            }
            GetElemSuper => {
                let base = self.pop();
                let key = self.pop();
                let recv = self.pop();
                let vals = vec![self.boxed(recv), self.boxed(key), self.boxed(base)];
                let r = self.js(Opcode::JsRt(RtOp::GetElemSuper), vals, MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
            }
            SetPropSuper | StrictSetPropSuper => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let v = self.pop();
                let base = self.pop();
                let recv = self.pop();
                let vals = vec![self.boxed(recv), self.boxed(base), self.boxed(v)];
                let r = self.js(
                    Opcode::JsRt(RtOp::SetPropSuper(a, op == StrictSetPropSuper)),
                    vals,
                    MType::VAL_TOP,
                );
                self.push(r, Ty::Val(TagSet::ALL));
            }
            SetElemSuper | StrictSetElemSuper => {
                let v = self.pop();
                let base = self.pop();
                let key = self.pop();
                let recv = self.pop();
                let vals = vec![
                    self.boxed(recv),
                    self.boxed(key),
                    self.boxed(base),
                    self.boxed(v),
                ];
                let r = self.js(
                    Opcode::JsRt(RtOp::SetElemSuper(op == StrictSetElemSuper)),
                    vals,
                    MType::VAL_TOP,
                );
                self.push(r, Ty::Val(TagSet::ALL));
            }
            InitHomeObject => {
                let home = self.pop();
                let f = self.top();
                let (x, y) = (self.boxed(f), self.boxed(home));
                self.js_void(Opcode::JsRt(RtOp::InitHomeObject), vec![x, y]);
            }
            FunWithProto => {
                let index = p.next_uint32().unwrap();
                let x = self.pop();
                let v = self.boxed(x);
                let r = self.js(
                    Opcode::JsRt(RtOp::FunWithProto(index)),
                    vec![v],
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            CheckThisReinit => {
                let x = self.top();
                let v = self.boxed(x);
                self.js_void(
                    Opcode::JsRt(RtOp::Check(mir::ops::CHECK_THIS_REINIT)),
                    vec![v],
                );
            }
            CheckReturn => {
                // [this] -> [result], with the frame's rval.
                let t = self.pop();
                let rv = self.st[self.rval_ix()];
                let (x, y) = (self.boxed(t), self.boxed(rv));
                let r = self.js(Opcode::JsRt(RtOp::CheckReturn), vec![x, y], MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
            }
            AddDisposable => {
                let hint = u32::from(p.next_uint8().unwrap());
                let nc = self.pop();
                let m = self.pop();
                let v = self.pop();
                let vals = vec![self.boxed(v), self.boxed(m), self.boxed(nc)];
                self.js_void(Opcode::JsRt(RtOp::AddDisposable(hint)), vals);
            }
            TakeDisposeCapability => {
                let r = self.js(
                    Opcode::JsRt(RtOp::TakeDisposeCapability),
                    vec![],
                    MType::VAL_TOP,
                );
                self.push(r, Ty::Val(TagSet::ALL));
            }
            CreateSuppressedError => {
                let sup = self.pop();
                let e = self.pop();
                let vals = vec![self.boxed(e), self.boxed(sup)];
                let r = self.js(
                    Opcode::JsRt(RtOp::CreateSuppressedError),
                    vals,
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            GetBoundName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let x = self.pop();
                let v = self.boxed(x);
                let r = self.js(Opcode::JsRt(RtOp::GetBoundName(a)), vec![v], MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
            }
            ObjWithProto => {
                let x = self.pop();
                let v = self.boxed(x);
                let r = self.js(
                    Opcode::JsRt(RtOp::ObjWithProto),
                    vec![v],
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            NewPrivateName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let t = TagSet::prims(crate::opsem::PRIM_SYMBOL);
                let r = self.js(Opcode::JsRt(RtOp::NewPrivateName(a)), vec![], MType::val(t));
                self.push(r, Ty::Val(t));
            }
            DynamicImport => {
                let o = self.pop();
                let s = self.pop();
                let vals = vec![self.boxed(s), self.boxed(o)];
                let r = self.js(
                    Opcode::JsRt(RtOp::DynamicImport),
                    vals,
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            SpreadEval | StrictSpreadEval => {
                self.publish_this();
                let arr = self.pop();
                let thisv = self.pop();
                let callee = self.pop();
                let vals = vec![self.boxed(callee), self.boxed(thisv), self.boxed(arr)];
                let r = self.js(
                    Opcode::JsRt(RtOp::SpreadEval(pc.get())),
                    vals,
                    MType::VAL_TOP,
                );
                self.push(r, Ty::Val(TagSet::ALL));
            }
            Generator => {
                let r = self.js(
                    Opcode::JsRt(RtOp::CreateGenerator),
                    vec![],
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            InitialYield | Yield | Await => {
                // Suspend with the frame as an exit writes it; a resume
                // enters this yield's own root at the landing (§5.2's
                // onramps, keyed by the resume index).
                let index = p.next_uint24().unwrap();
                let initial = op == InitialYield;
                let before = self.st.clone();
                let ops = self.exit_operands(pc, &before);
                let (nargs, nlocals) = (self.s.nargs, self.s.nlocals);
                self.term(
                    Opcode::GenSuspend {
                        pc,
                        index,
                        nargs,
                        nlocals,
                        initial,
                    },
                    ops,
                    vec![],
                );
                let popped = if initial { 1 } else { 2 };
                self.resume_root(index, pc + op.len(), &before, popped)?;
            }
            AfterYield => {
                p.next_uint24();
            }
            FinalYieldRval => {
                let g = self.pop();
                let gv = self.boxed(g);
                self.js_void(Opcode::JsRt(RtOp::GenFinal), vec![gv]);
                let x = self.st[self.rval_ix()];
                let v = if x.ty == Ty::Dead {
                    self.const_val(ConstVal::Undefined)
                } else {
                    self.boxed(x)
                };
                self.term(Opcode::Return, vec![v], vec![]);
            }
            ResumeKind => {
                let kind = i32::from(p.next_uint8().unwrap());
                let v = self.const_i32(kind);
                self.push(v, Ty::I32);
            }
            IsGenClosing => {
                let x = self.top();
                let v = self.boxed(x);
                let b = self.inst(Opcode::IsGenClosing, vec![v], Some(MType::Bool));
                self.push(b, Ty::Bool);
            }
            CheckResumeKind => {
                // [val, gen, kind] -> [val]: next goes on; throw and return
                // raise through the helper, which always fails.
                let k = self.pop();
                let g = self.pop();
                let next = match k.ty {
                    Ty::I32 => self.const_i32_of(k.v),
                    _ => None,
                };
                if next != Some(0) {
                    let ki = if k.ty == Ty::I32 {
                        k.v
                    } else {
                        let kv = self.boxed(k);
                        self.guard(Opcode::GuardUnbox(UnboxKind::I32), vec![kv], MType::I32_TOP)
                    };
                    let zero = self.const_i32(0);
                    let is_next = self.inst(
                        Opcode::Cmp(NumRepr::I32, Cc::Eq),
                        vec![ki, zero],
                        Some(MType::Bool),
                    );
                    let (cont, raise) = (self.new_block(), self.new_block());
                    self.term(
                        Opcode::Br,
                        vec![is_next],
                        vec![Self::goto(cont), Self::goto(raise)],
                    );
                    self.at(raise);
                    let v = self.top();
                    let vals = vec![self.boxed(v), self.boxed(g), self.boxed(k)];
                    // A return stages its value as the frame's rval: the
                    // throw's exit must leave that slot as the helper wrote
                    // it.
                    let ix = self.rval_ix();
                    self.pre[ix] = Slot::dead();
                    self.js_void(Opcode::JsRt(RtOp::GenCheckResume), vals);
                    self.term(Opcode::Unreachable, vec![], vec![]);
                    self.at(cont);
                }
            }
            AsyncAwait | AsyncResolve => {
                let g = self.pop();
                let v = self.pop();
                let vals = vec![self.boxed(v), self.boxed(g)];
                let r = self.js(
                    Opcode::JsRt(RtOp::AsyncAwait(u32::from(op == AsyncResolve))),
                    vals,
                    MType::VAL_TOP,
                );
                self.push(r, Ty::Val(TagSet::ALL));
            }
            AsyncReject => {
                let g = self.pop();
                let stack = self.pop();
                let reason = self.pop();
                let vals = vec![self.boxed(reason), self.boxed(stack), self.boxed(g)];
                let r = self.js(Opcode::JsRt(RtOp::AsyncReject), vals, MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
            }
            CanSkipAwait => {
                let x = self.top();
                let v = self.boxed(x);
                let r = self.js(
                    Opcode::JsRt(RtOp::CanSkipAwait),
                    vec![v],
                    MType::val(TagSet::BOOLEAN),
                );
                self.push(r, Ty::Val(TagSet::BOOLEAN));
            }
            MaybeExtractAwaitValue => {
                // [v, can] -> [v', can]
                let can = self.pop();
                let x = self.pop();
                let vals = vec![self.boxed(x), self.boxed(can)];
                let r = self.js(Opcode::JsRt(RtOp::MaybeExtractAwait), vals, MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
                self.repush(can);
            }
            Resume => {
                let k = self.pop();
                let v = self.pop();
                let g = self.pop();
                let vals = vec![self.boxed(g), self.boxed(v), self.boxed(k)];
                let r = self.js(Opcode::JsRt(RtOp::Resume), vals, MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
            }
            CallIter | CallContentIter => {
                // An iterator method (`obj[Symbol.iterator]()`, a
                // `return`): the ordinary call, whose uncallable callee
                // throws the iterator protocol's error.
                let argc = usize::from(p.next_uint16().unwrap());
                self.publish_this();
                let n = self.st.len();
                let operands: Vec<Slot> = self.st.drain(n - argc - 2..).collect();
                let vals: Vec<mir::Value> = operands.into_iter().map(|x| self.boxed(x)).collect();
                let r = self.js(Opcode::CallIter, vals, MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
                let claim = self.s.ctx.facts.call_types.get(&self.site(pc)).copied();
                self.guard_result(claim.unwrap_or_default(), pc + op.len(), false);
            }
            SpreadCall | SpreadNew | SpreadSuperCall => {
                // `f(...a)` / `new f(...a)` / `super(...a)`: baseline's
                // helper, over the spread's argument array.
                let construct = op != SpreadCall;
                self.publish_this();
                let nt = if construct {
                    let x = self.pop();
                    self.boxed(x)
                } else {
                    self.const_val(ConstVal::Null)
                };
                let arr = self.pop();
                let thisv = self.pop();
                let callee = self.pop();
                let vals = vec![self.boxed(callee), self.boxed(thisv), self.boxed(arr), nt];
                let (out, ty) = if construct {
                    (MType::val(TagSet::OBJECT), TagSet::OBJECT)
                } else {
                    (MType::VAL_TOP, TagSet::ALL)
                };
                let r = self.js(
                    Opcode::JsRt(RtOp::SpreadCall(u32::from(construct))),
                    vals,
                    out,
                );
                self.push(r, Ty::Val(ty));
            }
            OptimizeSpreadCall => {
                let x = self.pop();
                let v = self.boxed(x);
                let r = self.js(
                    Opcode::JsRt(RtOp::OptimizeSpreadCall),
                    vec![v],
                    MType::VAL_TOP,
                );
                self.push(r, Ty::Val(TagSet::ALL));
            }
            OptimizeGetIterator => {
                let x = self.pop();
                let v = self.boxed(x);
                let b = self.inst(Opcode::IterOptimizable, vec![v], Some(MType::Bool));
                self.push(b, Ty::Bool);
            }
            CheckIsObj => {
                let kind = u32::from(p.next_uint8().unwrap());
                let x = self.top();
                let v = self.boxed(x);
                self.js_void(Opcode::JsRt(RtOp::CheckIsObj(kind)), vec![v]);
            }
            CloseIter => {
                let kind = u32::from(p.next_uint8().unwrap());
                let x = self.pop();
                let v = self.boxed(x);
                self.js_void(Opcode::JsRt(RtOp::CloseIter(kind)), vec![v]);
            }
            Call | CallIgnoresRv | CallContent => {
                let argc = usize::from(p.next_uint16().unwrap());
                if self.math_call(argc) {
                    return Ok(());
                }
                self.publish_this();
                let n = self.st.len();
                let mut operands: Vec<Slot> = self.st.drain(n - argc - 2..).collect();
                // A receiver (or a `.call`'s `this`) a fence demoted from
                // under construction, for callees to inline: back to its
                // construction state (§2.3).
                {
                    let site = self.site(pc);
                    let facts = &self.s.ctx.facts;
                    let direct = !facts.scripted_targets(site).is_empty();
                    let fwd = (argc >= 1
                        && facts.apply_sites.get(&site) == Some(&crate::facts::CallForm::Call)
                        && (facts.apply_targets.contains_key(&site)
                            || facts.apply_target_sets.contains_key(&site)))
                        || (argc == 2
                            && self.s.apply_fwd.as_ref().is_some_and(|f| f.contains(&pc)));
                    for (i, want) in [(1, direct), (2, fwd)] {
                        if want {
                            if let Some(s) = self.ctor_reguard(operands[i]) {
                                operands[i] = s;
                            }
                        }
                    }
                }
                // A known closure is the callee (context-sensitive: known
                // here, maybe not where the analysis looked); the closures
                // the call passes go to the callee's build.
                let known = match operands[0].ty {
                    Ty::Fn(k) => Some(k),
                    _ => None,
                };
                let fns: Vec<Option<ScriptId>> = operands[2..]
                    .iter()
                    .map(|x| match x.ty {
                        Ty::Fn(k) => Some(k),
                        _ => None,
                    })
                    .collect();
                // An object under construction as the receiver, or as a
                // `.call`'s `this`: callees built for it (§2.3).
                let this_ctor = |x: Option<&Slot>| match x.map(|x| x.ty) {
                    Some(Ty::Ctor(k, n, t)) => Some((k, n, t)),
                    _ => None,
                };
                let recv_ctor = this_ctor(operands.get(1)).map(|c| (c, operands[1]));
                let fwd_ctor = this_ctor(operands.get(2)).map(|c| (c, operands[2]));
                // A published receiver of a proven type: callees built for
                // it, its own guard subsumed.
                let recv_obj = match operands.get(1).map(|x| x.ty) {
                    Some(Ty::Obj(keys, t, sl)) if recv_ctor.is_none() => {
                        Some(((keys.lo.get(), keys.hi.get(), t, sl), operands[1].v))
                    }
                    _ => None,
                };
                let vals: Vec<mir::Value> = operands.into_iter().map(|x| self.boxed(x)).collect();
                let facts_sids = self.s.ctx.facts.scripted_targets(self.site(pc));
                let known_sids: Vec<ScriptId> = known.into_iter().collect();
                let sids: &[ScriptId] = if known.is_some() {
                    &known_sids
                } else {
                    facts_sids
                };
                let sids: Vec<ScriptId> = sids.to_vec();
                let forward = if argc == 2 && self.hasown_site() {
                    Some(self.hasown_call(&vals))
                } else if argc >= 1 {
                    self.call_forward(&vals, &fns[1..], fwd_ctor.map(|(c, x)| (c, x, vals[2])))
                } else {
                    None
                };
                let r = if let Some(r) = forward {
                    r
                } else if argc == 2 && self.s.apply_fwd.as_ref().is_some_and(|f| f.contains(&pc)) {
                    self.apply_forward(&vals, fwd_ctor)
                } else if let Some(targets) = {
                    let rc = recv_ctor.map(|(c, _)| c);
                    let ro = recv_obj.map(|(o, _)| o);
                    let fns = &fns;
                    self.admit(&sids, false, &|s, k, ictx| {
                        s.callee_ctx(k, fns, rc, ro, ictx)
                    })
                } {
                    let raw = recv_ctor.map(|(_, x)| self.ctor_pass(x, vals[1]));
                    let r = if let Some((_, o)) = recv_obj {
                        self.inline_call_this(&targets, &vals, None, Some(o), false)
                    } else {
                        self.inline_call_this(&targets, &vals, None, raw, raw.is_some())
                    };
                    if raw.is_some() {
                        self.ctor_after(&targets, vals[1]);
                    }
                    r
                } else {
                    // Not inlined: a known single callee is called
                    // directly while it is the callee.
                    self.likely_targets = if sids.len() <= MAX_DIRECT_TARGETS
                        && sids.iter().all(|&k| self.s.direct_ok(k))
                    {
                        sids.to_vec()
                    } else {
                        vec![]
                    };
                    let r = self.js(Opcode::Call, vals, MType::VAL_TOP);
                    self.likely_targets.clear();
                    r
                };
                self.push(r, Ty::Val(TagSet::ALL));
                let claim = self.s.ctx.facts.call_types.get(&self.site(pc)).copied();
                self.guard_result(claim.unwrap_or_default(), pc + op.len(), false);
            }

            op => return Err(format!("{op:?}")),
        }
        Ok(())
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
