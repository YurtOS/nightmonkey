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
        let at = |i: usize| self.f.values[d.args[i]].ty;
        match d.op {
            Opcode::ConstVal(c) => {
                let bits = match c {
                    ConstVal::Undefined => UNDEF,
                    ConstVal::Null => TAG_NULL << 32,
                    ConstVal::Bool(b) => (TAG_BOOLEAN << 32) | u64::from(b),
                    ConstVal::Int32(n) => (TAG_INT32 << 32) | u64::from(n as u32),
                    ConstVal::Double(bits) => bits,
                    ConstVal::Uninitialized => (TAG_MAGIC << 32) | MAGIC_UNINITIALIZED_LEXICAL,
                    ConstVal::IsConstructing => (TAG_MAGIC << 32) | MAGIC_IS_CONSTRUCTING,
                    ConstVal::Dead => UNDEF,
                    ConstVal::Hole => {
                        (TAG_MAGIC << 32) | crate::wasm::translate::MAGIC_ELEMENTS_HOLE
                    }
                };
                let v = self.i64c(bits);
                self.def(inst, v);
            }
            Opcode::ConstI32(n) => {
                let v = self.i32c(n as u32);
                self.def(inst, v);
            }
            Opcode::ConstF64(bits) => {
                let v = self.f64c(bits);
                self.def(inst, v);
            }
            Opcode::ConstBool(b) => {
                let v = self.i32c(u32::from(b));
                self.def(inst, v);
            }
            Opcode::Box => {
                let v = self.boxed(&at(0), a[0])?;
                self.def(inst, v);
            }
            Opcode::BoxDouble => {
                let bits = self.un(Operator::I64ReinterpretF64, a[0], Type::I64);
                let nan = self.bin(Operator::F64Ne, a[0], a[0], Type::I32);
                let canon = self.i64c(CANONICAL_NAN_BITS);
                let v = self.select(Type::I64, canon, bits, nan);
                self.def(inst, v);
            }
            Opcode::Unbox(k) => {
                let v = match k {
                    UnboxKind::F64Num => self.to_f64(a[0]),
                    _ => self.un(Operator::I32WrapI64, a[0], Type::I32),
                };
                self.def(inst, v);
            }
            Opcode::Weaken => self.def(inst, a[0]),
            Opcode::I32ToInt => {
                let v = self.un(Operator::I64ExtendI32S, a[0], Type::I64);
                self.def(inst, v);
            }
            Opcode::I32ToF64 => {
                let v = self.un(Operator::F64ConvertI32S, a[0], Type::F64);
                self.def(inst, v);
            }
            Opcode::IntToF64 => {
                let v = self.un(Operator::F64ConvertI64S, a[0], Type::F64);
                self.def(inst, v);
            }
            Opcode::I32Wrap(op) => {
                let o = match op {
                    ArithOp::Add => Operator::I32Add,
                    ArithOp::Sub => Operator::I32Sub,
                    ArithOp::Mul => Operator::I32Mul,
                    // Its type rules out a zero divisor.
                    ArithOp::Rem => Operator::I32RemS,
                };
                let v = self.bin(o, a[0], a[1], Type::I32);
                self.def(inst, v);
            }
            Opcode::IntArith(op) => {
                let o = match op {
                    ArithOp::Add => Operator::I64Add,
                    ArithOp::Sub => Operator::I64Sub,
                    ArithOp::Mul => Operator::I64Mul,
                    ArithOp::Rem => Operator::I64RemS,
                };
                let v = self.bin(o, a[0], a[1], Type::I64);
                self.def(inst, v);
            }
            Opcode::F64Arith(op) => {
                let o = match op {
                    F64Op::Add => Operator::F64Add,
                    F64Op::Sub => Operator::F64Sub,
                    F64Op::Mul => Operator::F64Mul,
                    F64Op::Div => Operator::F64Div,
                    // Wasm has no fmod: the leaf helper (`js::NumberMod`).
                    F64Op::Mod => {
                        let v = self.call1(self.h.fmod, &[a[0], a[1]], Type::F64);
                        self.def(inst, v);
                        return Ok(());
                    }
                };
                let v = self.bin(o, a[0], a[1], Type::F64);
                self.def(inst, v);
            }
            Opcode::F64Neg => {
                let v = self.un(Operator::F64Neg, a[0], Type::F64);
                self.def(inst, v);
            }
            Opcode::I32Bit(op) => {
                let o = match op {
                    BitOp::And => Operator::I32And,
                    BitOp::Or => Operator::I32Or,
                    BitOp::Xor => Operator::I32Xor,
                    // Wasm masks shift counts to 5 bits, as JS does.
                    BitOp::Shl => Operator::I32Shl,
                    BitOp::Shr => Operator::I32ShrS,
                };
                let v = self.bin(o, a[0], a[1], Type::I32);
                self.def(inst, v);
            }
            Opcode::I32Ushr => {
                let r = self.bin(Operator::I32ShrU, a[0], a[1], Type::I32);
                let v = self.un(Operator::I64ExtendI32U, r, Type::I64);
                self.def(inst, v);
            }
            Opcode::Cmp(repr, cc) => {
                use Operator::*;
                let o = match (repr, cc) {
                    (NumRepr::I32, Cc::Eq) => I32Eq,
                    (NumRepr::I32, Cc::Ne) => I32Ne,
                    (NumRepr::I32, Cc::Lt) => I32LtS,
                    (NumRepr::I32, Cc::Le) => I32LeS,
                    (NumRepr::I32, Cc::Gt) => I32GtS,
                    (NumRepr::I32, Cc::Ge) => I32GeS,
                    (NumRepr::Int, Cc::Eq) => I64Eq,
                    (NumRepr::Int, Cc::Ne) => I64Ne,
                    (NumRepr::Int, Cc::Lt) => I64LtS,
                    (NumRepr::Int, Cc::Le) => I64LeS,
                    (NumRepr::Int, Cc::Gt) => I64GtS,
                    (NumRepr::Int, Cc::Ge) => I64GeS,
                    (NumRepr::F64, Cc::Eq) => F64Eq,
                    (NumRepr::F64, Cc::Ne) => F64Ne,
                    (NumRepr::F64, Cc::Lt) => F64Lt,
                    (NumRepr::F64, Cc::Le) => F64Le,
                    (NumRepr::F64, Cc::Gt) => F64Gt,
                    (NumRepr::F64, Cc::Ge) => F64Ge,
                };
                let v = self.bin(o, a[0], a[1], Type::I32);
                self.def(inst, v);
            }
            Opcode::Math(m) => {
                let v = match m {
                    MathFn::Abs => self.un(Operator::F64Abs, a[0], Type::F64),
                    MathFn::Floor => self.un(Operator::F64Floor, a[0], Type::F64),
                    MathFn::Ceil => self.un(Operator::F64Ceil, a[0], Type::F64),
                    MathFn::Trunc => self.un(Operator::F64Trunc, a[0], Type::F64),
                    MathFn::Sqrt => self.un(Operator::F64Sqrt, a[0], Type::F64),
                    MathFn::Fround => {
                        let f = self.un(Operator::F32DemoteF64, a[0], Type::F32);
                        self.un(Operator::F64PromoteF32, f, Type::F64)
                    }
                    // wasm's min/max are JS's: NaN wins, -0 < +0.
                    MathFn::Min => self.bin(Operator::F64Min, a[0], a[1], Type::F64),
                    MathFn::Max => self.bin(Operator::F64Max, a[0], a[1], Type::F64),
                    MathFn::Pow => self.call1(self.h.math_pow, &[a[0], a[1]], Type::F64),
                    MathFn::Sin | MathFn::Cos => {
                        let k = self.i32c(u32::from(m == MathFn::Cos));
                        self.call1(self.h.math_unary, &[k, a[0]], Type::F64)
                    }
                    _ => return Err(format!("lowering: math {m:?}")),
                };
                self.def(inst, v);
            }
            Opcode::CheckNative(n) => {
                // The callee's JSNative is the pristine one (a function
                // object's native sits in its env slot's place, as
                // `math_arms` reads it).
                let idx = native_math_index(&self.mm.natives[n].name)
                    .ok_or_else(|| format!("lowering: native {}", self.mm.natives[n].name))?;
                let (_, _, native) = self.classify_native(a[0]);
                let fail = self.body.add_block();
                self.check(native, fail);
                let cp = self.un(Operator::I32WrapI64, a[0], Type::I32);
                let nf = self.load_i32(cp, FUNC_ENV_SLOT_OFFSET);
                let c = self.i32c(self.h.math_natives_base + 4 * idx);
                let slot = self.load_i32(c, 0);
                let same = self.bin(Operator::I32Eq, nf, slot, Type::I32);
                let (ok_b, cont) = (self.body.add_block(), self.body.add_block());
                self.cond_br(same, Self::to(ok_b), Self::to(fail));
                self.cur = fail;
                self.terminate(Terminator::Br {
                    target: Self::to(cont),
                });
                self.cur = ok_b;
                let t = self.edge(inst, 0, &[])?;
                self.terminate(Terminator::Br { target: t });
                self.cur = cont;
                let e = self.edge(inst, 1, &[])?;
                self.terminate(Terminator::Br { target: e });
            }
            Opcode::ToInt32 => {
                let x = match at(0) {
                    MType::Int(_) => self.un(Operator::F64ConvertI64S, a[0], Type::F64),
                    _ => a[0],
                };
                let v = self.to_int32(x);
                self.def(inst, v);
            }
            Opcode::JsToBool => {
                let v = self.to_bool(a[0]);
                self.def(inst, v);
            }

            // --- terminators ---
            Opcode::Jump => {
                let t = self.edge(inst, 0, &[])?;
                self.terminate(Terminator::Br { target: t });
            }
            Opcode::Br => {
                let (t, e) = (self.edge(inst, 0, &[])?, self.edge(inst, 1, &[])?);
                self.cond_br(a[0], t, e);
            }
            Opcode::Switch(n) => {
                let mut targets = vec![];
                for k in 0..n as usize {
                    targets.push(self.edge(inst, k, &[])?);
                }
                let default = self.edge(inst, n as usize, &[])?;
                self.terminate(Terminator::Select {
                    value: a[0],
                    targets,
                    default,
                });
            }
            Opcode::SwitchStr(n) => self.switch_str(inst, &a, n)?,
            Opcode::StampFresh(w) => {
                let obj = self.un(Operator::I32WrapI64, a[0], Type::I32);
                let wv = self.i32c(w);
                self.store_i32(obj, OBJ_CLASS_IDX_OFFSET, wv);
            }
            Opcode::Restamp(i) => self.restamp(a[0], self.mm.restamps[i as usize]),
            Opcode::Return => {
                self.store_i64(self.retval_out, 0, a[0]);
                let z = self.i32c(0);
                self.ret(z);
            }
            Opcode::Unreachable => self.terminate(Terminator::Unreachable),
            Opcode::GenSuspend {
                index,
                nargs,
                nlocals,
                initial,
                ..
            } => {
                self.gen_suspend(inst, &a, index, nargs, nlocals, initial)?;
            }
            Opcode::IsGenClosing => {
                let m = self.i64c((TAG_MAGIC << 32) | MAGIC_GENERATOR_CLOSING);
                let r = self.bin(Operator::I64Eq, a[0], m, Type::I32);
                self.def(inst, r);
            }
            Opcode::Exit { pc, nargs, nlocals } | Opcode::ExitThrow { pc, nargs, nlocals } => {
                let mode = if matches!(d.op, Opcode::Exit { .. }) {
                    ResumeMode::Continue
                } else {
                    ResumeMode::Throw
                };
                let dead: Vec<bool> = d
                    .args
                    .iter()
                    .map(|&v| {
                        matches!(
                            self.f.values[v].def,
                            mir::func::ValueDef::Result(i, _)
                                if self.f.insts[i].op == Opcode::ConstVal(ConstVal::Dead)
                        )
                    })
                    .collect();
                let tys: Vec<MType> = d.args.iter().map(|&v| self.ty(v)).collect();
                self.exit(ResumeWord { pc, mode }, &a, &tys, &dead, nargs, nlocals)?;
            }
            Opcode::GuardUnbox(UnboxKind::F64Num) if self.plain_guard(inst) => {
                let tag = self.tag_of(a[0]);
                let have = value_tags(&self.ty(d.args[0]));
                self.unbox_num_edges(inst, a[0], tag, have)?;
            }
            Opcode::GuardUnbox(k) => {
                let v = a[0];
                let tag = self.tag_of(v);
                let (cond, out) = match k {
                    UnboxKind::I32 => {
                        let c = self.tag_is(tag, TAG_INT32 as u32);
                        (c, self.un(Operator::I32WrapI64, v, Type::I32))
                    }
                    UnboxKind::F64Num => {
                        let k = self.i32c(TAG_INT32 as u32);
                        let c = self.bin(Operator::I32LeU, tag, k, Type::I32);
                        (c, self.to_f64(v))
                    }
                    UnboxKind::Bool => {
                        let c = self.tag_is(tag, TAG_BOOLEAN as u32);
                        (c, self.un(Operator::I32WrapI64, v, Type::I32))
                    }
                    UnboxKind::Obj => {
                        let c = self.tag_is(tag, TAG_OBJECT as u32);
                        (c, self.un(Operator::I32WrapI64, v, Type::I32))
                    }
                    UnboxKind::Str => {
                        let c = self.tag_is(tag, TAG_STRING as u32);
                        (c, self.un(Operator::I32WrapI64, v, Type::I32))
                    }
                };
                self.guard(inst, cond, &[out])?;
            }
            Opcode::GuardTags(t) => {
                // Of the tags the input may have, test the fewer: those
                // in `t`, or those outside it.
                let have = match self.ty(d.args[0]) {
                    MType::Val(s) => s.tags,
                    _ => TagSet::ALL,
                };
                let (inside, outside) = (have.intersect(t), have.minus(t));
                let c = if outside.count() < inside.count() {
                    let o = self.has_tags(a[0], outside);
                    self.un(Operator::I32Eqz, o, Type::I32)
                } else {
                    self.has_tags(a[0], inside)
                };
                self.guard(inst, c, &[a[0]])?;
            }
            Opcode::F64ToIntExact => {
                let x = a[0];
                let i = self.un(Operator::I32TruncSatF64S, x, Type::I32);
                let back = self.un(Operator::F64ConvertI32S, i, Type::F64);
                let exact = self.bin(Operator::F64Eq, back, x, Type::I32);
                let bits = self.un(Operator::I64ReinterpretF64, x, Type::I64);
                let negz = self.i64c(1 << 63);
                let not_negz = self.bin(Operator::I64Ne, bits, negz, Type::I32);
                let ok = self.bin(Operator::I32And, exact, not_negz, Type::I32);
                self.guard(inst, ok, &[i])?;
            }
            Opcode::I32Ovf(op @ (ArithOp::Add | ArithOp::Sub))
                if self
                    .const_i32(d.args[1])
                    .or(if op == ArithOp::Add {
                        self.const_i32(d.args[0])
                    } else {
                        None
                    })
                    .is_some() =>
            {
                // `x + c` (either side) or `x - c`: overflow is `x` past
                // one bound, one compare rather than a widened sum.
                let (x, c) = match self.const_i32(d.args[1]) {
                    Some(c) => (a[0], c),
                    None => (a[1], self.const_i32(d.args[0]).unwrap()),
                };
                let c = if op == ArithOp::Sub { -c } else { c };
                let o = if op == ArithOp::Add {
                    Operator::I32Add
                } else {
                    Operator::I32Sub
                };
                let r = self.bin(o, a[0], a[1], Type::I32);
                let ok = if c >= 0 {
                    let bound = self.i32c((i64::from(i32::MAX) - c) as i32 as u32);
                    self.bin(Operator::I32LeS, x, bound, Type::I32)
                } else {
                    let bound = self.i32c((i64::from(i32::MIN) - c) as i32 as u32);
                    self.bin(Operator::I32GeS, x, bound, Type::I32)
                };
                self.guard(inst, ok, &[r])?;
            }
            Opcode::I32Ovf(op) => {
                let (x, y) = (a[0], a[1]);
                let (r, ok) = match op {
                    ArithOp::Add | ArithOp::Sub => {
                        let (o, wide) = if op == ArithOp::Add {
                            (Operator::I32Add, Operator::I64Add)
                        } else {
                            (Operator::I32Sub, Operator::I64Sub)
                        };
                        let r = self.bin(o, x, y, Type::I32);
                        let x64 = self.un(Operator::I64ExtendI32S, x, Type::I64);
                        let y64 = self.un(Operator::I64ExtendI32S, y, Type::I64);
                        let w = self.bin(wide, x64, y64, Type::I64);
                        let r64 = self.un(Operator::I64ExtendI32S, r, Type::I64);
                        (r, self.bin(Operator::I64Eq, w, r64, Type::I32))
                    }
                    ArithOp::Mul => {
                        // Branches, not one flag: the product fits, and
                        // only a zero one (rare) tests for -0 (a negative
                        // operand).
                        let x64 = self.un(Operator::I64ExtendI32S, x, Type::I64);
                        let y64 = self.un(Operator::I64ExtendI32S, y, Type::I64);
                        let w = self.bin(Operator::I64Mul, x64, y64, Type::I64);
                        let r = self.un(Operator::I32WrapI64, w, Type::I32);
                        let r64 = self.un(Operator::I64ExtendI32S, r, Type::I64);
                        let fits = self.bin(Operator::I64Eq, w, r64, Type::I32);
                        let (nz, zero, ok_b, fail_b) = (
                            self.body.add_block(),
                            self.body.add_block(),
                            self.body.add_block(),
                            self.body.add_block(),
                        );
                        self.cond_br(fits, Self::to(nz), Self::to(fail_b));
                        self.cur = nz;
                        self.cond_br(r, Self::to(ok_b), Self::to(zero));
                        self.cur = zero;
                        let xy = self.bin(Operator::I32Or, x, y, Type::I32);
                        let z = self.i32c(0);
                        let neg = self.bin(Operator::I32LtS, xy, z, Type::I32);
                        self.cond_br(neg, Self::to(fail_b), Self::to(ok_b));
                        self.cur = fail_b;
                        let f = self.edge(inst, 1, &[])?;
                        self.terminate(Terminator::Br { target: f });
                        self.cur = ok_b;
                        (r, self.i32c(1))
                    }
                    ArithOp::Rem => {
                        // A zero divisor fails (NaN), and so does a zero
                        // remainder of a negative dividend (-0). `rem_s`
                        // traps on 0, so it divides by 1 there instead;
                        // INT_MIN % -1 is 0 in Wasm, and fails as -0.
                        let z = self.i32c(0);
                        let nz = self.bin(Operator::I32Ne, y, z, Type::I32);
                        let one = self.i32c(1);
                        let d = self.select(Type::I32, y, one, nz);
                        let r = self.bin(Operator::I32RemS, x, d, Type::I32);
                        let neg = self.bin(Operator::I32LtS, x, z, Type::I32);
                        let rz = self.un(Operator::I32Eqz, r, Type::I32);
                        let negz = self.bin(Operator::I32And, neg, rz, Type::I32);
                        let pos = self.un(Operator::I32Eqz, negz, Type::I32);
                        (r, self.bin(Operator::I32And, nz, pos, Type::I32))
                    }
                };
                self.guard(inst, ok, &[r])?;
            }
            Opcode::Prim(p) => self.prim_op(inst, p, &a)?,
            Opcode::JsAdd
            | Opcode::JsBinop(_)
            | Opcode::JsUnop(_)
            | Opcode::JsCompare(_)
            | Opcode::JsToNumeric => {
                self.numeric_fast_arms(inst, &d.op, &a)?;
                if d.op == Opcode::JsAdd {
                    self.concat_arm(inst, &d, &a)?;
                }
                self.js_op(inst, &d.op, &a)?
            }
            Opcode::ArgsMapped(n) | Opcode::ArgsMappedSet(n) => {
                // The entry made the object (mapped scripts are not
                // inlined: the frame is the function's own).
                let obj = self.load_i64(self.vp, self.layout.args_obj());
                let i = self.i32c(n);
                if let Opcode::ArgsMapped(_) = d.op {
                    let v = self.call1(self.h.get_mapped_arg, &[obj, i], Type::I64);
                    self.def(inst, v);
                } else {
                    self.call(self.h.set_mapped_arg, &[obj, i, a[0]], &[]);
                }
            }
            Opcode::JsTypeofEq(k) => {
                let v = self.typeof_eq(a[0], k);
                self.def(inst, v);
            }
            Opcode::JsConstantStrictEq(k) => {
                // `ConstantStrictEqual` inline: the operand's type
                // byte, then its payload.
                let v = self.constant_strict_eq(a[0], k);
                self.def(inst, v);
            }
            // The activation's environment is fixed (§5.1): the frame's env
            // slot, which the fresh entry (or baseline, before an onramp)
            // set, and which is rooted with the frame.
            Opcode::GlobalObject => {
                let realm = self.load_i32(self.cx, JSCONTEXT_REALM_OFFSET);
                let global = self.load_i32(realm, REALM_GLOBAL_OFFSET);
                let g = self.box_tagged(TAG_OBJECT, global);
                self.def(inst, g);
            }
            Opcode::EnvCurrent => {
                let env = self.frame_env();
                let p = self.un(Operator::I32WrapI64, env, Type::I32);
                self.def(inst, p);
            }
            Opcode::EnvCallee(hops) => {
                let env = self.frame_env();
                let hv = self.i32c(hops);
                let r = self.call1(self.h.env_callee, &[self.cx, env, hv], Type::I64);
                self.def(inst, r);
            }
            Opcode::ObjectLit(index) => {
                let script = self.script_ptr();
                let iv = self.i32c(index);
                let r = self.call1(self.h.object, &[self.cx, script, iv], Type::I64);
                self.def(inst, r);
            }
            Opcode::EnvSet => {
                let off = self.frame_env_off();
                self.store_i64(self.vp, off, a[0]);
            }
            Opcode::EnvPop => {
                let env = self.frame_env();
                let p = self.un(Operator::I32WrapI64, env, Type::I32);
                let up = self.load_i64(p, FIXED_SLOTS_BASE);
                let off = self.frame_env_off();
                self.store_i64(self.vp, off, up);
            }
            // `EnvironmentObject::ENCLOSING_ENV_SLOT`, always fixed.
            Opcode::EnvParent => {
                let env = self.load_i64(a[0], FIXED_SLOTS_BASE);
                let p = self.un(Operator::I32WrapI64, env, Type::I32);
                self.def(inst, p);
            }
            Opcode::EnvLoad(slot) => {
                let (addr, off) = self.env_slot(a[0], slot.get());
                let v = self.load_i64(addr, off);
                self.def(inst, v);
            }
            Opcode::EnvStore(slot) => {
                // `setAliasedBinding` zero hops up, inline: the slot store
                // with its barriers.
                let (addr, off) = self.env_slot(a[0], slot.get());
                self.pre_barrier(addr, off);
                self.store_i64(addr, off, a[1]);
                let s = self.i32c(slot.get());
                self.post_barrier(self.h.post_write_barrier, a[0], s, a[1]);
            }
            Opcode::FrameStore(k) => {
                let fid = self.cur_frame as usize;
                let l = self.frame_layouts[fid];
                let (nargs, nlocals) = (l.nargs, l.nlocals);
                let off = match k {
                    0 => self.frame_off[fid] + FrameLayout::THIS,
                    k if k <= nargs => self.frame_off[fid] + l.arg(k - 1),
                    k if k <= nargs + nlocals => self.frame_voff[fid] + l.local(k - 1 - nargs),
                    _ => self.frame_voff[fid] + l.rval(),
                };
                // The Value it is. A double goes in as a double (NaN made
                // canonical, as any boxed double must be), not re-tagged
                // as an int32: `box_number` is not needed for validity.
                let v = match at(0) {
                    MType::F64(_) => {
                        let bits = self.un(Operator::I64ReinterpretF64, a[0], Type::I64);
                        let nan = self.bin(Operator::F64Ne, a[0], a[0], Type::I32);
                        let canon = self.i64c(CANONICAL_NAN_BITS);
                        self.select(Type::I64, canon, bits, nan)
                    }
                    t => self.boxed(&t, a[0])?,
                };
                // `this` and the formals sit below the actuals (`sp`); the
                // rest of the function's frame, and inline frames, at `vp`.
                let base = if fid == 0 && k <= nargs {
                    self.sp
                } else {
                    self.vp
                };
                self.store_i64(base, off, v);
            }
            Opcode::LitNew(n) => {
                // As `js.rt.newobject`: the inline nursery allocation from
                // the site's cell, else the helper (which fills the cell).
                let cell = self.alloc_inline(inst, None)?;
                let nv = self.i32c(n);
                let live = self.live_across(inst);
                let (ok, r) = self.gc_call(self.h.new_object, &[cell, nv], &live)?;
                let t = self.edge(inst, 0, &[r])?;
                let e = self.edge(inst, 1, &[])?;
                self.cond_br(ok, t, e);
            }
            Opcode::LitInit(name, _) => {
                // As `js.rt.initprop`: the site's add transition replayed
                // inline, else the helper (which fills the row).
                let site = self.init_prop_inline(inst, a[0], a[1])?;
                let at = self.atom(name);
                let av = self.i32c(crate::wasm::mir::abi::INIT_ATTR_ENUMERATE);
                let sv = self.i32c(site);
                let live = self.live_across(inst);
                let (ok, _) = self.gc_call(self.h.init_prop, &[a[0], at, a[1], av, sv], &live)?;
                let t = self.edge(inst, 0, &[])?;
                let e = self.edge(inst, 1, &[])?;
                self.cond_br(ok, t, e);
            }
            Opcode::JsLambda(index) => {
                // Inline from the site's row: no GC, so the ok edge keeps
                // everything in registers. The helper (which fills the
                // row), and the rooting, only on the slow path.
                let env = self.box_tagged(TAG_OBJECT, a[0]);
                let cell = self.i32c(CONSTRUCT_CELL_ADDR_PLACEHOLDER);
                let idx = self.atoms.next_construct_cell();
                self.construct_cell_patches.push((cell, idx + 1));
                let slow = self.body.add_block();
                let r = self.lambda_inline(cell, env, slow);
                let t = self.edge(inst, 0, &[r])?;
                self.terminate(Terminator::Br { target: t });
                self.cur = slow;
                let script = self.script_ptr();
                let i = self.i32c(index);
                let live = self.live_across(inst);
                let (ok, r) = self.gc_call(self.h.lambda, &[env, script, i, cell], &live)?;
                let t = self.edge(inst, 0, &[r])?;
                let e = self.edge(inst, 1, &[])?;
                self.cond_br(ok, t, e);
            }
            Opcode::JsGetName(name) => {
                // A getter may run: clean only if no class word was
                // demoted meanwhile.
                if let Some(&bid) = self.gname_bids.get(&name) {
                    self.gname_fast_arms(inst, bid)?;
                }
                let at = self.atom(name);
                let z = self.i32c(0);
                self.js_call(inst, self.h.get_gname, &[at, z], false)?;
            }
            Opcode::JsGetProp(name) | Opcode::GetPropData(name) => {
                // The site's inline cache (a fact-free read): the
                // shared probe `night_ic_get` (own and holder ways, then
                // the megamorphic table) takes `ok_clean` on a hit; a miss
                // runs the generic get and fills the site's ways. For
                // `getprop.data` the same arms take `ok`, and a miss asks
                // the runtime's pure lookup, failing where it would run
                // code.
                let cache = self.atoms.next_prop_cache();
                let way_base = self.ic_addr(cache * INLINE_IC_STRIDE);
                let nm = String::from_utf16_lossy(self.mm.atoms[name].chars());
                if nm == "charCodeAt" || nm == "charAt" {
                    // A string's pristine char method, while String.prototype
                    // is untouched: the cached native.
                    let cell = if nm == "charCodeAt" {
                        self.h.str_ccat_cell
                    } else {
                        self.h.str_cat_cell
                    };
                    let ic = self.body.add_block();
                    let tag = self.tag_of(a[0]);
                    let is_str = self.tag_is(tag, TAG_STRING as u32);
                    let fslot = self.i32c(self.h.str_fuse_addr_slot);
                    let faddr = self.load_i32(fslot, 0);
                    let fword = self.load_i32(faddr, 0);
                    let intact = self.un(Operator::I32Eqz, fword, Type::I32);
                    let c = self.i32c(cell);
                    let bits = self.load_i64(c, 0);
                    let z = self.i64c(0);
                    let armed = self.bin(Operator::I64Ne, bits, z, Type::I32);
                    let ok = self.bin(Operator::I32And, is_str, intact, Type::I32);
                    let ok = self.bin(Operator::I32And, ok, armed, Type::I32);
                    let hit = self.body.add_block();
                    self.cond_br(ok, Self::to(hit), Self::to(ic));
                    self.cur = hit;
                    let t = self.edge(inst, 0, &[bits])?;
                    self.terminate(Terminator::Br { target: t });
                    self.cur = ic;
                }
                if self.mm.atoms[name].chars()
                    == "length".encode_utf16().collect::<Vec<u16>>().as_slice()
                {
                    // Not a slot: a string's or an array's own word.
                    let ic = self.body.add_block();
                    self.length_arms(inst, a[0], ic)?;
                    self.cur = ic;
                }
                if let Opcode::GetPropData(_) = d.op {
                    self.get_ic_pure(inst, name, a[0], way_base, cache)?;
                } else {
                    self.get_ic(inst, name, a[0], way_base, cache)?;
                }
            }
            Opcode::JsSetProp(name, strict) => self.set_ic(inst, name, a[0], a[1], strict)?,
            Opcode::SetPropData(name) => self.set_ic_pure(inst, name, a[0], a[1])?,
            Opcode::JsGetElem | Opcode::GetElemData => {
                // An in-bounds, non-hole dense element of a native object
                // inline (as baseline does), taking `ok_clean` (`ok`);
                // everything else through the helper (for `getelem.data`,
                // its pure read, failing where the read would run code).
                let slow = self.body.add_block();
                let v = self.dense_element(a[0], a[1], slow);
                let t = self.edge(inst, 0, &[v])?;
                self.terminate(Terminator::Br { target: t });
                self.cur = slow;
                // The other pure arms, where the operands' types admit
                // them: a string's char, and a string key's megamorphic
                // probe (`obj[name]`).
                let (rt, kt) = (
                    value_tags(&self.ty(d.args[0])),
                    value_tags(&self.ty(d.args[1])),
                );
                if !rt.intersect(TagSet::prims(PRIM_STRING)).is_empty()
                    && !kt.intersect(TagSet::INT32).is_empty()
                {
                    let other = self.body.add_block();
                    let c = self.string_char(a[0], a[1], other);
                    let t = self.edge(inst, 0, &[c])?;
                    self.terminate(Terminator::Br { target: t });
                    self.cur = other;
                }
                if !rt.intersect(TagSet::OBJECT).is_empty()
                    && !kt.intersect(TagSet::prims(PRIM_STRING)).is_empty()
                {
                    let other = self.body.add_block();
                    let tag = self.tag_of(a[0]);
                    let is_obj = self.tag_is(tag, TAG_OBJECT as u32);
                    let ktag = self.tag_of(a[1]);
                    let is_str = self.tag_is(ktag, TAG_STRING as u32);
                    let both = self.bin(Operator::I32And, is_obj, is_str, Type::I32);
                    self.check(both, other);
                    let obj = self.un(Operator::I32WrapI64, a[0], Type::I32);
                    let r = self.call1(self.h.elem_mega_get, &[obj, a[1]], Type::I64);
                    let rtag = self.tag_of(r);
                    let miss = self.tag_is(rtag, TAG_MAGIC as u32);
                    let hit = self.body.add_block();
                    self.cond_br(miss, Self::to(other), Self::to(hit));
                    self.cur = hit;
                    let t = self.edge(inst, 0, &[r])?;
                    self.terminate(Terminator::Br { target: t });
                    self.cur = other;
                }
                // An arguments object's element (`args_element`),
                // in a function that makes one.
                if self.makes_args
                    && !rt.intersect(TagSet::OBJECT).is_empty()
                    && !kt.intersect(TagSet::INT32).is_empty()
                {
                    let other = self.body.add_block();
                    let v = self.args_element(a[0], a[1], other);
                    let t = self.edge(inst, 0, &[v])?;
                    self.terminate(Terminator::Br { target: t });
                    self.cur = other;
                }
                if self.ta_poly(inst) {
                    // A typed array of any kind: the
                    // probe's value, or the magic tag on a miss.
                    let generic = self.body.add_block();
                    let (obj, idx) = self.obj_int(a[0], a[1], generic);
                    let r = self.call1(self.h.ta_get_poly, &[obj, idx], Type::I64);
                    let tag = self.tag_of(r);
                    let miss = self.tag_is(tag, TAG_MAGIC as u32);
                    let hit = self.body.add_block();
                    self.cond_br(miss, Self::to(generic), Self::to(hit));
                    self.cur = hit;
                    let t = self.edge(inst, 0, &[r])?;
                    self.terminate(Terminator::Br { target: t });
                    self.cur = generic;
                }
                if d.op == Opcode::GetElemData {
                    self.slow_census(inst);
                    let live = self.live_across(inst);
                    let (r, v) = self.gc_call(self.h.get_elem_pure, &[a[0], a[1]], &live)?;
                    self.epoch_same = None;
                    let one = self.i32c(1);
                    let ok = self.bin(Operator::I32Eq, r, one, Type::I32);
                    let t = self.edge(inst, 0, &[v])?;
                    let other = self.body.add_block();
                    self.cond_br(ok, t, Self::to(other));
                    self.cur = other;
                    let none = self.un(Operator::I32Eqz, r, Type::I32);
                    let f = self.edge(inst, 1, &[])?;
                    let e = self.edge(inst, 2, &[])?;
                    self.cond_br(none, f, e);
                } else {
                    self.js_call(inst, self.h.get_element, &[a[0], a[1]], false)?;
                }
            }
            Opcode::JsSetElem(_, duty) | Opcode::SetElemData(duty) => {
                // An in-bounds overwrite of a non-hole dense element inline,
                // taking `ok_clean`: an own writable data property unless
                // the elements are frozen. The store bypasses the engine,
                // so it owes the array stamp's RANGES claim its duty: a
                // clear for a value not proven inside the claim
                // (no MIR fact rests on it; the epoch bump tells callers').
                let num =
                    matches!(self.ty(d.args[2]), MType::Val(s) if s.tags.subset_of(TagSet::NUMBER));
                let slow = self.body.add_block();
                let (obj, elements, idx, addr, _) = self.dense_slot(a[0], a[1], slow);
                let back = self.i32c(ELEMENTS_FLAGS_BACK);
                let header = self.bin(Operator::I32Sub, elements, back, Type::I32);
                let flags = self.load_i32(header, 0);
                let fz = self.i32c(ELEMENTS_FROZEN_FLAG);
                let frozen = self.bin(Operator::I32And, flags, fz, Type::I32);
                let good = self.un(Operator::I32Eqz, frozen, Type::I32);
                self.check(good, slow);
                if duty {
                    self.elem_duty(obj);
                }
                if !num {
                    self.pre_barrier(addr, 0);
                }
                self.store_i64(addr, 0, a[2]);
                if !num {
                    let f = self.h.post_write_barrier_elem;
                    self.post_barrier(f, obj, idx, a[2]);
                }
                let t = self.edge(inst, 0, &[])?;
                self.terminate(Terminator::Br { target: t });
                self.cur = slow;
                {
                    let generic = self.body.add_block();
                    self.elem_append_arm(inst, a[0], a[1], a[2], duty, num, generic)?;
                    self.cur = generic;
                }
                if self.ta_poly(inst) {
                    // A typed array of any kind (the poly-TA store probe).
                    let generic = self.body.add_block();
                    let (obj, idx) = self.obj_int(a[0], a[1], generic);
                    let ok = self.call1(self.h.ta_set_poly, &[obj, idx, a[2]], Type::I32);
                    let hit = self.body.add_block();
                    self.cond_br(ok, Self::to(hit), Self::to(generic));
                    self.cur = hit;
                    let t = self.edge(inst, 0, &[])?;
                    self.terminate(Terminator::Br { target: t });
                    self.cur = generic;
                }
                // Only where the key may be a string: the arm at every
                // element store cost mandreel 5.6% of its code (-3.9%).
                let key_str = !value_tags(&self.ty(d.args[1]))
                    .intersect(TagSet::STRING)
                    .is_empty();
                if key_str {
                    let generic = self.body.add_block();
                    self.elem_add_arm(inst, a[0], a[1], a[2], generic)?;
                    self.cur = generic;
                }
                if let Opcode::JsSetElem(strict, _) = d.op {
                    let sv = self.i32c(u32::from(strict));
                    self.js_call(inst, self.h.set_element, &[a[0], a[1], a[2], sv], false)?;
                } else {
                    // `setelem.data`: the runtime's set, as `set_ic_pure`'s.
                    self.slow_census(inst);
                    let live = self.live_across(inst);
                    let (r, _) = self.gc_call(self.h.set_elem_pure, &[a[0], a[1], a[2]], &live)?;
                    self.epoch_same = None;
                    self.store_codes(inst, r)?;
                }
            }
            Opcode::LengthArray => {
                // The elements header's length word, unsigned.
                let elements = self.load_i32(a[0], OBJ_ELEMENTS_OFFSET);
                let back = self.i32c(ELEMENTS_LENGTH_BACK);
                let la = self.bin(Operator::I32Sub, elements, back, Type::I32);
                let len = self.load_i32(la, 0);
                let r = self.un(Operator::I64ExtendI32U, len, Type::I64);
                self.def(inst, r);
            }
            Opcode::LengthString => {
                let len = self.load_i32(a[0], STRING_LENGTH_OFFSET);
                self.def(inst, len);
            }
            Opcode::GuardRange(r) => {
                // One unsigned compare: `x - lo <=u hi - lo`.
                let x = a[0];
                let lo = self.i32c(r.lo as i32 as u32);
                let d = self.bin(Operator::I32Sub, x, lo, Type::I32);
                let span = self.i32c((r.hi - r.lo) as u32);
                let ok = self.bin(Operator::I32LeU, d, span, Type::I32);
                self.guard(inst, ok, &[x])?;
            }
            Opcode::IntToI32 => {
                let x = a[0];
                let lo = self.i64c(i64::from(i32::MIN) as u64);
                let hi = self.i64c(i64::from(i32::MAX) as u64);
                let ge = self.bin(Operator::I64GeS, x, lo, Type::I32);
                let le = self.bin(Operator::I64LeS, x, hi, Type::I32);
                let ok = self.bin(Operator::I32And, ge, le, Type::I32);
                let i = self.un(Operator::I32WrapI64, x, Type::I32);
                self.guard(inst, ok, &[i])?;
            }
            Opcode::GuardKind(ObjKind::Array) => {
                let shape = self.load_i32(a[0], SHAPE_OFFSET);
                let base = self.load_i32(shape, SHAPE_BASESHAPE_OFFSET);
                let clasp = self.load_i32(base, BASESHAPE_CLASP_OFFSET);
                let aslot = self.i32c(self.h.array_class_slot);
                let arr_class = self.load_i32(aslot, 0);
                let is_arr = self.bin(Operator::I32Eq, clasp, arr_class, Type::I32);
                self.guard(inst, is_arr, &[a[0]])?;
            }
            Opcode::LengthTa => {
                // Its length slot's payload (a detached one reads 0).
                let len = self.load_i32(a[0], TA_LENGTH_PAYLOAD_OFFSET);
                self.def(inst, len);
            }
            Opcode::GuardKind(ObjKind::Plain) => {
                let shape = self.load_i32(a[0], SHAPE_OFFSET);
                let base = self.load_i32(shape, SHAPE_BASESHAPE_OFFSET);
                let clasp = self.load_i32(base, BASESHAPE_CLASP_OFFSET);
                let pslot =
                    self.i32c(self.h.strlit_slot + crate::region_shape::STRLIT_PLAIN_CLASS_OFF);
                let plain_class = self.load_i32(pslot, 0);
                let is_plain = self.bin(Operator::I32Eq, clasp, plain_class, Type::I32);
                self.guard(inst, is_plain, &[a[0]])?;
            }
            Opcode::GuardKind(ObjKind::Native) => {
                let shape = self.load_i32(a[0], SHAPE_OFFSET);
                let flags = self.load_i32(shape, SHAPE_IMMUTABLE_FLAGS_OFFSET);
                let bit = self.i32c(SHAPE_IS_NATIVE_BIT);
                let native = self.bin(Operator::I32And, flags, bit, Type::I32);
                self.guard(inst, native, &[a[0]])?;
            }
            Opcode::GuardKind(ObjKind::TypedArray(k)) => {
                // The class is the kind's typed-array class.
                let shape = self.load_i32(a[0], SHAPE_OFFSET);
                let base = self.load_i32(shape, SHAPE_BASESHAPE_OFFSET);
                let clasp = self.load_i32(base, BASESHAPE_CLASP_OFFSET);
                let cslot = self.i32c(self.h.ta_class_base + 4 * (u32::from(k.code()) - 1));
                let want = self.load_i32(cslot, 0);
                let ok = self.bin(Operator::I32Eq, clasp, want, Type::I32);
                self.guard(inst, ok, &[a[0]])?;
            }
            Opcode::LoadTa | Opcode::StoreTa => {
                // An in-bounds element (a detached array has length 0);
                // else `fail`.
                let k = match self.ty(d.args[0]) {
                    MType::Obj(o) => match o.kind {
                        ObjKind::TypedArray(k) => k,
                        _ => return Err("lowering: typed array op without a kind".into()),
                    },
                    _ => return Err("lowering: typed array op without a kind".into()),
                };
                let fail = self.body.add_block();
                let len = self.load_i32(a[0], TA_LENGTH_PAYLOAD_OFFSET);
                let ok = self.bin(Operator::I32LtU, a[1], len, Type::I32);
                self.check(ok, fail);
                let data = self.load_i32(a[0], TA_DATA_PAYLOAD_OFFSET);
                let sh = k.log2_bytes();
                let addr = if sh == 0 {
                    self.bin(Operator::I32Add, data, a[1], Type::I32)
                } else {
                    let s = self.i32c(sh);
                    let off = self.bin(Operator::I32Shl, a[1], s, Type::I32);
                    self.bin(Operator::I32Add, data, off, Type::I32)
                };
                use crate::opsem::TaKind as K;
                let outs = if d.op == Opcode::LoadTa {
                    let m = self.mem(sh, 0);
                    let v = match k {
                        K::Int8 => self.un(Operator::I32Load8S { memory: m }, addr, Type::I32),
                        K::Uint8 | K::Uint8Clamped => {
                            self.un(Operator::I32Load8U { memory: m }, addr, Type::I32)
                        }
                        K::Int16 => self.un(Operator::I32Load16S { memory: m }, addr, Type::I32),
                        K::Uint16 => self.un(Operator::I32Load16U { memory: m }, addr, Type::I32),
                        K::Int32 => self.un(Operator::I32Load { memory: m }, addr, Type::I32),
                        K::Uint32 => return Err("lowering: load_ta of a Uint32Array".into()),
                        K::Float32 | K::Float64 => {
                            let d = if k == K::Float32 {
                                let f = self.un(Operator::F32Load { memory: m }, addr, Type::F32);
                                self.un(Operator::F64PromoteF32, f, Type::F64)
                            } else {
                                self.un(Operator::F64Load { memory: m }, addr, Type::F64)
                            };
                            // Any NaN as the canonical one: a boxed double
                            // must not look like a tag.
                            let nan = self.bin(Operator::F64Ne, d, d, Type::I32);
                            let c = self.f64c(f64::NAN.to_bits());
                            self.select(Type::F64, c, d, nan)
                        }
                    };
                    vec![v]
                } else {
                    let m = self.mem(sh, 0);
                    let v = a[2];
                    match k {
                        K::Int8 | K::Uint8 => {
                            self.op(Operator::I32Store8 { memory: m }, &[addr, v], None);
                        }
                        K::Int16 | K::Uint16 => {
                            self.op(Operator::I32Store16 { memory: m }, &[addr, v], None);
                        }
                        K::Int32 | K::Uint32 => {
                            self.op(Operator::I32Store { memory: m }, &[addr, v], None);
                        }
                        K::Float32 => {
                            let f = self.un(Operator::F32DemoteF64, v, Type::F32);
                            self.op(Operator::F32Store { memory: m }, &[addr, f], None);
                        }
                        K::Float64 => {
                            self.op(Operator::F64Store { memory: m }, &[addr, v], None);
                        }
                        K::Uint8Clamped => {
                            // An int32 clamped to 0..=255.
                            let z = self.i32c(0);
                            let neg = self.bin(Operator::I32LtS, v, z, Type::I32);
                            let lo = self.select(Type::I32, z, v, neg);
                            let hi = self.i32c(255);
                            let big = self.bin(Operator::I32GtS, lo, hi, Type::I32);
                            let c = self.select(Type::I32, hi, lo, big);
                            self.op(Operator::I32Store8 { memory: m }, &[addr, c], None);
                        }
                    }
                    vec![]
                };
                let t = self.edge(inst, 0, &outs)?;
                self.terminate(Terminator::Br { target: t });
                self.cur = fail;
                let f = self.edge(inst, 1, &[])?;
                self.terminate(Terminator::Br { target: f });
            }
            Opcode::LoadElem if self.fusable_unbox(inst).is_some() => {
                let (g, k, p) = self.fusable_unbox(inst).unwrap();
                self.load_elem_unboxed(inst, g, k, p, a[0], a[1])?;
            }
            Opcode::LoadElem => {
                // An in-bounds, non-hole dense element; else `fail`.
                let fail = self.body.add_block();
                let (_, v) = self.elem_addr(a[0], a[1], fail, true);
                let t = self.edge(inst, 0, &[v.unwrap()])?;
                self.terminate(Terminator::Br { target: t });
                self.cur = fail;
                let f = self.edge(inst, 1, &[])?;
                self.terminate(Terminator::Br { target: f });
            }
            Opcode::StoreElem(duty, append) => {
                // An in-bounds overwrite of a non-hole dense element of
                // unfrozen elements (an own writable data property); else
                // `fail`. RANGES is dropped under the store's duty.
                let num =
                    matches!(self.ty(d.args[2]), MType::Val(s) if s.tags.subset_of(TagSet::NUMBER));
                let fail = self.body.add_block();
                let (addr, _) = self.elem_addr(a[0], a[1], fail, true);
                let elements = self.load_i32(a[0], OBJ_ELEMENTS_OFFSET);
                let back = self.i32c(ELEMENTS_FLAGS_BACK);
                let header = self.bin(Operator::I32Sub, elements, back, Type::I32);
                let flags = self.load_i32(header, 0);
                let fz = self.i32c(ELEMENTS_FROZEN_FLAG);
                let frozen = self.bin(Operator::I32And, flags, fz, Type::I32);
                let thawed = self.un(Operator::I32Eqz, frozen, Type::I32);
                self.check(thawed, fail);
                if duty {
                    self.elem_duty(a[0]);
                }
                if !num {
                    self.pre_barrier(addr, 0);
                }
                self.store_i64(addr, 0, a[2]);
                if !num {
                    let f = self.h.post_write_barrier_elem;
                    self.post_barrier(f, a[0], a[1], a[2]);
                }
                let t = self.edge(inst, 0, &[])?;
                self.terminate(Terminator::Br { target: t });
                self.cur = fail;
                // Past the initialized length or into a hole: the append
                // arm, where the prototypes' proof allows (it writes the
                // lengths, and runs no JS); else `fail`.
                if append {
                    let miss = self.body.add_block();
                    let recv = self.box_tagged(TAG_OBJECT, a[0]);
                    let key = self.box_tagged(TAG_INT32, a[1]);
                    self.elem_append_arm(inst, recv, key, a[2], duty, num, miss)?;
                    self.cur = miss;
                    // What the arm refused (growth, a row not yet cached):
                    // the runtime's append, which runs no JS.
                    if duty {
                        self.elem_duty(a[0]);
                    }
                    let live = self.live_across(inst);
                    let (ok, _) = self.gc_call(self.h.elem_grow, &[recv, a[1], a[2]], &live)?;
                    self.epoch_same = None;
                    let t = self.edge(inst, 0, &[])?;
                    let f = self.edge(inst, 1, &[])?;
                    self.cond_br(ok, t, f);
                    return Ok(());
                }
                let f = self.edge(inst, 1, &[])?;
                self.terminate(Terminator::Br { target: f });
            }
            Opcode::Call => self.js_call_op(inst, &a, false)?,
            Opcode::CallIter => self.js_call_op(inst, &a, true)?,
            Opcode::CallEval(pc) => self.js_eval_op(inst, &a, pc)?,
            Opcode::GuardScript(sid) => self.guard_script(inst, &d, &a, sid)?,
            Opcode::AccessorProbe(name, set) => {
                // The accessor arm: the (receiver shape, atom, kind)-hashed
                // row the IC miss helpers prime for a prototype getter or
                // setter, its holder's shape still the recorded one.
                use crate::wasm::translate::{
                    ACCESSOR_ATOM_KIND, ACCESSOR_CACHE_ENTRY_BYTES, ACCESSOR_CACHE_SIZE,
                    ACCESSOR_CALLEE, ACCESSOR_HOLDER_PTR, ACCESSOR_HOLDER_SHAPE,
                    ACCESSOR_RECV_SHAPE,
                };
                let aid = self.atoms.intern_chars(self.mm.atoms[name].chars());
                let ak = (aid << 1) | u32::from(set);
                let fail_b = self.body.add_block();
                let tag = self.tag_of(a[0]);
                let is_obj = self.tag_is(tag, TAG_OBJECT as u32);
                self.check(is_obj, fail_b);
                let obj = self.un(Operator::I32WrapI64, a[0], Type::I32);
                let shape = self.load_i32(obj, SHAPE_OFFSET);
                let three = self.i32c(3);
                let sh = self.bin(Operator::I32ShrU, shape, three, Type::I32);
                let k1 = self.i32c(2654435761);
                let h1 = self.bin(Operator::I32Mul, sh, k1, Type::I32);
                let k2 = self.i32c(ak.wrapping_mul(0x9e37_79b9));
                let hh = self.bin(Operator::I32Xor, h1, k2, Type::I32);
                let mask = self.i32c(ACCESSOR_CACHE_SIZE - 1);
                let idx = self.bin(Operator::I32And, hh, mask, Type::I32);
                let stride = self.i32c(ACCESSOR_CACHE_ENTRY_BYTES);
                let off = self.bin(Operator::I32Mul, idx, stride, Type::I32);
                let base = self.i32c(self.h.accessor_cache_base);
                let entry = self.bin(Operator::I32Add, base, off, Type::I32);
                let es = self.load_i32(entry, ACCESSOR_RECV_SHAPE);
                let ea = self.load_i32(entry, ACCESSOR_ATOM_KIND);
                let m1 = self.bin(Operator::I32Eq, es, shape, Type::I32);
                let akv = self.i32c(ak);
                let m2 = self.bin(Operator::I32Eq, ea, akv, Type::I32);
                let m = self.bin(Operator::I32And, m1, m2, Type::I32);
                self.check(m, fail_b);
                // A matched row is primed: its holder is non-null.
                let hp = self.load_i32(entry, ACCESSOR_HOLDER_PTR);
                let hs = self.load_i32(entry, ACCESSOR_HOLDER_SHAPE);
                let live = self.load_i32(hp, SHAPE_OFFSET);
                let m3 = self.bin(Operator::I32Eq, live, hs, Type::I32);
                self.check(m3, fail_b);
                let callee = self.load_i64(entry, ACCESSOR_CALLEE);
                let t = self.edge(inst, 0, &[callee])?;
                self.terminate(Terminator::Br { target: t });
                self.cur = fail_b;
                let f = self.edge(inst, 1, &[])?;
                self.terminate(Terminator::Br { target: f });
            }
            Opcode::InlineEnter => self.inline_enter(&d, &a)?,
            Opcode::NewThis(nslots, word) => {
                // Runs no code. The inline nursery allocation cannot GC, so
                // it takes the ok edge with everything still in registers;
                // only the helper call on its slow path may GC, and only
                // there is what is live across it rooted.
                let live = self.live_across(inst);
                let callee = self.box_tagged(TAG_OBJECT, a[0]);
                let (this_v, slow, cell) = self.construct_this_inline(callee, Some(a[1]), word);
                let t = self.edge(inst, 0, &[this_v])?;
                self.terminate(Terminator::Br { target: t });
                self.cur = slow;
                self.root(&live)?;
                let top_off = self.top_off(live.len());
                let top = self.add_off(self.vp, top_off);
                let ok =
                    self.construct_this_call(top, callee, callee, Some(a[1]), nslots, word, cell);
                self.after_gc(&live);
                self.cold = true;
                let r = self.load_i64(self.vp, top_off);
                let t = self.edge(inst, 0, &[r])?;
                let e = self.edge(inst, 1, &[])?;
                self.cond_br(ok, t, e);
            }
            Opcode::NewThisInit {
                nslots,
                word,
                types,
                n,
                ..
            } => {
                // As `new_this`, with the first `n` fields made with the
                // object: the row's final shape and one check of the
                // prototype chain, else the runtime's adds (which fill the
                // row), failing before anything observable where an add
                // would not be plain.
                let live = self.live_across(inst);
                let callee = self.box_tagged(TAG_OBJECT, a[0]);
                let xs: Vec<Value> = a[2..].to_vec();
                let (obj, slow, cell) = self.construct_this_final(a[1], word, &xs);
                let t = self.edge(inst, 0, &[obj])?;
                self.terminate(Terminator::Br { target: t });
                self.cur = slow;
                self.root(&live)?;
                let base = self.top_off(live.len());
                for (i, &x) in xs.iter().enumerate() {
                    self.store_i64(self.vp, base + 8 * u32::try_from(i).unwrap(), x);
                }
                let top_off = base + 8 * n;
                let top = self.add_off(self.vp, top_off);
                let vals = self.add_off(self.vp, base);
                let cache0 = self.atoms.next_prop_cache();
                for _ in 1..n {
                    self.atoms.next_prop_cache();
                }
                let bits = CLASS_WORD_SLOTS | if types { CLASS_WORD_SHALLOW } else { 0 };
                let args: Vec<Value> = [nslots, 0, word, 0, n, cache0, bits]
                    .iter()
                    .map(|&c| self.i32c(c))
                    .collect();
                let r = self.call1(
                    self.h.new_this_init,
                    &[
                        self.cx, top, callee, a[1], args[0], cell, args[2], vals, args[4], args[5],
                        args[6],
                    ],
                    Type::I32,
                );
                self.after_gc(&live);
                self.cold = true;
                let out = self.load_i64(self.vp, top_off);
                let o = self.un(Operator::I32WrapI64, out, Type::I32);
                let one = self.i32c(1);
                let made = self.bin(Operator::I32Eq, r, one, Type::I32);
                let (yes, no) = (self.body.add_block(), self.body.add_block());
                self.cond_br(made, Self::to(yes), Self::to(no));
                self.cur = yes;
                let t = self.edge(inst, 0, &[o])?;
                self.terminate(Terminator::Br { target: t });
                self.cur = no;
                let f = self.edge(inst, 1, &[])?;
                let e = self.edge(inst, 2, &[])?;
                self.cond_br(r, f, e);
            }
            Opcode::CreateThis(nslots, word) => {
                // May GC: root what is live across it.
                let live = self.live_across(inst);
                self.root(&live)?;
                let top_off = self.top_off(live.len());
                let top = self.add_off(self.vp, top_off);
                // `.prototype` may be a getter (a proxy's): clean only if
                // no class word was demoted meanwhile.
                let pre = self.epoch();
                let ok = self.construct_this(top, a[0], a[1], nslots, word);
                self.after_gc(&live);
                let post = self.epoch();
                let same = self.bin(Operator::I32Eq, pre, post, Type::I32);
                let r = self.load_i64(self.vp, top_off);
                self.clean_or_dirty(inst, ok, same, &[r])?;
            }
            Opcode::ObjEmulatesUndef => {
                // Only while some object's class emulates `undefined` (the
                // runtime's fuse) can one be falsy; the leaf says.
                let (yes, no, join) = (
                    self.body.add_block(),
                    self.body.add_block(),
                    self.body.add_block(),
                );
                let r = self.body.add_blockparam(join, Type::I32);
                let dda = self.dda_possible();
                self.cond_br(dda, Self::to(yes), Self::to(no));
                self.cur = no;
                let z = self.i32c(0);
                self.terminate(Terminator::Br {
                    target: BlockTarget {
                        block: join,
                        args: vec![z],
                    },
                });
                self.cur = yes;
                let boxed = self.box_tagged(TAG_OBJECT, a[0]);
                let t = self.call1(self.h.to_boolean, &[self.cx, boxed], Type::I32);
                let f = self.un(Operator::I32Eqz, t, Type::I32);
                self.terminate(Terminator::Br {
                    target: BlockTarget {
                        block: join,
                        args: vec![f],
                    },
                });
                self.cur = join;
                self.def(inst, r);
            }
            Opcode::FnIsCtor => {
                let flags = self.load_i32(a[0], FUNC_FLAGS_SLOT_OFFSET);
                let cbit = self.i32c(FUNCTION_FLAGS_CONSTRUCTOR);
                let c = self.bin(Operator::I32And, flags, cbit, Type::I32);
                let z = self.i32c(0);
                let v = self.bin(Operator::I32Ne, c, z, Type::I32);
                self.def(inst, v);
            }
            Opcode::CtorStamp(layout, nfields, keep) => {
                self.ctor_stamp_inline(a[0], layout, nfields, keep, false)
            }
            Opcode::CtorPublish(layout, nfields, keep) => {
                self.ctor_stamp_inline(a[0], layout, nfields, keep, true)
            }
            Opcode::JsRt(r) => {
                use crate::mir::ops::RtOp;
                let h = self.h;
                let (f, args) = match r {
                    RtOp::Instanceof => {
                        let cell = self.i32c(IOF_CELL_ADDR_PLACEHOLDER);
                        let idx = self.atoms.next_iof_cell();
                        self.iof_cell_patches.push((cell, idx + 1));
                        self.instanceof_arms(inst, a[0], a[1], cell)?;
                        (h.instanceof_, vec![a[0], a[1], cell])
                    }
                    RtOp::In => (h.in_, vec![a[0], a[1]]),
                    RtOp::HasOwn => {
                        self.has_own_arm(inst, a[0], a[1])?;
                        (h.has_own, vec![a[0], a[1]])
                    }
                    RtOp::DelProp(name, strict) => {
                        let (at, sv) = (self.atom(name), self.i32c(u32::from(strict)));
                        (h.del_prop, vec![a[0], at, sv])
                    }
                    RtOp::DelElem(strict) => {
                        let sv = self.i32c(u32::from(strict));
                        (h.del_elem, vec![a[0], a[1], sv])
                    }
                    RtOp::NewObject(n) => {
                        let cell = self.alloc_inline(inst, None)?;
                        let nv = self.i32c(n);
                        (h.new_object, vec![cell, nv])
                    }
                    RtOp::NewArray(len) => {
                        let cell = self.alloc_inline(inst, Some(len))?;
                        let lv = self.i32c(len);
                        (h.new_array, vec![lv, cell])
                    }
                    RtOp::InitProp(name, attrs) => {
                        let site = if attrs == crate::wasm::mir::abi::INIT_ATTR_ENUMERATE {
                            self.init_prop_inline(inst, a[0], a[1])?
                        } else {
                            u32::MAX
                        };
                        let (at, av, sv) = (self.atom(name), self.i32c(attrs), self.i32c(site));
                        (h.init_prop, vec![a[0], at, a[1], av, sv])
                    }
                    RtOp::InitElem(attrs, duty) => {
                        let key = self.f.insts[inst].args[1];
                        let index = match self.f.values[key].def {
                            mir::func::ValueDef::Result(i, _) => match self.f.insts[i].op {
                                Opcode::ConstVal(ConstVal::Int32(n)) => u32::try_from(n).ok(),
                                _ => None,
                            },
                            _ => None,
                        };
                        match index {
                            Some(i)
                                if attrs == crate::wasm::mir::abi::INIT_ATTR_ENUMERATE
                                    && i < crate::constants::INLINE_INIT_ELEM_CAP =>
                            {
                                self.init_elem_inline(inst, a[0], i, a[2], duty)?
                            }
                            _ => {}
                        }
                        let av = self.i32c(attrs);
                        (h.init_elem, vec![a[0], a[1], a[2], av])
                    }
                    RtOp::ToPropertyKey => (h.to_property_key, vec![a[0]]),
                    RtOp::RegExp(idx) => {
                        let script = self.script_ptr();
                        let iv = self.i32c(idx);
                        (h.regexp, vec![script, iv])
                    }
                    RtOp::ToString => {
                        // A string is its own; else the helper.
                        let tag = self.tag_of(a[0]);
                        let s = self.tag_is(tag, TAG_STRING as u32);
                        let (hit, miss) = (self.body.add_block(), self.body.add_block());
                        self.cond_br(s, Self::to(hit), Self::to(miss));
                        self.cur = hit;
                        let t = self.edge(inst, 0, &[a[0]])?;
                        self.terminate(Terminator::Br { target: t });
                        self.cur = miss;
                        (h.tostring, vec![a[0]])
                    }
                    RtOp::Symbol(code) => {
                        // A well-known symbol: permanent, from a leaf.
                        let cv = self.i32c(code);
                        let r = self.call1(h.symbol, &[self.cx, cv], Type::I64);
                        let t = self.edge(inst, 0, &[r])?;
                        self.terminate(Terminator::Br { target: t });
                        return Ok(());
                    }
                    RtOp::Intrinsic(_) | RtOp::BuiltinObject(_) => {
                        // A realm constant: the cell once armed; the helper
                        // resolves it and arms it.
                        let (id, row, helper) = match r {
                            RtOp::Intrinsic(name) => {
                                let id = self.atoms.intern_chars(self.mm.atoms[name].chars());
                                (id, self.atoms.intrinsic_cell(id), h.get_intrinsic_cell)
                            }
                            RtOp::BuiltinObject(kind) => (
                                kind,
                                self.atoms.builtin_object_cell(kind),
                                h.builtin_object_cell,
                            ),
                            _ => unreachable!(),
                        };
                        let cell =
                            self.i32c(crate::wasm::mir::abi::INTRINSIC_CELL_ADDR_PLACEHOLDER);
                        self.intrinsic_cell_patches.push((cell, row));
                        let bits = self.load_i64(cell, 0);
                        let z = self.i64c(0);
                        let armed = self.bin(Operator::I64Ne, bits, z, Type::I32);
                        let (hit, miss) = (self.body.add_block(), self.body.add_block());
                        self.cond_br(armed, Self::to(hit), Self::to(miss));
                        self.cur = hit;
                        let t = self.edge(inst, 0, &[bits])?;
                        self.terminate(Terminator::Br { target: t });
                        self.cur = miss;
                        let iv = self.i32c(id);
                        (helper, vec![iv, cell])
                    }
                    RtOp::InitPropGetSet(name, kind) => {
                        let (at, kv) = (self.atom(name), self.i32c(kind));
                        (h.init_prop_getset, vec![a[0], at, a[1], kv])
                    }
                    RtOp::Iter => {
                        self.iter_from_shape(inst, a[0])?;
                        (h.iter_, vec![a[0]])
                    }
                    RtOp::Check(k) => {
                        let f = match k {
                            mir::ops::CHECK_OBJ_COERCIBLE => h.check_obj_coercible,
                            mir::ops::CHECK_CLASS_HERITAGE => h.check_class_heritage,
                            mir::ops::CHECK_THIS_REINIT => h.check_this_reinit,
                            _ => h.check_this,
                        };
                        (f, vec![a[0]])
                    }
                    RtOp::SetFunName(prefix) => {
                        let k = self.i32c(prefix);
                        (h.set_fun_name, vec![a[0], a[1], k])
                    }
                    RtOp::GlobalThis => (h.global_this, vec![]),
                    RtOp::BigInt(idx) => {
                        let script = self.script_ptr();
                        let iv = self.i32c(idx);
                        (h.bigint, vec![script, iv])
                    }
                    RtOp::MutateProto => (h.mutate_proto, vec![a[0], a[1]]),
                    RtOp::CheckIsObj(kind) => {
                        let k = self.i32c(kind);
                        (h.check_is_obj, vec![a[0], k])
                    }
                    RtOp::CloseIter(kind) => {
                        let k = self.i32c(kind);
                        (h.close_iter, vec![a[0], k])
                    }
                    RtOp::OptimizeSpreadCall => (h.optimize_spread_call, vec![a[0]]),
                    RtOp::PushEnv(kind, pc) => {
                        let f = match kind {
                            mir::ops::ENV_LEXICAL => h.push_lexical_env,
                            mir::ops::ENV_CLASS_BODY => h.push_class_body_env,
                            _ => h.push_var_env,
                        };
                        let env = self.frame_env();
                        let script = self.script_ptr();
                        let pcv = self.i32c(pc);
                        (f, vec![env, script, pcv])
                    }
                    RtOp::EnterWith(pc) => {
                        let env = self.frame_env();
                        let script = self.script_ptr();
                        let pcv = self.i32c(pc);
                        (h.enter_with, vec![env, a[0], script, pcv])
                    }
                    RtOp::FreshenEnv(recreate) => {
                        let f = if recreate != 0 {
                            h.recreate_lexical_env
                        } else {
                            h.freshen_lexical_env
                        };
                        (f, vec![self.frame_env()])
                    }
                    RtOp::GetName(name, for_typeof) => {
                        let env = self.frame_env();
                        let (at, tv) = (self.atom(name), self.i32c(for_typeof));
                        (h.get_name, vec![env, at, tv])
                    }
                    RtOp::BindName(name, unqualified) => {
                        let f = if unqualified != 0 {
                            h.bind_unqualified_name
                        } else {
                            h.bind_name
                        };
                        let env = self.frame_env();
                        (f, vec![env, self.atom(name)])
                    }
                    RtOp::DelName(name) => {
                        let env = self.frame_env();
                        (h.del_name, vec![env, self.atom(name)])
                    }
                    RtOp::BindVar => (h.bind_var, vec![self.frame_env()]),
                    RtOp::SuperBase => (h.super_base, vec![a[0]]),
                    RtOp::SuperFun => (h.super_fun, vec![a[0]]),
                    RtOp::GetPropSuper(name) => {
                        (h.get_prop_super, vec![a[0], a[1], self.atom(name)])
                    }
                    RtOp::GetElemSuper => (h.get_elem_super, vec![a[0], a[1], a[2]]),
                    RtOp::SetPropSuper(name, strict) => {
                        let (at, sv) = (self.atom(name), self.i32c(u32::from(strict)));
                        (h.set_prop_super, vec![a[0], a[1], at, a[2], sv])
                    }
                    RtOp::SetElemSuper(strict) => {
                        let sv = self.i32c(u32::from(strict));
                        (h.set_elem_super, vec![a[0], a[1], a[2], a[3], sv])
                    }
                    RtOp::InitHomeObject => (h.init_home_object, vec![a[0], a[1]]),
                    RtOp::FunWithProto(index) => {
                        let env = self.frame_env();
                        let script = self.script_ptr();
                        let iv = self.i32c(index);
                        (h.fun_with_proto, vec![env, a[0], script, iv])
                    }
                    RtOp::CheckReturn => (h.check_return, vec![a[0], a[1]]),
                    RtOp::CreateGenerator => {
                        let callee = self.load_i64(self.sp, FrameLayout::CALLEE);
                        let env = if self.plain_env || self.own_env {
                            self.frame_env()
                        } else {
                            self.i64c(UNDEF)
                        };
                        (h.create_generator, vec![callee, env])
                    }
                    RtOp::GenFinal => (h.gen_final, vec![a[0]]),
                    RtOp::GenCheckResume => {
                        let k = self.un(Operator::I32WrapI64, a[2], Type::I32);
                        let rval = self.add_off(
                            self.vp,
                            self.frame_voff[self.cur_frame as usize] + self.layout.rval(),
                        );
                        (h.gen_check_resume, vec![a[1], a[0], k, rval])
                    }
                    RtOp::AsyncAwait(resolve) => {
                        let f = if resolve != 0 {
                            h.async_resolve
                        } else {
                            h.async_await
                        };
                        (f, vec![a[1], a[0]])
                    }
                    RtOp::AsyncReject => (h.async_reject, vec![a[2], a[0], a[1]]),
                    RtOp::CanSkipAwait => (h.can_skip_await, vec![a[0]]),
                    RtOp::MaybeExtractAwait => {
                        let can = self.un(Operator::I32WrapI64, a[1], Type::I32);
                        (h.maybe_extract_await, vec![a[0], can])
                    }
                    RtOp::Resume => (h.resume, vec![a[0], a[1], a[2]]),
                    RtOp::AddDisposable(hint) => {
                        let env = self.frame_env();
                        let hv = self.i32c(hint);
                        (h.add_disposable, vec![env, a[0], a[1], a[2], hv])
                    }
                    RtOp::TakeDisposeCapability => {
                        (h.take_dispose_capability, vec![self.frame_env()])
                    }
                    RtOp::CreateSuppressedError => (h.create_suppressed_error, vec![a[0], a[1]]),
                    RtOp::GetBoundName(name) => (h.get_bound_name, vec![a[0], self.atom(name)]),
                    RtOp::ObjWithProto => (h.obj_with_proto, vec![a[0]]),
                    RtOp::NewPrivateName(name) => (h.new_private_name, vec![self.atom(name)]),
                    RtOp::DynamicImport => {
                        let script = self.script_ptr();
                        (h.dynamic_import, vec![script, a[0], a[1]])
                    }
                    RtOp::SpreadEval(pc) => {
                        let env = self.frame_env();
                        let script = self.script_ptr();
                        let pcv = self.i32c(pc);
                        (h.spread_eval, vec![a[0], a[1], a[2], env, script, pcv])
                    }
                    RtOp::InitElemGetSet(kind) => {
                        let kv = self.i32c(kind);
                        (h.init_elem_getset, vec![a[0], a[1], a[2], kv])
                    }
                    RtOp::SetName(name, strict) => {
                        let (at, sv) = (self.atom(name), self.i32c(u32::from(strict)));
                        (h.set_name, vec![a[0], at, a[1], sv])
                    }
                    RtOp::SpreadCall(construct) => {
                        let c = self.i32c(construct);
                        (h.spread_call, vec![a[0], a[1], a[2], a[3], c])
                    }
                    RtOp::CheckPrivateField(cond, kind) => {
                        let (cv, kv) = (self.i32c(cond), self.i32c(kind));
                        (h.check_private_field, vec![a[0], a[1], cv, kv])
                    }
                    RtOp::GetNameTypeof(name) => {
                        // `get_gname`'s typeof form: unbound is undefined.
                        let (at, one) = (self.atom(name), self.i32c(1));
                        (h.get_gname, vec![at, one])
                    }
                };
                self.js_call(inst, f, &args, false)?;
            }
            Opcode::JsThrow => {
                // The helper sets the pending exception and always fails.
                // It may GC (capturing the stack): root what its throw exit
                // reads.
                let live = self.live_across(inst);
                self.root(&live)?;
                let top_off = self.top_off(live.len());
                let top = self.add_off(self.vp, top_off);
                self.call(self.h.throw, &[self.cx, top, a[0]], &[]);
                self.after_gc(&live);
                let e = self.edge(inst, 0, &[])?;
                self.terminate(Terminator::Br { target: e });
            }
            Opcode::ArgsObject => {
                // The frame caches it, as baseline does, so both tiers
                // (and repeated reads) see one object.
                let fid = self.cur_frame as usize;
                let l = self.frame_layouts[fid];
                let slot = self.frame_voff[fid] + l.args_obj();
                let (asp, argc) = self.actuals();
                let cached = self.load_i64(self.vp, slot);
                let tag = self.tag_of(cached);
                let undef = self.tag_is(tag, TAG_UNDEFINED as u32);
                let (build, have) = (self.body.add_block(), self.body.add_block());
                self.cond_br(undef, Self::to(build), Self::to(have));
                self.cur = have;
                let t = self.edge(inst, 0, &[cached])?;
                self.terminate(Terminator::Br { target: t });
                self.cur = build;
                let live = self.live_across(inst);
                // With an environment, the runtime is given it, as
                // baseline's prologue does: a mapped object in a function
                // with a call object records it (`MaybeForwardToCallObject`).
                // (An inlined callee's object is unmapped: no env.)
                let (ok, r) = if fid == 0 && (self.plain_env || self.own_env) {
                    let env = self.load_i64(self.vp, l.env());
                    self.gc_call(self.h.arguments_env, &[asp, argc, env], &live)?
                } else {
                    self.gc_call(self.h.arguments_, &[asp, argc], &live)?
                };
                let (store, e) = (self.body.add_block(), self.edge(inst, 1, &[])?);
                self.cond_br(ok, Self::to(store), e);
                self.cur = store;
                self.store_i64(self.vp, slot, r);
                let t = self.edge(inst, 0, &[r])?;
                self.terminate(Terminator::Br { target: t });
            }
            Opcode::RestArray(n) => {
                let live = self.live_across(inst);
                let nv = self.i32c(n);
                let (asp, argc) = self.actuals();
                let (ok, r) = self.gc_call(self.h.rest, &[asp, argc, nv], &live)?;
                let t = self.edge(inst, 0, &[r])?;
                let e = self.edge(inst, 1, &[])?;
                self.cond_br(ok, t, e);
            }
            Opcode::ArgsLength => {
                let (_, argc) = self.actuals();
                self.def(inst, argc);
            }
            Opcode::IterMore => {
                let r = self.iter_more(a[0]);
                self.def(inst, r);
            }
            Opcode::IterEnd => self.iter_end(a[0]),
            Opcode::IterOptimizable => {
                let r = self.call1(self.h.optimize_get_iterator, &[self.cx, a[0]], Type::I32);
                self.def(inst, r);
            }
            Opcode::IterIsDone => {
                let m = self.i64c((TAG_MAGIC << 32) | crate::wasm::translate::MAGIC_NO_ITER_VALUE);
                let r = self.bin(Operator::I64Eq, a[0], m, Type::I32);
                self.def(inst, r);
            }
            Opcode::FrameCallee => {
                let fid = self.cur_frame as usize;
                let v = if fid == 0 {
                    self.load_i64(self.sp, FrameLayout::CALLEE)
                } else {
                    self.load_i64(self.vp, self.frame_off[fid] + FrameLayout::CALLEE)
                };
                self.def(inst, v);
            }
            Opcode::FrameNewTarget => {
                let fid = self.cur_frame as usize;
                let off = self.frame_voff[fid] + self.frame_layouts[fid].new_target();
                let v = self.load_i64(self.vp, off);
                self.def(inst, v);
            }
            Opcode::ActualArgOr(k) => {
                let (asp, argc) = self.actuals();
                let v = self.load_i64(asp, FrameLayout::ARGS + 8 * k);
                let kv = self.i32c(k);
                let have = self.bin(Operator::I32LtU, kv, argc, Type::I32);
                let undef = self.i64c(UNDEF);
                let r = self.select(Type::I64, v, undef, have);
                self.def(inst, r);
            }
            Opcode::JsIsBuiltin(k) => {
                let cell = self.i32c(self.h.builtin_cells_base + 8 * k);
                let bits = self.load_i64(cell, 0);
                let r = self.bin(Operator::I64Eq, a[0], bits, Type::I32);
                self.def(inst, r);
            }
            Opcode::ApplyFwd => {
                let (asp, argc) = self.actuals();
                self.js_call(
                    inst,
                    self.h.apply_fwd,
                    &[a[0], a[1], a[2], asp, argc],
                    false,
                )?;
            }
            Opcode::ActualArg => {
                let eight = self.i32c(8);
                let off = self.bin(Operator::I32Mul, a[0], eight, Type::I32);
                let (asp, _) = self.actuals();
                let addr = self.bin(Operator::I32Add, asp, off, Type::I32);
                let v = self.load_i64(addr, FrameLayout::ARGS);
                self.def(inst, v);
            }
            Opcode::JsTypeof => {
                // A leaf: the type's name is an atom.
                let v = self.call(self.h.typeof_, &[self.cx, a[0]], &[Type::I64]);
                self.def(inst, v);
            }
            Opcode::ExitInline {
                pc,
                nargs,
                nlocals,
                throw,
            } => self.exit_inline(inst, &d, &a, pc, nargs, nlocals, throw)?,
            Opcode::Construct(nslots, word) => {
                // `new Array()`: the pristine Array
                // constructor, as its own new.target, makes what `[]` does.
                if a.len() == 3 && self.names_atom("Array") {
                    let (callee, nt) = (a[0], a[2]);
                    let is_arr = self.builtin_is(callee, crate::wasm::translate::BC_ARRAY_CTOR);
                    let same = self.bin(Operator::I64Eq, callee, nt, Type::I32);
                    let m = self.bin(Operator::I32And, is_arr, same, Type::I32);
                    let (arr, other) = (self.body.add_block(), self.body.add_block());
                    self.cond_br(m, Self::to(arr), Self::to(other));
                    self.cur = arr;
                    // The helper's reload rebinds the live values on its
                    // path only.
                    let saved = (
                        self.vmap.clone(),
                        self.slotted.clone(),
                        self.dirty.clone(),
                        self.framed.clone(),
                    );
                    let cell = self.alloc_inline(inst, Some(0))?;
                    let lv = self.i32c(0);
                    let live = self.live_across(inst);
                    let (ok, r) = self.gc_call(self.h.new_array, &[lv, cell], &live)?;
                    let same = self.epoch_same.take().expect("gc_call sampled the epoch");
                    self.clean_or_dirty(inst, ok, same, &[r])?;
                    (self.vmap, self.slotted, self.dirty, self.framed) = saved;
                    self.cur = other;
                }
                // The frame `[callee, this, args…, new.target]` above the
                // rooting slots, then the runtime's construct (it creates
                // `this` sized and seeded for the site, and runs the
                // constructor). The result lands at the frame's top.
                let live = self.live_across(inst);
                self.root(&live)?;
                let construct_pre = self.epoch();
                let frame = self.top_off(live.len());
                for (k, &v) in a.iter().enumerate() {
                    self.store_i64(self.vp, frame + 8 * u32::try_from(k).unwrap(), v);
                }
                let argc = u32::try_from(a.len() - 3).unwrap();
                let top_off = frame + 8 * u32::try_from(a.len()).unwrap();
                let base = self.add_off(self.vp, frame);
                let top = self.add_off(self.vp, top_off);
                let (callee, new_target) = (a[0], a[a.len() - 1]);
                let join = self.body.add_block();
                let ok_p = self.body.add_blockparam(join, Type::I32);
                let res_p = self.body.add_blockparam(join, Type::I64);
                // Direct: a compiled constructor gets its `this` from
                // `create_this` (sized and stamped for the site) and runs
                // by a direct call, with no trip through the engine's
                // construct.
                const HEADROOM: u32 = 64 * 1024;
                let (funcidx, script) = self.classify(callee);
                let z = self.i32c(0);
                let compiled = self.bin(Operator::I32Ne, funcidx, z, Type::I32);
                let fits = self.stack_fits(top, HEADROOM);
                let maybe = self.bin(Operator::I32And, compiled, fits, Type::I32);
                let enabled = self.i32c(1);
                let maybe = self.bin(Operator::I32And, maybe, enabled, Type::I32);
                let (chk, generic) = (self.body.add_block(), self.body.add_block());
                self.cond_br(maybe, Self::to(chk), Self::to(generic));
                self.cur = chk;
                let fun = self.un(Operator::I32WrapI64, callee, Type::I32);
                let flags = self.load_i32(fun, FUNC_FLAGS_SLOT_OFFSET);
                let cbit = self.i32c(FUNCTION_FLAGS_CONSTRUCTOR);
                let is_ctor = self.bin(Operator::I32And, flags, cbit, Type::I32);
                let direct = self.body.add_block();
                self.cond_br(is_ctor, Self::to(direct), Self::to(generic));
                self.cur = direct;
                let made = self.construct_this(top, callee, new_target, nslots, word);
                let call_b = self.body.add_block();
                let undef = self.i64c(UNDEF);
                self.cond_br(
                    made,
                    Self::to(call_b),
                    BlockTarget {
                        block: join,
                        args: vec![made, undef],
                    },
                );
                self.cur = call_b;
                let thisv = self.load_i64(top, 0);
                self.store_i64(base, FrameLayout::THIS, thisv);
                // `create_this` may GC: the frame's copies are current, the
                // operands from before it are not.
                let new_target = self.load_i64(base, 8 * (argc + 2));
                let off = self.i32c(u32::MAX);
                self.body_off_patches.push(off);
                let body_idx = self.bin(Operator::I32Sub, funcidx, off, Type::I32);
                let av = self.i32c(argc);
                let args = self
                    .body
                    .arg_pool
                    .from_iter([self.cx, base, av, top, script, new_target, body_idx].into_iter());
                let tys = self
                    .body
                    .type_pool
                    .from_iter([Type::I32, Type::I32].into_iter());
                let call = self.push_val(ValueDef::Operator(
                    Operator::CallIndirect {
                        sig_index: self.h.night_abi_sig2,
                        table_index: self.h.indirect_table,
                    },
                    args,
                    tys,
                ));
                let err = self.push_val(ValueDef::PickOutput(call, 0, Type::I32));
                let ok_d = self.un(Operator::I32Eqz, err, Type::I32);
                // The constructor's result if an object, else its `this`
                // (reread: the GC updates the frame).
                let r = self.load_i64(top, 0);
                let rt = self.tag_of(r);
                let obj = self.tag_is(rt, TAG_OBJECT as u32);
                let th = self.load_i64(base, FrameLayout::THIS);
                let res = self.select(Type::I64, r, th, obj);
                self.terminate(Terminator::Br {
                    target: BlockTarget {
                        block: join,
                        args: vec![ok_d, res],
                    },
                });
                self.cur = generic;
                let (av, nv, wv) = (self.i32c(argc), self.i32c(nslots), self.i32c(word));
                let ok = self.call1(
                    self.h.construct,
                    &[self.cx, top, base, av, nv, wv],
                    Type::I32,
                );
                let r = self.load_i64(top, 0);
                self.terminate(Terminator::Br {
                    target: BlockTarget {
                        block: join,
                        args: vec![ok, r],
                    },
                });
                self.cur = join;
                self.after_gc(&live);
                let post = self.epoch();
                let same = self.bin(Operator::I32Eq, construct_pre, post, Type::I32);
                self.clean_or_dirty(inst, ok_p, same, &[res_p])?;
            }
            Opcode::GuardLayout {
                keys,
                types,
                slots,
                closed,
            } => {
                // The stamp word (`JSObject*+4`): identity is layout key + 1
                // in the low 16 bits; TYPES is the SHALLOW bit (§4.3). The
                // SLOTS bit too: a claim then holds
                // the fields at their slots, until a kill (only the engine
                // clears SLOTS, bumping the epoch), so the field ops under
                // it load and store with no test of their own.
                let w = self.load_i32(a[0], OBJ_CLASS_IDX_OFFSET);
                let m = self.i32c(0xFFFF);
                let id = self.bin(Operator::I32And, w, m, Type::I32);
                let want = if types { CLASS_WORD_SHALLOW } else { 0 }
                    | if slots { CLASS_WORD_SLOTS } else { 0 }
                    | if closed { CLASS_WORD_CLOSED } else { 0 };
                // One masked compare: identity and
                // the bits at once. Over a range, the masked word less the
                // lowest wanted word is at most the span only with every
                // bit there (a missing bit, at 1 << 16 or above, wraps it).
                // The sentinel too, which the expected word lacks: under
                // it the low half is a set of fields, not an identity.
                let mask = self.i32c(CLASS_WORD_SENTINEL | 0xFFFF | want);
                let wm = self.bin(Operator::I32And, w, mask, Type::I32);
                let exp = self.i32c((keys.lo.get() + 1) | want);
                let ok = if keys.lo == keys.hi {
                    self.bin(Operator::I32Eq, wm, exp, Type::I32)
                } else {
                    let rel = self.bin(Operator::I32Sub, wm, exp, Type::I32);
                    let span = self.i32c(keys.hi.get() - keys.lo.get());
                    self.bin(Operator::I32LeU, rel, span, Type::I32)
                };
                self.guard_word = Some((w, id, keys));
                self.guard_obj = Some(a[0]);
                self.guard(inst, ok, &[a[0]])?;
                self.guard_word = None;
                self.guard_obj = None;
            }
            Opcode::GuardCtor { key, n, types } => {
                // Under construction for `key` (§2.3): the sentinel with
                // its early key, SLOTS (and TYPES), the word's field set
                // `n`, and the span of `n` (every field is at its row's
                // slot, so a property past them would raise it): exactly
                // the fields `n`.
                let w = self.load_i32(a[0], OBJ_CLASS_IDX_OFFSET);
                let bits = CLASS_WORD_SLOTS | if types { CLASS_WORD_SHALLOW } else { 0 };
                let fm = (1 << mir::types::FieldSet::WORD_BITS) - 1;
                let m =
                    self.i32c(CLASS_WORD_SENTINEL | (EARLY_KEY_MAX << EARLY_KEY_SHIFT) | bits | fm);
                let want = self.i32c(
                    CLASS_WORD_SENTINEL
                        | ((key.get() + 1) << EARLY_KEY_SHIFT)
                        | bits
                        | n.word_bits(),
                );
                let wm = self.bin(Operator::I32And, w, m, Type::I32);
                let ok = self.bin(Operator::I32Eq, wm, want, Type::I32);
                let shape = self.load_i32(a[0], SHAPE_OFFSET);
                let imm = self.load_i32(shape, SHAPE_IMMUTABLE_FLAGS_OFFSET);
                let sm = self.i32c(SHAPE_SMALL_SLOTSPAN_MASK_BITS << SHAPE_SMALL_SLOTSPAN_SHIFT);
                let span = self.bin(Operator::I32And, imm, sm, Type::I32);
                let nv = self.i32c(n.span() << SHAPE_SMALL_SLOTSPAN_SHIFT);
                let s_ok = self.bin(Operator::I32Eq, span, nv, Type::I32);
                let ok = self.bin(Operator::I32And, ok, s_ok, Type::I32);
                self.guard(inst, ok, &[a[0]])?;
            }
            Opcode::InitField(name) => self.init_field(inst, name, a[0], a[1])?,
            Opcode::PublishLayout => {
                // The stamp of a completed `constructing(n)` object (its
                // type proves the sentinel, the key and every field): its
                // identity, keeping SLOTS and, where claimed, TYPES.
                let c = self
                    .ty(d.args[0])
                    .obj_info()
                    .and_then(|o| o.layout)
                    .ok_or("lowering: publish_layout without a layout claim")?;
                let w = self.load_i32(a[0], OBJ_CLASS_IDX_OFFSET);
                let keep = CLASS_WORD_SLOTS | if c.types { CLASS_WORD_SHALLOW } else { 0 };
                let kb = self.i32c(keep);
                let bits = self.bin(Operator::I32And, w, kb, Type::I32);
                // Exactly the row's fields: CLOSED, where the layout may be.
                let closed = if self.mm.layouts.get(&c.keys.lo).is_some_and(|l| l.closed) {
                    CLASS_WORD_CLOSED
                } else {
                    0
                };
                let idx = self.i32c((c.keys.lo.get() + 1) | closed);
                let nw = self.bin(Operator::I32Or, idx, bits, Type::I32);
                self.store_i32(a[0], OBJ_CLASS_IDX_OFFSET, nw);
                self.def(inst, a[0]);
            }
            Opcode::CheckFuse(fuse) => {
                let addr = self.mm.fuses[fuse].addr;
                if addr == 0 {
                    return Err(format!("lowering: {fuse} has no fuse word"));
                }
                let base = self.i32c(addr);
                let w = self.load_i32(base, 0);
                let one = self.i32c(1);
                let ok = self.bin(Operator::I32Eq, w, one, Type::I32);
                self.guard(inst, ok, &[])?;
            }
            Opcode::MethodLoad { cell, atom, script } => {
                // The receiver's prototype is the one the cell is armed
                // for (an unarmed cell holds 0, an unarmable one 1): the
                // function there. Else, if unarmed, the runtime arms it
                // for this prototype (a leaf) and it is tested again.
                let sa = self
                    .mm
                    .script_addrs
                    .get(&script)
                    .copied()
                    .ok_or("lowering: method.load of a script with no address")?;
                let c = self.i32c(cell);
                let shape = self.load_i32(a[0], SHAPE_OFFSET);
                let base = self.load_i32(shape, SHAPE_BASESHAPE_OFFSET);
                let proto = self.load_i32(base, BASESHAPE_PROTO_OFFSET);
                let p = self.load_i32(c, 0);
                let hit = self.bin(Operator::I32Eq, proto, p, Type::I32);
                // CLOSED (the receiver is of a published layout: its word
                // is a stamp's).
                let w = self.load_i32(a[0], OBJ_CLASS_IDX_OFFSET);
                let cb = self.i32c(CLASS_WORD_CLOSED);
                let closed = self.bin(Operator::I32And, w, cb, Type::I32);
                let f = self.load_i32(c, crate::region_shape::METHOD_CELL_FN_OFF);
                let (miss, open) = (self.body.add_block(), self.body.add_block());
                let fail = self.edge(inst, 1, &[])?;
                self.cond_br(closed, Self::to(open), fail);
                self.cur = open;
                let t = self.edge(inst, 0, &[f])?;
                self.cond_br(hit, t, Self::to(miss));
                self.cur = miss;
                let (arm, done) = (self.body.add_block(), self.body.add_block());
                let ok = self.body.add_blockparam(done, Type::I32);
                let z = self.i32c(0);
                let unarmed = self.bin(Operator::I32Eq, p, z, Type::I32);
                self.cond_br(
                    unarmed,
                    Self::to(arm),
                    BlockTarget {
                        block: done,
                        args: vec![z],
                    },
                );
                self.cur = arm;
                let rb = self.box_tagged(TAG_OBJECT, a[0]);
                let av = self.atom(atom);
                let sv = self.i32c(sa);
                let r = self.call1(self.h.method_arm, &[rb, c, av, sv], Type::I32);
                self.terminate(Terminator::Br {
                    target: BlockTarget {
                        block: done,
                        args: vec![r],
                    },
                });
                self.cur = done;
                let f = self.load_i32(c, crate::region_shape::METHOD_CELL_FN_OFF);
                self.guard(inst, ok, &[f])?;
            }
            Opcode::CheckBinding(b, BindingCheck::Fn) => {
                // The value fuse in its predicted state: the binding holds
                // a compiled function of its predicted script. A cell a
                // major GC zeroed re-arms at the resolve, so a miss resolves
                // once before it exits.
                let slot = self.mm.bindings[b].slot;
                let vals = self.i32c(self.h.global_vals_base + 16 * slot);
                let fw = self.load_i32(vals, 8);
                let want = self.i32c(BINDING_FUSE_PREDICTED);
                let ok = self.bin(Operator::I32Eq, fw, want, Type::I32);
                let miss = self.body.add_block();
                let t = self.edge(inst, 0, &[])?;
                self.cond_br(ok, t, Self::to(miss));
                self.cur = miss;
                let bv = self.i32c(slot);
                self.call(
                    self.h.resolve_global_slot_guarded,
                    &[self.cx, bv],
                    &[Type::I32],
                );
                let fw = self.load_i32(vals, 8);
                let ok = self.bin(Operator::I32Eq, fw, want, Type::I32);
                self.guard(inst, ok, &[])?;
            }
            Opcode::CheckBinding(b, k) => {
                let slot = self.mm.bindings[b].slot;
                let write = k == BindingCheck::Write;
                if !write {
                    // A read of a binding whose value fuse is armed
                    // (`gGlobalVals`) needs no slot: `load_gname` takes the
                    // value there. Straight to `ok`, the test kept for the
                    // loads its fact reaches (they dominate them).
                    let vals = self.i32c(self.h.global_vals_base + 16 * slot);
                    let fw = self.load_i32(vals, 8);
                    let armed = self.fuse_armed(fw);
                    if let Some(&EdgeArg::Out(0)) = d.succs[0].args.first() {
                        let fact = self.f.blocks[d.succs[0].block].params[0];
                        self.binding_armed.insert(fact, armed);
                    }
                    let chk = self.body.add_block();
                    let t = self.edge(inst, 0, &[])?;
                    self.cond_br(armed, t, Self::to(chk));
                    self.cur = chk;
                } else {
                    // An armed value fuse says the binding is still the
                    // global's own data property (a redefinition, delete or
                    // lexical shadow blows it), and a resolved row whether
                    // it is writable (bit 2; a major GC zeroes the row,
                    // which the full test below resolves again).
                    let vals = self.i32c(self.h.global_vals_base + 16 * slot);
                    let fw = self.load_i32(vals, 8);
                    let base = self.i32c(self.h.global_slots_base);
                    let entry = self.load_i32(base, 8 * slot);
                    let two = self.i32c(2);
                    let armed4 = self.bin(Operator::I32Shl, fw, two, Type::I32);
                    let both = self.bin(Operator::I32And, armed4, entry, Type::I32);
                    let four = self.i32c(4);
                    let fast = self.bin(Operator::I32And, both, four, Type::I32);
                    let chk = self.body.add_block();
                    let t = self.edge(inst, 0, &[])?;
                    self.cond_br(fast, t, Self::to(chk));
                    self.cur = chk;
                }
                let ok = self.binding_ok(slot, write);
                self.guard(inst, ok, &[])?;
            }
            Opcode::LoadGName(b) => {
                // The value fuse's copy while armed, else the slot. The
                // check that made the fact tested the fuse: its test, where
                // the fact is that check's (the fuse only disarms through
                // a write, which kills the fact).
                let slot = self.mm.bindings[b].slot;
                let vals = self.i32c(self.h.global_vals_base + 16 * slot);
                if self.ty(d.args[0]) == MType::Fact(FactKind::BindingFn(b)) {
                    // In the predicted state, which a write leaves only
                    // for another function of the script (and kills the
                    // fact meanwhile): the copy is the value.
                    let v = self.load_i64(vals, 0);
                    self.def(inst, v);
                    return Ok(());
                }
                // (A write to a global disarms its fuse but keeps the
                // fact: only a load in the check's `ok` block, before any
                // write there.)
                let fact = d.args[0];
                let fresh = matches!(self.f.values[fact].def, mir::func::ValueDef::Param(b, _) if b == self.cur_mblock)
                    && !self.f.blocks[self.cur_mblock]
                        .insts
                        .iter()
                        .take_while(|&&i| i != inst)
                        .any(|&i| matches!(self.f.insts[i].op, Opcode::StoreGName(_)));
                let armed = match self.binding_armed.get(&fact).filter(|_| fresh) {
                    Some(&a) => a,
                    None => {
                        let fw = self.load_i32(vals, 8);
                        self.fuse_armed(fw)
                    }
                };
                let (hit, miss, join) = (
                    self.body.add_block(),
                    self.body.add_block(),
                    self.body.add_block(),
                );
                let r = self.body.add_blockparam(join, Type::I64);
                self.cond_br(armed, Self::to(hit), Self::to(miss));
                self.cur = hit;
                let v = self.load_i64(vals, 0);
                self.terminate(Terminator::Br {
                    target: BlockTarget {
                        block: join,
                        args: vec![v],
                    },
                });
                self.cur = miss;
                let addr = self.binding_addr(slot);
                let v = self.load_i64(addr, 0);
                self.terminate(Terminator::Br {
                    target: BlockTarget {
                        block: join,
                        args: vec![v],
                    },
                });
                self.cur = join;
                self.def(inst, r);
            }
            Opcode::StoreGName(b) => {
                let def = &self.mm.bindings[b];
                let (slot, name) = (def.slot, def.name);
                let fused = self.gname_fused.get(&name).copied();
                let gc_free = value_tags(&self.ty(d.args[1])).subset_of(GC_FREE_TAGS);
                self.binding_store(slot, a[1], gc_free, fused);
            }
            Opcode::LoadField(name) => self.field_op(inst, name, a[0], None)?,
            Opcode::LoadSlot(name) => {
                let t = self.ty(self.f.insts[inst].args[0]);
                let o = t.obj_info().ok_or("lowering: load_slot of a non-object")?;
                let slot = mir::ops::slot_of(o, name, self.mm)
                    .ok_or("lowering: load_slot without a proven slot")?;
                let v = self.load_i64(a[0], FIXED_SLOTS_BASE + 8 * slot);
                self.def(inst, v);
            }
            Opcode::StoreField(name) => self.field_op(inst, name, a[0], Some(a[1]))?,
            Opcode::StoreSlot(name) => {
                // `field_op`'s store with TYPES kept by type: RANGES dropped
                // (no MIR claim reads it), the barriers unless a number.
                let d = &self.f.insts[inst];
                let t = self.ty(d.args[0]);
                let num =
                    matches!(self.ty(d.args[1]), MType::Val(s) if s.tags.subset_of(TagSet::NUMBER));
                let o = t.obj_info().ok_or("lowering: store_slot of a non-object")?;
                let slot = mir::ops::slot_of(o, name, self.mm)
                    .ok_or("lowering: store_slot without a proven slot")?;
                let off = FIXED_SLOTS_BASE + 8 * slot;
                let w = self.load_i32(a[0], OBJ_CLASS_IDX_OFFSET);
                self.clear_bits(a[0], w, CLASS_WORD_RANGES);
                if !num {
                    self.pre_barrier(a[0], off);
                }
                self.store_i64(a[0], off, a[1]);
                if !num {
                    let sv = self.i32c(slot);
                    self.post_barrier(self.h.post_write_barrier, a[0], sv, a[1]);
                }
            }
            Opcode::JsBoxThis => {
                // Allocates (a primitive's wrapper); runs no user code.
                let live = self.live_across(inst);
                let (ok, r) = self.gc_call(self.h.box_nonstrict_this, &[a[0]], &live)?;
                self.epoch_same = None;
                let t = self.edge(inst, 0, &[r])?;
                let e = self.edge(inst, 1, &[])?;
                self.cond_br(ok, t, e);
            }
            Opcode::JsBindGName(name) => {
                if let Some(&bid) = self.gname_bids.get(&name) {
                    self.gname_bind_arms(inst, bid)?;
                }
                let at = self.atom(name);
                self.js_call(inst, self.h.bind_unqualified_gname, &[at], false)?;
            }
            Opcode::JsSetName(name, strict) => {
                if let Some(&bid) = self.gname_bids.get(&name) {
                    let fused = self.gname_fused.get(&name).copied();
                    self.gname_set_arms(inst, bid, a[1], fused)?;
                }
                let at = self.atom(name);
                let sv = self.i32c(u32::from(strict));
                self.js_call(inst, self.h.set_name, &[a[0], at, a[1], sv], false)?;
            }
            Opcode::ConstStr(_) if self.lazy_strs.contains_key(&self.f.insts[inst].results[0]) => {}
            Opcode::ConstStr(name) => {
                // The atom itself, from the startup-filled atom table (a
                // pinned atom never moves):
                // one load, no allocation.
                let id = self.atoms.intern_chars(self.mm.atoms[name].chars());
                let slot = self.i32c(self.h.atom_table_slot);
                let tbl = self.load_i32(slot, 0);
                let v = self.load_i32(tbl, 4 * id);
                self.def(inst, v);
            }
            op => {
                return Err(format!(
                    "lowering: {} is not lowered yet",
                    mir::print::mnemonic(&op)
                ))
            }
        }
        Ok(())
    }
}
