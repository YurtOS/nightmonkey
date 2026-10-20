//! Lowering a MIR function to its own waffle function (MIR.md §9).
//!
//! The body has the shared `night_abi_sig2` signature and is the script's
//! table entry; the script's baseline body is a separate function that the
//! exits call (`docs/BASELINE.md` §7).
//!
//! - **Values.** Each MIR value becomes at most one waffle value per point:
//!   `Val` and `Int` are i64, `F64` is f64, the rest i32, and ghosts
//!   (`Fact`) vanish. Blocks map one to one, plus internal blocks for the
//!   ops that branch.
//! - **Reducibility (§5.4).** A function with onramp roots may be
//!   irreducible; waffle's backend makes it reducible by duplication.
//! - **Rooting (§4.4).** A managed value (`Val`, `Obj`, `Str`) that may be
//!   live across a may-GC call has a home slot in the rooting area, above
//!   baseline's fixed frame and locals; values never live at once share
//!   one. Every may-GC call's GC scan covers the whole area, initialized
//!   at entry. Before a call, each live managed value its slot does not
//!   hold yet is stored, boxed; SSA values do not change and the GC
//!   updates slots in place, so it stays stored across later calls. After
//!   a call, register copies are dropped, and a value is reloaded where
//!   next used. Where each managed value is (register, slot, or both) is
//!   tracked along the emission; a block's entry state is decided from
//!   its incoming edges, each of which gets its own waffle block to bring
//!   the value there (`enter_block`, `conform`). Unmanaged values are
//!   never rooted.
//! - **Frame.** Formals, locals and rval stay in their own
//!   representations (no write-through, except a mapped `arguments`'s
//!   formals); exits write them.
//! - **Exits (§5.1).** An exit writes the whole baseline frame (`this`,
//!   formals, locals, rval, stack, and the fixed slots), stores its resume
//!   word, calls the baseline body with `ARGC_RESUME_BIT`, and returns
//!   what it returns.
//! - **Effects.** Every body reports `FLAGS_ALL`, and a generic op always
//!   takes its `ok_dirty` edge; accurate flags come with M5.

use std::collections::{BTreeMap, BTreeSet};

use waffle::entity::EntityRef;
use waffle::{
    Block, BlockTarget, Func, FunctionBody, MemoryArg, Module, Operator, Terminator, Type, Value,
    ValueDef,
};

use crate::ids::{Pc, ScriptId};
use crate::mir;
use crate::mir::func::{Edge, EdgeArg, RootKind};
use crate::mir::ops::{
    frame_parts, ArithOp, BindingCheck, BitOp, Cc, ConstVal, F64Op, JsBinop, JsCc, JsUnop, MathFn,
    NumRepr, Opcode, UnboxKind,
};
use crate::mir::types::{FactKind, Machine, ObjKind, TagSet, Type as MType};
use crate::opsem::{
    Prims, PRIM_BIGINT, PRIM_BOOLEAN, PRIM_DOUBLE, PRIM_INT32, PRIM_NULL, PRIM_STRING, PRIM_SYMBOL,
    PRIM_UNDEFINED,
};
use crate::region_shape::BINDING_FUSE_PREDICTED;
use crate::wasm::baseline::layout::{
    FrameLayout, ResumeMode, ResumeWord, ARGC_FLAGS, ARGC_ONRAMP_BIT, ARGC_RESUME_BIT, ERR_DEOPT,
    ONRAMP_BACKOFF,
};
use crate::wasm::mir::abi::{
    ALLOC_CELL_ADDR_PLACEHOLDER, BASESCRIPT_NIGHTFUNCINDEX_OFFSET, BASESHAPE_CLASP_OFFSET,
    BASESHAPE_PROTO_OFFSET, BINOP_BITAND, BINOP_BITNOT, BINOP_BITOR, BINOP_BITXOR, BINOP_DEC,
    BINOP_DIV, BINOP_INC, BINOP_LSH, BINOP_MOD, BINOP_MUL, BINOP_RSH, BINOP_SUB, BINOP_URSH,
    CALLOBJ_CALLEE_OFFSET, CALLOBJ_ENCLOSING_OFFSET, CALL_CELL_ADDR_PLACEHOLDER, CALL_CELL_FUNCIDX,
    CALL_CELL_SCRIPT, CHUNK_STORE_BUFFER_OFFSET, CLASS_WORD_CLOSED, CLASS_WORD_RANGES,
    CLASS_WORD_SENTINEL, CLASS_WORD_SHALLOW, CLASS_WORD_SLOTS, CMP_EQ, CMP_GE, CMP_GT, CMP_LE,
    CMP_LT, CMP_NE, CMP_STRICTEQ, CMP_STRICTNE, CONSTRUCT_CELL_ADDR_PLACEHOLDER,
    CONSTRUCT_CELL_CTORSHAPE, CONSTRUCT_CELL_FINALSHAPE, CONSTRUCT_CELL_GEN,
    CONSTRUCT_CELL_PROTO2PTR, CONSTRUCT_CELL_PROTO2SHAPE, CONSTRUCT_CELL_PROTOPTR,
    CONSTRUCT_CELL_PROTOSHAPE, CONSTRUCT_CELL_PROTOSLOTENC, EARLY_KEY_MAX, EARLY_KEY_SHIFT,
    ELEMENTS_CAPACITY_BACK, ELEMENTS_FLAGS_BACK, ELEMENTS_FROZEN_FLAG, ELEMENTS_HEADER_BYTES,
    ELEMENTS_INITLEN_BACK, ELEMENTS_LENGTH_BACK, ELEMENTS_PUSH_BAIL_MASK, ENV_CELL_ARMED,
    ENV_CELL_CLASS_WORD, ENV_CELL_GEN, FAT_INLINE_MAX_LATIN1, FAT_INLINE_MAX_TWO_BYTE,
    FIXED_SLOTS_BASE, FLAGS_ALL, FUNCTION_FLAGS_CONSTRUCTOR, FUNC_ENV_SLOT_OFFSET,
    FUNC_FLAGS_SLOT_OFFSET, FUNC_SCRIPT_SLOT_OFFSET, IC_SET_ABSSLOT, IC_SET_RECVSHAPE,
    IC_SET_SLOTENC, IC_TRANS_ABSSLOT, IC_TRANS_NEWSHAPE, IC_TRANS_OLDSHAPE, IC_TRANS_PROTO0,
    IC_TRANS_PROTO_HOPS, IC_TRANS_PROTO_ROW_BYTES, IC_TRANS_ROW_OFF, IC_TRANS_SLOTOFF,
    IC_WAY_ADDR_PLACEHOLDER, IC_WAY_HOLDERPTR, IC_WAY_MONO_OFF, IC_WAY_RECVSHAPE,
    IOF_CELL_ADDR_PLACEHOLDER, IOF_CELL_GEN, IOF_CELL_SLOTENC, JSCLASSOPS_CALL_OFFSET,
    JSCLASS_COPS_OFFSET, JSCLASS_EMULATES_UNDEFINED, JSCLASS_FLAGS_OFFSET, JSCLASS_IS_PROXY,
    JSCONTEXT_REALM_OFFSET, JSCONTEXT_ZONE_OFFSET, LAMBDA_CELL_GEN, LAMBDA_CELL_TEMPLATE,
    MAX_FIXED_SLOTS, NATIVE_SLOTS_OFFSET, NOT_CHUNK_MASK, NURSERY_HEADER_BYTES,
    OBJ_CLASS_IDX_OFFSET, OBJ_ELEMENTS_OFFSET, REALM_GLOBAL_OFFSET, ROPE_BYTES, ROPE_LEFT_OFFSET,
    ROPE_RIGHT_OFFSET, SHAPE_BASESHAPE_OFFSET, SHAPE_FIXED_SLOTS_MASK_BITS,
    SHAPE_FIXED_SLOTS_SHIFT, SHAPE_IMMUTABLE_FLAGS_OFFSET, SHAPE_IS_NATIVE_BIT, SHAPE_OFFSET,
    SHAPE_PERMUTED_SLOTS_BIT, SHAPE_SMALL_SLOTSPAN_MASK_BITS, SHAPE_SMALL_SLOTSPAN_SHIFT,
    STRING_CHARS_OFFSET, STRING_FLAGS_OFFSET, STRING_INLINE_CHARS_BIT, STRING_LATIN1_CHARS_BIT,
    STRING_LENGTH_OFFSET, STRING_LINEAR_BIT, STRING_MAX_LENGTH, TA_DATA_PAYLOAD_OFFSET,
    TA_LENGTH_PAYLOAD_OFFSET, VAL_GCTHING_TAG_MIN, ZONE_NEEDS_BARRIER_OFFSET,
};
use crate::wasm::translate::{
    AtomTable, Helpers, APPEND_CACHE_ENTRY_BYTES, APPEND_CACHE_SIZE, BC_ARR_POP, BC_ARR_PUSH,
    BC_MAP_GET, BC_REGEXP_EXEC, BC_REGEXP_TEST, ELEMENTS_POP_BAIL_MASK, INLINE_IC_STRIDE,
    MAGIC_GENERATOR_CLOSING, MAGIC_IS_CONSTRUCTING, MAGIC_UNINITIALIZED_LEXICAL, TAG_BIGINT_HI,
    TAG_BOOLEAN, TAG_CLEAR, TAG_INT32, TAG_MAGIC, TAG_NULL, TAG_OBJECT, TAG_STRING, TAG_SYMBOL,
    TAG_UNDEFINED,
};

mod boxing;
mod calls;
mod construct;
mod elements;
mod emit;
mod exits;
mod flow;
mod globals;
mod guards;
mod inst_access;
mod inst_calls;
mod inst_construct;
mod inst_control;
mod inst_env;
mod inst_fields;
mod inst_guards;
mod inst_runtime;
mod inst_values;
mod iteration;
mod numeric;
mod props;
mod rooting;
mod stamps;
mod strings;
mod values;

type R<T> = Result<T, String>;

/// The most arguments past the first an inline `push` stores (each costs a
/// store and a post-barrier test).
const PUSH_ARM_MAX_EXTRA: usize = 7;

const UNDEF: u64 = TAG_UNDEFINED << 32;

/// The tags of values that are no GC thing (no barrier owes them anything).
const GC_FREE_TAGS: TagSet = TagSet::prims(crate::opsem::Prims::from_bits(
    crate::opsem::NUM.bits()
        | crate::opsem::PRIM_BOOLEAN.bits()
        | crate::opsem::PRIM_NULL.bits()
        | crate::opsem::PRIM_UNDEFINED.bits(),
));

/// `JS::GenericNaN()`'s bits.
const CANONICAL_NAN_BITS: u64 = 0x7FF8_0000_0000_0000;

/// How much of an inline frame's fixed slots (env, arguments object,
/// new.target, rval, resume word, backoff) its `inline.enter` writes.
/// Only an exit into the callee's baseline body reads the rest, so the
/// exit hub writes what the entry left out, and the frame's GC scan (`frame_top`) stops below it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fixed {
    /// All six: the callee reads its arguments object or new.target from
    /// the frame, or it is a construct.
    All,
    /// The env slot only: the callee sets its environment (`EnvSet`,
    /// `EnvPop`).
    Env,
    /// None: the callee's environment is its function's, read from the
    /// callee where used.
    None,
}

/// A lowered MIR function.
pub struct Lowered {
    pub body: FunctionBody,
    /// Adapter-offset placeholders of direct calls.
    pub body_off_patches: Vec<Value>,
    /// `Call` placeholders for the script's baseline body, one per exit.
    pub baseline_calls: Vec<Value>,
    /// Property-IC way-address placeholders, with their row offsets.
    pub prop_ic_patches: Vec<(Value, u32)>,
    /// `instanceof` cell placeholders, with their rows (+1).
    pub iof_cell_patches: Vec<(Value, u32)>,
    pub construct_cell_patches: Vec<(Value, u32)>,
    pub call_cell_patches: Vec<(Value, u32)>,
    pub alloc_cell_patches: Vec<(Value, u32)>,
    pub intrinsic_cell_patches: Vec<(Value, u32)>,
    /// Likely-callee direct calls: the expected-funcidx const, the stub
    /// `call`, and the callee's script id (`wasm/mod.rs` patches both).
    pub likely_patches: Vec<(Value, Value, u32)>,
    /// Per mnemonic: how many instructions, and the wasm values they
    /// lowered to (`--dump-opsize`).
    pub opsize: BTreeMap<String, (u32, u32)>,
    /// The body as text, each value marked with the MIR instruction that
    /// emitted it (`;;@i<n>`, n = the instruction's index): `--viz` only.
    pub viz_text: Option<String>,
}

/// `Lowered::viz_text`'s marks: from a value index on, which MIR
/// instruction (its index + 1; 0 for the entry, edges and hubs) emitted
/// the values.
struct VizMarks(Vec<(usize, u32)>);

impl waffle::PrintDecorator for VizMarks {
    fn after_inst(&self, value: Value, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let i = value.index();
        match self.0.partition_point(|&(s, _)| s <= i) {
            0 => Ok(()),
            k if self.0[k - 1].1 == 0 => Ok(()),
            k => write!(f, " ;;@i{}", self.0[k - 1].1 - 1),
        }
    }
}

/// The `math_natives_base` slot of a Math native the builder names
/// (`Math.<fn>`).
pub(crate) fn native_math_index(name: &str) -> Option<u32> {
    use crate::wasm::translate::{
        MN_ABS, MN_CEIL, MN_COS, MN_FLOOR, MN_FROUND, MN_MAX, MN_MIN, MN_POW, MN_SIN, MN_SQRT,
        MN_TRUNC,
    };
    Some(match name {
        "Math.abs" => MN_ABS,
        "Math.floor" => MN_FLOOR,
        "Math.ceil" => MN_CEIL,
        "Math.trunc" => MN_TRUNC,
        "Math.sqrt" => MN_SQRT,
        "Math.fround" => MN_FROUND,
        "Math.min" => MN_MIN,
        "Math.max" => MN_MAX,
        "Math.pow" => MN_POW,
        "Math.sin" => MN_SIN,
        "Math.cos" => MN_COS,
        _ => return None,
    })
}

/// Whether `op`'s lowering reaches a may-GC call only off its expected
/// path: an inline arm handles the common case, and the helper behind it
/// (an IC miss, growth, a slow allocation, a generic numeric operand) or
/// the op itself (an exit, boxing a primitive `this`) is the exception. A
/// value live across a loop that GCs only there stays in a register on
/// the loop's back edges; the exceptional edge reloads it (`conform`).
/// Only a placement choice: a wrong answer costs reloads, never
/// correctness.
fn gc_only_off_path(op: &Opcode) -> bool {
    matches!(
        op,
        Opcode::StoreElem(..)
            | Opcode::InitField(_)
            | Opcode::GetElemData
            | Opcode::SetElemData(_)
            | Opcode::GetPropData(_)
            | Opcode::SetPropData(_)
            | Opcode::Prim(_)
            | Opcode::NewThis(..)
            | Opcode::NewThisInit { .. }
            | Opcode::ExitInline { .. }
            | Opcode::JsBoxThis
    )
}

/// A generator's resume dispatch over at most this many yields is a chain
/// of compares; past it, a `br_table`.
const RESUME_CHAIN_MAX: usize = 4;

/// The resume words `f`'s exits and throws carry: the set the baseline
/// body must accept (`docs/BASELINE.md` §4).
pub fn resume_words(f: &mir::Func) -> Vec<ResumeWord> {
    let mut out = BTreeSet::new();
    for (_, d) in f.insts.iter() {
        let mode = match d.op {
            Opcode::Exit { .. } => ResumeMode::Continue,
            Opcode::ExitThrow { .. } => ResumeMode::Throw,
            _ => continue,
        };
        let (pc, _, _) = d.op.exit_shape().unwrap();
        out.insert(ResumeWord { pc, mode });
    }
    out.into_iter().collect()
}

/// The tags a value of MIR type `t` may have, boxed.
fn value_tags(t: &MType) -> TagSet {
    match t {
        MType::Val(v) => v.tags,
        MType::Obj(_) => TagSet::OBJECT,
        MType::Str(_) => TagSet::prims(PRIM_STRING),
        MType::Bool => TagSet::BOOLEAN,
        MType::I32(_) => TagSet::INT32,
        MType::F64(_) | MType::Int(_) => TagSet::NUMBER,
        _ => TagSet::ALL,
    }
}

/// The resume words of the exits and throws reachable from `root`: where
/// an activation entered there can resume baseline.
pub fn resume_words_from(f: &mir::Func, root: mir::Block) -> Vec<ResumeWord> {
    let mut seen = BTreeSet::new();
    let mut work = vec![root];
    let mut out = BTreeSet::new();
    while let Some(b) = work.pop() {
        if !seen.insert(b) {
            continue;
        }
        work.extend(f.succs(b));
        if let Some(t) = f.terminator(b) {
            let op = f.insts[t].op;
            if let Some((pc, _, _)) = op.exit_shape() {
                let mode = if matches!(op, Opcode::Exit { .. }) {
                    ResumeMode::Continue
                } else {
                    ResumeMode::Throw
                };
                out.insert(ResumeWord { pc, mode });
            }
        }
    }
    out.into_iter().collect()
}

/// The waffle type a MIR type lowers to (`None` for a ghost).
fn machine(t: &MType) -> Option<Type> {
    match t.repr().machine() {
        Machine::I32 => Some(Type::I32),
        Machine::I64 => Some(Type::I64),
        Machine::F64 => Some(Type::F64),
        Machine::None => None,
    }
}

/// An exit operand's representation, as far as boxing it goes: exits
/// whose operands agree on these share an exit hub.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum BoxKind {
    Val,
    I32,
    Int,
    F64,
    Bool,
    Obj,
    Str,
}

impl BoxKind {
    fn of(t: &MType) -> R<BoxKind> {
        Ok(match t {
            MType::Val(_) => BoxKind::Val,
            MType::I32(_) => BoxKind::I32,
            MType::Int(_) => BoxKind::Int,
            MType::F64(_) => BoxKind::F64,
            MType::Bool => BoxKind::Bool,
            MType::Obj(_) => BoxKind::Obj,
            MType::Str(_) => BoxKind::Str,
            t => return Err(format!("exit: cannot box {}", mir::print::type_str(t))),
        })
    }
}

fn is_managed(t: &MType) -> bool {
    // By representation, not by `may_hold_gc_thing`: rooting a boxed
    // int32 too keeps overwriting home slots whose last GC thing is dead,
    // and without it a dead object stays pinned there across a GC
    // (gc/weak-marking-01.js).
    t.repr().is_managed()
}

struct Lower<'a> {
    h: Helpers,
    f: &'a mir::Func,
    layout: FrameLayout,
    body: FunctionBody,
    cur: Block,
    cx: Value,
    sp: Value,
    /// The base of the frame's variable region (locals, fixed slots,
    /// operands) and of everything MIR places above it: `sp`, or past the
    /// actuals beyond the formals for a script that reads its actuals
    /// (`FrameLayout::rebase_vp`).
    vp: Value,
    /// `argc` without its flag bits.
    argc: Value,
    retval_out: Value,
    new_target: Value,
    blocks: BTreeMap<mir::Block, Block>,
    live_in: BTreeMap<mir::Block, BTreeSet<mir::Value>>,
    /// The waffle value standing for each MIR value at the emission point:
    /// for a managed value, its register copy, if it has a valid one (a
    /// may-GC call invalidates them).
    vmap: BTreeMap<mir::Value, Value>,
    /// Rooting (see the module doc): each managed value that may be live
    /// across a may-GC call has a home slot in the rooting area, which
    /// starts at `root_base` and holds `nslots` slots; `slotted` is the set
    /// of values whose home slot holds them at the emission point.
    root_base: u32,
    home: BTreeMap<mir::Value, u32>,
    nslots: u32,
    slotted: BTreeSet<mir::Value>,
    /// Home slots that may hold invalid bits: not written since entry
    /// (a dead frame's), or at or above the top of a may-GC call since
    /// (unscanned, so possibly stale, and possibly overwritten by the
    /// call's frame or out-slot). Those below the next may-GC call's top
    /// are cleared there, as its scan must see only valid Values. A slot
    /// written and scanned since stays valid (the GC updates it), so a
    /// value that dies is not cleared: it stays alive until its slot is
    /// reused, falls above a top, or the activation returns (§4.4).
    dirty: BTreeSet<u32>,
    /// The rooting slots a may-GC call scans, per strongly connected
    /// component of the CFG (by block): in a cycle, every frame-0 call
    /// scans the most any of its calls needs, so a loop's slots below
    /// that stay valid around it (`enter_block`). Absent: no cycle.
    scc_top: BTreeMap<mir::Block, u32>,
    /// Per inline frame inlined into the function's own frame (by frame
    /// id; 0 elsewhere): the rooting slots its region (it and the frames
    /// inlined into it) keeps -- one past the highest home of a value
    /// live anywhere in it. The frame sits right above them, over the
    /// rest, which nothing live in the region occupies.
    inline_k: Vec<u32>,
    /// The scan limit (offset from `sp`) of the last `root`: the top a
    /// may-GC call passes (`top_off`).
    gc_top: u32,
    /// Per (frame, slot): the value a `frame.store` put there, along the
    /// emission path; a store of the same value again is dropped.
    framed: BTreeMap<(u32, u32), mir::Value>,
    /// The builder's retaining frame stores before the next instruction:
    /// emitted where that instruction can GC (`root`), not before it.
    pending_retain: Vec<((u32, u32), mir::Value)>,
    /// `const.str`s whose only uses are `switch.str` cases: not emitted
    /// where they stand; the switch loads each atom where it compares it
    /// (a switch's cases would otherwise all load before its first).
    lazy_strs: BTreeMap<mir::Value, mir::entity::AtomId>,
    /// The MIR block being lowered.
    cur_mblock: mir::Block,
    /// Whether the stamp epoch was unchanged across the last `gc_call`.
    epoch_same: Option<Value>,
    /// Whether the emission point is on a helper's slow path: its edges'
    /// managed values are reloaded on the edge rather than at their uses.
    cold: bool,
    /// Edges into blocks not entered yet, and each entered block's entry
    /// state (`enter_block`).
    pending: BTreeMap<mir::Block, Vec<PendingEdge>>,
    plans: BTreeMap<mir::Block, Vec<(mir::Value, Loc, bool)>>,
    entry_dirty: BTreeMap<mir::Block, BTreeSet<u32>>,
    rpo_index: BTreeMap<mir::Block, usize>,
    preds: BTreeMap<mir::Block, Vec<mir::Block>>,
    /// Per block: the managed values that may have been live across a
    /// may-GC call since their definition, at its end.
    crossed_out: BTreeMap<mir::Block, BTreeSet<mir::Value>>,
    /// With inlined callees (§5.5): per frame id its base and end offsets
    /// from `sp` and its layout. Frame 0 is the function's, ending past
    /// the rooting area; each inline frame sits at its parent's end. A
    /// may-GC call's scan limit is its frame's top: the end of frame 0,
    /// or an inline frame's fixed slots (its operand stack is baseline's,
    /// written only by the frame's exits, so nothing in MIR reads it or
    /// lets the GC see it). A nested inline frame starts at its parent's
    /// top; an exit's baseline body runs on the frame up to its end.
    inline: bool,
    frame_off: Vec<u32>,
    /// Per frame: where its variable region (locals, fixed slots,
    /// operands) starts, from `vp`: its `frame_off`, past the actuals
    /// beyond its formals for a callee reading its actuals (baseline's
    /// `vp` rebase).
    frame_voff: Vec<u32>,
    frame_end: Vec<u32>,
    frame_top: Vec<u32>,
    /// Per frame: how much of an inline frame's fixed slots its
    /// `inline.enter` writes (and its GC scan covers); the exit hub writes
    /// the rest (§5.5).
    fixed: Vec<Fixed>,
    frame_layouts: Vec<FrameLayout>,
    /// The frame of the instruction being lowered.
    cur_frame: u32,
    baseline_calls: Vec<Value>,
    /// The stress mode's period (`Options::mir_stress`); 0 = off.
    stress: u32,
    /// While a `guard.layout` is lowered: its word, identity and keys, for
    /// the guard census's miss reason.
    guard_word: Option<(Value, Value, mir::types::KeyRange)>,
    guard_obj: Option<Value>,
    /// A `guard.script` chain's function test, for the next guard on the
    /// same callee, keyed by the block that guard is in (the first's
    /// `fail`, its only way in).
    script_memo: BTreeMap<mir::Block, (mir::Value, Value)>,
    /// Whether the function makes an arguments object (`args.object`):
    /// then its element reads try the `arguments[i]` arm (`args_element`).
    makes_args: bool,
    /// A `check.binding`'s fact, with its value-fuse test (`load_gname`'s).
    binding_armed: BTreeMap<mir::Value, Value>,
    /// Whether the function has onramp roots, and (if so) the entry's
    /// test of `ARGC_ONRAMP_BIT`, which says how this activation began.
    has_onramps: bool,
    onramp_flag: Value,
    mm: &'a mir::Module,
    atoms: &'a mut AtomTable,
    /// Adapter-offset placeholders (`Outcome::Compiled::body_off_patches`).
    body_off_patches: Vec<Value>,
    /// Property-IC way-address placeholders (`Outcome::Compiled::prop_ic_patches`).
    prop_ic_patches: Vec<(Value, u32)>,
    /// `instanceof` cell placeholders (`Outcome::Compiled::iof_cell_patches`).
    iof_cell_patches: Vec<(Value, u32)>,
    /// Construct cell placeholders (one cell per site).
    construct_cell_patches: Vec<(Value, u32)>,
    /// Call value cell placeholders (one cell per site; 0: the trash row).
    call_cell_patches: Vec<(Value, u32)>,
    likely_patches: Vec<(Value, Value, u32)>,
    alloc_cell_patches: Vec<(Value, u32)>,
    intrinsic_cell_patches: Vec<(Value, u32)>,
    opsize: BTreeMap<String, (u32, u32)>,
    /// The census helper, when exits are counted (`--mir-exit-census`).
    exit_census: Option<Func>,
    /// The census helper, when blocks are counted (`--block-census`).
    block_census: Option<Func>,
    /// The census helper, when rooting stores are counted (`--root-census`).
    root_census: Option<Func>,
    /// The census helper, when Wasm blocks are counted (`--value-census`),
    /// and the emitter of each Wasm value range: (first value, tag).
    value_census: Option<Func>,
    value_tags: Vec<(usize, String)>,
    /// `--viz`: `VizMarks`, as they are recorded.
    viz_marks: Option<Vec<(usize, u32)>>,
    /// The instruction being lowered, for the root census's labels.
    cur_inst: Option<mir::Inst>,
    /// Per frame (0 the function's own, then the inline frames): whether
    /// any instruction in it or in a frame inlined into it may GC
    /// (`may_gc`; an `exit.inline` does). Computed on first use.
    frame_gc: Option<Vec<bool>>,
    /// The inline frame the current block's `inline.enter` entered, if any:
    /// the retaining stores pending at its terminator are for that callee.
    block_enters: Option<u32>,
    /// Retaining stores handed to a block's sole predecessor's successor:
    /// still pending on entry, written at its first GC point (`root`).
    carried_retain: BTreeMap<mir::Block, Vec<((u32, u32), mir::Value)>>,
    /// One exit hub per frame shape (`exit_hub`).
    exit_hubs: BTreeMap<Vec<Option<BoxKind>>, Block>,
    /// One `exit.inline` hub per inline frame: its entry and last blocks,
    /// its site-index param and call status, and each site's tail
    /// (`exit_inline`).
    inline_hubs: BTreeMap<u32, (Block, Block, Value, Value, Value, Vec<Block>)>,
    strict: bool,
    plain_env: bool,
    own_env: bool,
    forward_resume: bool,
    is_gen: bool,
    mapped_formals: bool,
    inline_layouts: Vec<FrameLayout>,
    /// The syntactic global binding (`TranslateCtx::syn_gnames`) each
    /// global name read names, for its inline arms.
    gname_bids: BTreeMap<mir::entity::AtomId, u32>,
    /// Each global name with a fused literal (`fused_gnames`), which an
    /// inline write must keep.
    gname_fused: BTreeMap<mir::entity::AtomId, crate::wasm::translate::FusedGname>,
    /// Each property name's predicted (layout stamp key, byte offset)
    /// pairs (`layout_addpred_in`), for the inline add arm.
    add_preds: BTreeMap<mir::entity::AtomId, Vec<(u32, u32)>>,
}

/// An edge into a block not entered yet: its own waffle block, filled in
/// when the block's entry state is decided, with the edge's explicit
/// args and where each managed live-in of the target is along it (its
/// register copy, if valid; whether its home slot holds it).
struct PendingEdge {
    tb: Block,
    args: Vec<Value>,
    snap: BTreeMap<mir::Value, (Option<Value>, bool)>,
    dirty: BTreeSet<u32>,
    framed: BTreeMap<(u32, u32), mir::Value>,
    cold: bool,
}

/// Where a managed live-in of a block is on entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Loc {
    /// In a register: a waffle param of the block.
    Param,
    /// In a register, the same waffle value along every edge.
    Direct(Value),
    /// Only in its home slot.
    Slot,
}

/// Per-script lowering choices besides the function itself.
#[derive(Clone, Default)]
pub struct LowerOpts {
    /// The stress mode's period (`Options::mir_stress`); 0 = off.
    pub stress: u32,
    /// Count every exit (`--mir-exit-census`).
    pub exit_census: bool,
    /// Count every MIR block's executions (`--block-census`).
    pub block_census: bool,
    /// Count every executed rooting site (`--root-census`).
    pub root_census: bool,
    /// Count every executed Wasm block, by the MIR ops in it
    /// (`--value-census`).
    pub value_census: bool,
    /// Keep the body's text, its values marked with their MIR
    /// instructions (`--viz`).
    pub viz: bool,
    /// Strict-mode code: a field store's generic fallback throws on failure.
    pub strict: bool,
    /// The activation's environment is its callee's (`baseline::env_is_plain`).
    pub plain_env: bool,
    /// The activation makes its own environment (a call object, a named
    /// lambda's scope) at entry, fixed from then on: `env_setup`.
    pub own_env: bool,
    /// The script may be inlined: its entry forwards a resume to its
    /// baseline body (§5.5).
    pub forward_resume: bool,
    /// A generator or async body: its entry also takes a generator's
    /// resume (`EnterNightResume`) to the yield's `Resume` root.
    pub is_gen: bool,
    /// The script's formals are a mapped `arguments`'s, which may write
    /// them behind MIR's back: their frame stores are never dropped.
    pub mapped_formals: bool,
    /// Each inline frame's callee layout (`FrameLayout::of`); empty for
    /// hand-written MIR, whose inline frames take the full layout.
    pub inline_layouts: Vec<FrameLayout>,
}

/// Lower `f` (a function of `mm`, whose baseline frame is `layout`) into a
/// new body of `m`.
/// The `const.str` results used only as `switch.str` cases (`Lower::lazy_strs`).
fn lazy_switch_strs(f: &mir::Func) -> BTreeMap<mir::Value, mir::entity::AtomId> {
    let mut strs: BTreeMap<mir::Value, mir::entity::AtomId> = BTreeMap::new();
    for &b in &f.layout {
        for &i in &f.blocks[b].insts {
            if let Opcode::ConstStr(a) = f.insts[i].op {
                strs.insert(f.insts[i].results[0], a);
            }
        }
    }
    for &b in &f.layout {
        for &i in &f.blocks[b].insts {
            let d = &f.insts[i];
            let case_uses = if matches!(d.op, Opcode::SwitchStr(_)) {
                1
            } else {
                usize::MAX
            };
            for (k, v) in d.args.iter().enumerate() {
                if k < case_uses {
                    strs.remove(v);
                }
            }
            for e in &d.succs {
                for a in &e.args {
                    if let EdgeArg::Value(v) = a {
                        strs.remove(v);
                    }
                }
            }
        }
    }
    for l in &f.loops {
        if let Some(e) = &l.entry {
            for v in &e.state {
                strs.remove(v);
            }
        }
    }
    strs
}

pub fn lower<'a>(
    m: &mut Module,
    h: Helpers,
    mm: &'a mir::Module,
    atoms: &'a mut AtomTable,
    f: &'a mir::Func,
    layout: FrameLayout,
    o: LowerOpts,
    gname_bids: BTreeMap<mir::entity::AtomId, u32>,
    gname_fused: BTreeMap<mir::entity::AtomId, crate::wasm::translate::FusedGname>,
    add_preds: BTreeMap<mir::entity::AtomId, Vec<(u32, u32)>>,
) -> R<Lowered> {
    let body = FunctionBody::new(m, h.night_abi_sig2);
    let entry = body.entry;
    let p = |i: usize| body.blocks[entry].params[i].1;
    let (cx, sp, argc, retval_out, new_target) = (p(0), p(1), p(2), p(3), p(5));
    // Rooting slots and callee frames go above baseline's fixed frame and
    // locals, which therefore always hold valid Values (a fresh entry
    // initializes them): an exit can leave a dead one as it is. They
    // overlap baseline's operand slots, which MIR never reads while it
    // runs (an onramp reads them before anything is spilled, an exit
    // writes every one below its depth), so those are not initialized:
    // a helper's GC scan stops at the spilled roots.
    let root_base = layout.operand(0);
    let mut l = Lower {
        h,
        f,
        layout,
        body,
        cur: entry,
        cx,
        sp,
        vp: sp,
        argc,
        retval_out,
        new_target,
        blocks: BTreeMap::new(),
        live_in: BTreeMap::new(),
        vmap: BTreeMap::new(),
        root_base,
        home: BTreeMap::new(),
        nslots: 0,
        slotted: BTreeSet::new(),
        scc_top: BTreeMap::new(),
        inline_k: vec![],
        gc_top: root_base,
        dirty: BTreeSet::new(),
        framed: BTreeMap::new(),
        pending_retain: vec![],
        lazy_strs: lazy_switch_strs(f),
        cur_mblock: mir::Block::from_u32(0),
        cold: false,
        epoch_same: None,
        pending: BTreeMap::new(),
        plans: BTreeMap::new(),
        entry_dirty: BTreeMap::new(),
        rpo_index: BTreeMap::new(),
        preds: BTreeMap::new(),
        crossed_out: BTreeMap::new(),
        inline: !f.inline_frames.is_empty(),
        frame_off: vec![0],
        frame_voff: vec![0],
        frame_end: vec![root_base],
        frame_top: vec![root_base],
        fixed: vec![Fixed::All],
        frame_layouts: vec![layout],
        cur_frame: 0,
        baseline_calls: vec![],
        stress: o.stress,
        guard_word: None,
        script_memo: BTreeMap::new(),
        makes_args: f.insts.iter().any(|(_, d)| d.op == Opcode::ArgsObject),
        binding_armed: BTreeMap::new(),
        guard_obj: None,
        has_onramps: f.roots.iter().any(|r| r.kind != RootKind::Entry),
        onramp_flag: argc,
        mm,
        atoms,
        body_off_patches: vec![],
        prop_ic_patches: vec![],
        iof_cell_patches: vec![],
        construct_cell_patches: vec![],
        call_cell_patches: vec![],
        likely_patches: vec![],
        alloc_cell_patches: vec![],
        intrinsic_cell_patches: vec![],
        opsize: BTreeMap::new(),
        exit_census: if o.exit_census { h.census } else { None },
        block_census: if o.block_census { h.census } else { None },
        root_census: if o.root_census { h.census } else { None },
        value_census: if o.value_census { h.census } else { None },
        value_tags: vec![],
        viz_marks: o.viz.then(Vec::new),
        cur_inst: None,
        frame_gc: None,
        block_enters: None,
        carried_retain: BTreeMap::new(),
        exit_hubs: BTreeMap::new(),
        inline_hubs: BTreeMap::new(),
        strict: o.strict,
        plain_env: o.plain_env,
        own_env: o.own_env,
        forward_resume: o.forward_resume,
        is_gen: o.is_gen,
        mapped_formals: o.mapped_formals,
        inline_layouts: o.inline_layouts.clone(),
        gname_bids,
        gname_fused,
        add_preds,
    };
    l.run()?;
    let viz_text = l.viz_marks.take().map(|marks| {
        let marks = VizMarks(marks);
        format!("{}", l.body.display_with_decorator("", None, Some(&marks)))
    });
    crate::wasm::pad_body(&mut l.body, u64::from(l.f.script.get()));
    Ok(Lowered {
        body: l.body,
        body_off_patches: l.body_off_patches,
        baseline_calls: l.baseline_calls,
        prop_ic_patches: l.prop_ic_patches,
        iof_cell_patches: l.iof_cell_patches,
        construct_cell_patches: l.construct_cell_patches,
        call_cell_patches: l.call_cell_patches,
        alloc_cell_patches: l.alloc_cell_patches,
        intrinsic_cell_patches: l.intrinsic_cell_patches,
        likely_patches: l.likely_patches,
        opsize: l.opsize,
        viz_text,
    })
}

impl<'a> Lower<'a> {
    // --- instructions ------------------------------------------------------------------

    fn def(&mut self, inst: mir::Inst, v: Value) {
        let r = self.f.insts[inst].results[0];
        self.vmap.insert(r, v);
    }

    fn inst(&mut self, inst: mir::Inst) -> R<()> {
        self.cur_frame = self.f.inst_frame[inst];
        let d = self.f.insts[inst].clone();
        self.epoch_same = None;
        if let Opcode::FrameStore(k) = d.op {
            let key = (self.cur_frame, k);
            let aliased =
                self.mapped_formals && self.cur_frame == 0 && k >= 1 && k <= self.layout.nargs;
            if !aliased {
                // A retaining store (the builder's `retain_locals`): held
                // for the next instruction's GC points.
                if self.framed.get(&key) != Some(&d.args[0]) {
                    self.pending_retain.push((key, d.args[0]));
                }
                return Ok(());
            }
        }
        let a = self.args(inst)?;
        if self.inst_values(inst, &d, &a)? {
            return Ok(());
        }
        if self.inst_control(inst, &d, &a)? {
            return Ok(());
        }
        if self.inst_guards(inst, &d, &a)? {
            return Ok(());
        }
        if self.inst_env(inst, &d, &a)? {
            return Ok(());
        }
        if self.inst_access(inst, &d, &a)? {
            return Ok(());
        }
        if self.inst_calls(inst, &d, &a)? {
            return Ok(());
        }
        if self.inst_runtime(inst, &d, &a)? {
            return Ok(());
        }
        if self.inst_construct(inst, &d, &a)? {
            return Ok(());
        }
        if self.inst_fields(inst, &d, &a)? {
            return Ok(());
        }
        Err("lowering: no arm lowers the op".into())
    }
}
