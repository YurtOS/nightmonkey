//! Baseline code generation: one pass over the bytecode, each op lowered to
//! a direct call to one runtime helper (trivial ops inline), with every JS
//! value in the NightStack frame (`layout`).
//!
//! No JS value lives in SSA between ops. An op loads its inputs from their
//! operand slots, calls its helper with `top` just above the operand
//! stack at the op's entry depth (so its inputs stay rooted), and stores its
//! result back to a slot. After a may-GC call nothing loaded before it is
//! reused: a moving GC updates the rooted slots, not SSA copies, so every
//! post-call use reloads from memory.
//!
//! Control flow follows the bytecode: one waffle block per basic block
//! (`layout::leaders`), no block params. Exceptions branch from the
//! throwing op to its statically known handler, running the unwinding the
//! interpreter's `ProcessTryNotes` does (iterator closes, env pops) on the
//! way.

use std::collections::{BTreeMap, BTreeSet};

use waffle::entity::EntityRef;
use waffle::{
    Block, BlockTarget, Func, FunctionBody, MemoryArg, Operator, Terminator, Type, Value, ValueDef,
};

use super::layout::{
    self, FrameLayout, ResumeMode, ResumeWord, StackDepths, ARGC_FLAGS, ARGC_ONRAMP_BIT,
    ARGC_RESUME_BIT, ERR_DEOPT, ONRAMP_BACKOFF, ONRAMP_COUNT_MASK, ONRAMP_LEVEL_MAX,
    ONRAMP_LEVEL_SHIFT,
};
use crate::bytecode::{BytecodeParser, JSOp, Script, TryNoteKind};
use crate::ids::{Pc, ScriptId, Site};
use crate::source::{ScopeData, SourceObject};
use crate::wasm::mir::abi::{
    BINOP_BITAND, BINOP_BITNOT, BINOP_BITOR, BINOP_BITXOR, BINOP_DEC, BINOP_DIV, BINOP_INC,
    BINOP_LSH, BINOP_MOD, BINOP_MUL, BINOP_RSH, BINOP_SUB, BINOP_URSH, CLASS_WORD_SENTINEL,
    CLASS_WORD_SLOTS, CMP_EQ, CMP_GE, CMP_GT, CMP_LE, CMP_LT, CMP_NE, CMP_STRICTEQ, CMP_STRICTNE,
    EARLY_KEY_MAX, EARLY_KEY_SHIFT, ELEMENTS_INITLEN_BACK, FIXED_SLOTS_BASE, FLAGS_ALL,
    FUNC_ENV_SLOT_OFFSET, FUNC_SCRIPT_SLOT_OFFSET, IC_WAY_ADDR_PLACEHOLDER, INIT_ATTR_ENUMERATE,
    INIT_ATTR_HIDDEN, INIT_ATTR_LOCKED, NO_NSLOTS, OBJ_CLASS_IDX_OFFSET, OBJ_ELEMENTS_OFFSET,
    SHAPE_IMMUTABLE_FLAGS_OFFSET, SHAPE_IS_NATIVE_BIT, SHAPE_OFFSET,
};
use crate::wasm::mir::stamp::{
    construct_nslots, typed_alloc_word, typed_keep_bits, typed_restamp_args,
};
use crate::wasm::translate::{
    AtomTable, Helpers, TranslateCtx, INLINE_IC_STRIDE, MAGIC_ELEMENTS_HOLE,
    MAGIC_GENERATOR_CLOSING, MAGIC_IS_CONSTRUCTING, MAGIC_NO_ITER_VALUE,
    MAGIC_UNINITIALIZED_LEXICAL, TAG_BIGINT_HI, TAG_BOOLEAN, TAG_CLEAR, TAG_INT32, TAG_MAGIC,
    TAG_NULL, TAG_OBJECT, TAG_STRING, TAG_SYMBOL, TAG_UNDEFINED,
};

mod calls;
mod emit;
mod exceptions;
mod numeric;
mod op_access;
mod op_arith;
mod op_async;
mod op_consts;
mod op_control;
mod op_locals;
mod op_objects;
mod op_rare;
mod op_values;
mod resume;
mod stamps;

/// The most frame a baseline body may use above `sp`, in bytes. Runtime
/// entries guarantee 64 KiB of NightStack headroom beyond the callee's
/// actuals (`kNightStackHeadroomSlots`), and nothing checks a body's own
/// frame against it, so a larger frame is declined rather than risked.
const MAX_FRAME_BYTES: u32 = 48 * 1024;

const UNDEF: u64 = TAG_UNDEFINED << 32;

/// The resume word's "not resuming" payload: no encoded pc is `u32::MAX`
/// (`ResumeWord::MAX_PC` leaves the top bit clear).
const RESUME_NONE: u32 = u32::MAX;

/// One try-note-driven unwinding step on the way to a handler.
#[derive(Clone, Copy)]
enum Close {
    /// A for-in iterator at operand depth `d - 1`.
    ForIn(u32),
    /// A destructuring iterator at `d - 2`, its done flag at `d - 1`.
    Destructuring(u32),
}

pub(super) struct Gen<'a> {
    ctx: &'a TranslateCtx<'a>,
    sid: ScriptId,
    /// A layout constructor's ctor-exit stamp (`night_runtime_ctor_stamp`
    /// arguments after `this`): without it no object a baseline or MIR
    /// constructor builds is ever stamped, and every layout guard misses.
    ctor_stamp: Option<[u32; 3]>,
    /// An init delegate's restamp (`night_runtime_ctor_restamp`'s
    /// arguments after `this`).
    ctor_restamp: Option<[u32; 7]>,
    /// A fill script's restamp of a formal at its returns
    /// (`arg_restamps_in`): the formal and the restamp arguments.
    arg_restamp: Option<(u32, [u32; 7])>,
    atoms: &'a mut AtomTable,
    script: &'a Script,
    is_global: bool,
    h: Helpers,
    pub(super) body: FunctionBody,
    layout: FrameLayout,
    depths: StackDepths,
    needs_env: bool,
    env_scopes: Vec<u32>,
    cur: Block,
    /// Whether `cur` is open (reachable and not yet terminated).
    live: bool,
    cx: Value,
    sp: Value,
    vp: Value,
    argc: Value,
    retval_out: Value,
    new_target: Value,
    blocks: BTreeMap<Pc, Block>,
    error_blk: Option<Block>,
    finally_landing: BTreeMap<Pc, Block>,
    exc_targets: BTreeMap<(Pc, u32), BlockTarget>,
    /// Generator or async body: suspend/resume through the generator
    /// object (`EnterNightResume` protocol).
    is_gen: bool,
    /// Resume labels: (resume index, landing pc, saved operand depth).
    gen_resume: Vec<(u32, Pc, u32)>,
    /// Loop intervals `[header, end)`, from the bytecode's back edges.
    loops: Vec<(Pc, Pc)>,
    /// Every pc a resume may land on (`docs/BASELINE.md` §4).
    resume_targets: BTreeSet<Pc>,
    /// Every resume word a resume may carry: the generator landings and
    /// the MIR body's exit and throw pcs (`resumes`).
    resume_words: BTreeSet<ResumeWord>,
    /// Resume words the caller asked for (a MIR body's exits), entered
    /// through the `ARGC_RESUME_BIT` fork.
    ext_resumes: Vec<ResumeWord>,
    /// `argc` as passed, flags included.
    argc_raw: Value,
    /// The ABI's script parameter, passed through to onramp calls.
    script_param: Value,
    /// Loop headers with a MIR onramp root (`docs/BASELINE.md` §7), with
    /// the resume words that root reaches.
    onramps: BTreeMap<Pc, Vec<ResumeWord>>,
    /// Direct-call placeholders for the script's MIR body.
    pub(super) main_calls: Vec<Value>,
    /// The resume dispatch (`ARGC_RESUME_BIT` entry).
    resume_disp: Option<Block>,
    /// Loop headers that dispatch resumes: header pc -> (dispatch block,
    /// the header's own code block). Every branch to the header goes to the
    /// dispatch block, which is therefore the loop's header node.
    dispatch: BTreeMap<Pc, (Block, Block)>,
    /// The resume entry, if the body has one (generators).
    gen_dispatch_blk: Option<Block>,
    /// Adapter-offset placeholders of direct calls (`Outcome::Compiled`'s
    /// `body_off_patches`).
    pub(super) body_off_patches: Vec<Value>,
    /// Property-IC way-address placeholders, with their row offsets
    /// (`Outcome::Compiled::prop_ic_patches`).
    pub(super) prop_ic_patches: Vec<(Value, u32)>,
    /// The op being lowered and its entry depth.
    pc: Pc,
    d: u32,
}

type R<T> = Result<T, String>;

/// An inline int32 operation: the raw i32 result of two raw i32 operands,
/// or a branch to the slow block (the op's helper call) when the result is
/// not an int32.
type Int32Op<'a> = fn(&mut Gen<'a>, Value, Value, Block) -> Value;

/// `number_arms`' int32 arm: raw i32 operands to a boxed result, or a
/// branch to the block given (the next arm).
type IntArm<'s, 'a> = dyn Fn(&mut Gen<'a>, &[Value], Block) -> Value + 's;

/// `number_arms`' number arm: f64 operands to a boxed result.
type NumArm<'s, 'a> = dyn Fn(&mut Gen<'a>, &[Value]) -> Value + 's;

/// `JS::GenericNaN()`'s bits.
const CANONICAL_NAN_BITS: u64 = 0x7FF8_0000_0000_0000;

/// The `night_runtime_binop` kind of an arithmetic op.
fn binop_kind(op: JSOp) -> u32 {
    match op {
        JSOp::Sub => BINOP_SUB,
        JSOp::Mul => BINOP_MUL,
        JSOp::Div => BINOP_DIV,
        JSOp::Mod => BINOP_MOD,
        JSOp::BitAnd => BINOP_BITAND,
        JSOp::BitOr => BINOP_BITOR,
        JSOp::BitXor => BINOP_BITXOR,
        JSOp::Lsh => BINOP_LSH,
        JSOp::Rsh => BINOP_RSH,
        JSOp::Ursh => BINOP_URSH,
        JSOp::Inc => BINOP_INC,
        JSOp::Dec => BINOP_DEC,
        JSOp::BitNot => BINOP_BITNOT,
        _ => unreachable!("{op:?} is not a binop kind"),
    }
}

impl<'a> Gen<'a> {
    pub(super) fn new(
        ctx: &'a TranslateCtx<'a>,
        sid: ScriptId,
        atoms: &'a mut AtomTable,
        script: &'a Script,
        is_global: bool,
        body: FunctionBody,
        depths: StackDepths,
        resumes: &[ResumeWord],
        onramps: &[(Pc, Vec<ResumeWord>)],
    ) -> R<Gen<'a>> {
        let layout = FrameLayout::of(script);
        let needs_env = needs_env(script);
        let frame_top = layout.top(depths.max + 3);
        if frame_top > MAX_FRAME_BYTES {
            return Err(format!("frame too large ({frame_top} bytes)"));
        }
        let entry = body.entry;
        let p = |i: usize| body.blocks[entry].params[i].1;
        let (cx, sp, argc, retval_out, script_param, new_target) =
            (p(0), p(1), p(2), p(3), p(4), p(5));
        Ok(Gen {
            ctx,
            sid,
            ctor_stamp: ctx.stamp_ctors_in.get(&sid).map(|si| {
                [
                    si.layout_id,
                    u32::try_from(si.fields.len()).unwrap(),
                    typed_keep_bits(ctx, si),
                ]
            }),
            ctor_restamp: ctx
                .deleg_restamps_in
                .get(&sid)
                .and_then(|si| typed_restamp_args(ctx, si)),
            arg_restamp: ctx
                .arg_restamps_in
                .get(&sid)
                .and_then(|(f, si)| Some((*f, typed_restamp_args(ctx, si)?))),
            atoms,
            script,
            is_global,
            h: ctx.helpers,
            body,
            layout,
            depths,
            needs_env,
            env_scopes: env_scopes_of(script),
            cur: entry,
            live: true,
            cx,
            sp,
            vp: sp,
            argc,
            retval_out,
            new_target,
            blocks: BTreeMap::new(),
            error_blk: None,
            finally_landing: BTreeMap::new(),
            exc_targets: BTreeMap::new(),
            is_gen: script.is_generator_or_async,
            gen_resume: vec![],
            loops: vec![],
            resume_targets: BTreeSet::new(),
            resume_words: BTreeSet::new(),
            ext_resumes: resumes.to_vec(),
            argc_raw: argc,
            script_param,
            onramps: onramps.iter().cloned().collect(),
            main_calls: vec![],
            resume_disp: None,
            dispatch: BTreeMap::new(),
            gen_dispatch_blk: None,
            body_off_patches: vec![],
            prop_ic_patches: vec![],
            pc: Pc::new(0),
            d: 0,
        })
    }

    fn prologue(&mut self) {
        let nargs = u32::from(self.script.nargs);
        // Formals the caller did not pass read as undefined.
        let undef = self.i64c(UNDEF);
        for i in 0..nargs {
            let off = self.layout.arg(i);
            let cur = self.load_i64(self.sp, off);
            let iv = self.i32c(i);
            let keep = self.binop(Operator::I32LtU, iv, self.argc, Type::I32);
            let v = self.select(Type::I64, cur, undef, keep);
            self.store_i64(self.sp, off, v);
        }
        // Every slot is a valid Value before the first helper call.
        for j in 0..self.layout.nlocals {
            self.store_i64(self.vp, self.layout.local(j), undef);
        }
        if self.layout.has_env {
            self.store_i64(self.vp, self.layout.env(), undef);
        }
        if self.layout.has_args_obj {
            self.store_i64(self.vp, self.layout.args_obj(), undef);
        }
        if self.layout.has_new_target {
            self.store_i64(self.vp, self.layout.new_target(), self.new_target);
        }
        self.store_i64(self.vp, self.layout.rval(), undef);
        let resume = self.i64c(TAG_INT32 << 32);
        self.store_i64(self.vp, self.layout.resume(), resume);
        self.store_i64(self.vp, self.layout.backoff(), resume);
        // Exceptions from the prologue have no handler: pc 0, depth 0.
        (self.pc, self.d) = (Pc::new(0), 0);
        if self.needs_env {
            if self.env_is_plain() {
                let callee = self.load_i64(self.sp, 0);
                let p = self.unop(Operator::I32WrapI64, callee, Type::I32);
                let env = self.load_i64(p, FUNC_ENV_SLOT_OFFSET);
                self.set_env(env);
            } else {
                let script = self.script_ptr();
                let es = self.h.env_setup;
                let no_cell = self.i32c(0);
                let env = self.rt(es, &[self.sp, script, no_cell]);
                self.set_env(env);
            }
        }
        if self.script.has_mapped_args {
            let args = if self.needs_env {
                let env = self.env();
                let f = self.h.arguments_env;
                self.rt(f, &[self.sp, self.argc, env])
            } else {
                let f = self.h.arguments_;
                self.rt(f, &[self.sp, self.argc])
            };
            self.store_i64(self.vp, self.layout.args_obj(), args);
        }
    }

    // --- the walk ---------------------------------------------------------------

    pub(super) fn run(&mut self) -> R<()> {
        self.loops = crate::wasm::translate::scan_loop_intervals(self.script)
            .into_iter()
            .map(|(h, e)| (Pc::new(h), Pc::new(e)))
            .collect();
        self.loops.sort();
        self.frame_regs();
        for w in self.ext_resumes.clone() {
            if self.depths.at(w.pc).is_none() {
                return Err(format!("resume at pc {} with no stack depth", w.pc));
            }
            self.resume_targets.insert(w.pc);
            self.resume_words.insert(w);
        }
        if self.is_gen {
            self.scan_resumes()?;
            // Fresh call or resume: a resume stages the generator-closing
            // magic as `this`, which no call can pass.
            let thisv = self.load_i64(self.sp, FrameLayout::THIS);
            let magic = self.i64c((TAG_MAGIC << 32) | MAGIC_GENERATOR_CLOSING);
            let is_resume = self.binop(Operator::I64Eq, thisv, magic, Type::I32);
            // A MIR body's exit from a resumed activation keeps that magic
            // `this` in the frame, and comes with `ARGC_RESUME_BIT`: it is
            // the resume word's, not the generator's.
            let bit = self.i32c(ARGC_RESUME_BIT);
            let exit = self.binop(Operator::I32And, self.argc_raw, bit, Type::I32);
            let not_exit = self.unop(Operator::I32Eqz, exit, Type::I32);
            let is_resume = self.binop(Operator::I32And, is_resume, not_exit, Type::I32);
            let disp = self.body.add_block();
            let fresh = self.body.add_block();
            self.gen_dispatch_blk = Some(disp);
            self.cond_br(is_resume, Self::goto(disp), Self::goto(fresh));
            self.cur = fresh;
            self.live = true;
        }
        let resume_blk = if self.ext_resumes.is_empty() {
            None
        } else {
            // A resume (from a MIR exit) finds the frame already written:
            // it skips the prologue and routes by the resume word.
            let bit = self.i32c(ARGC_RESUME_BIT);
            let is_resume = self.binop(Operator::I32And, self.argc_raw, bit, Type::I32);
            let disp = self.body.add_block();
            let fresh = self.body.add_block();
            self.cond_br(is_resume, Self::goto(disp), Self::goto(fresh));
            self.cur = fresh;
            self.live = true;
            Some(disp)
        };
        self.resume_disp = resume_blk;
        if !self.onramps.is_empty() && resume_blk.is_none() {
            return Err("BUG: onramps without resume words".into());
        }
        self.prologue();
        let mut leaders = layout::leaders(self.script);
        // A suspend returns, so the code after it is entered only by a
        // resume: each landing starts a block.
        leaders.extend(self.resume_targets.iter().copied());
        let len = self.script.bytecode.len();
        let mut p = self.script.parser();
        loop {
            let pc = Pc::new(u32::try_from(len - p.remaining()).unwrap());
            let Some(op) = p.next_op() else { break };
            let depth = self.depths.at(pc);
            if leaders.contains(&pc) && depth.is_some() {
                let b = self.block(pc);
                if self.live {
                    self.br(Self::goto(b));
                }
                self.cur = match self.dispatch.get(&pc) {
                    Some(&(_, code)) => code,
                    None => b,
                };
                self.live = true;
                if depth.is_some() && self.onramps.contains_key(&pc) {
                    self.onramp(pc);
                }
            }
            let (Some(d), true) = (depth, self.live) else {
                // Unreachable: skip the op.
                skip(&mut p, op);
                continue;
            };
            (self.pc, self.d) = (pc, d);
            let before = p.remaining();
            self.op(&mut p, op)?;
            if self.live {
                self.local_restamp(pc);
            }
            let consumed = before - p.remaining() + 1;
            if consumed != op.len() as usize {
                return Err(format!(
                    "BUG: {op:?} at {pc} consumed {consumed} bytes, expected {}",
                    op.len()
                ));
            }
            // A falling-through op whose successor starts a block is joined
            // at the top of the loop; a non-falling-through one has already
            // terminated.
        }
        if self.live {
            self.terminate(Terminator::Unreachable);
        }
        self.finalize_dispatch();
        self.finalize_gen_dispatch();
        if let Some(disp) = resume_blk {
            self.finalize_resume_dispatch(disp);
        }
        // Every block must end somewhere: an unterminated one would be
        // emitted as a trap. Declining is always safe; trapping is not.
        for (b, data) in self.body.blocks.entries() {
            if matches!(data.terminator, Terminator::None) {
                let pc = self.blocks.iter().find(|&(_, &x)| x == b).map(|(p, _)| *p);
                return Err(format!("BUG: unterminated block {b} (pc {pc:?})"));
            }
        }
        Ok(())
    }

    fn ret(&mut self, v: Value) {
        if let Some([layout, nfields, keep]) = self.ctor_stamp {
            let thisv = self.load_i64(self.sp, FrameLayout::THIS);
            let (l, n, k) = (self.i32c(layout), self.i32c(nfields), self.i32c(keep));
            self.call(self.h.ctor_stamp, &[thisv, l, n, k], None);
        }
        if let Some(r) = self.ctor_restamp {
            let thisv = self.load_i64(self.sp, FrameLayout::THIS);
            self.restamp(thisv, r);
        }
        if let Some((n, r)) = self.arg_restamp {
            let v = self.formal(n);
            self.restamp(v, r);
        }
        self.store_i64(self.retval_out, 0, v);
        let zero = self.i32c(0);
        let flags = self.i32c(FLAGS_ALL);
        self.terminate(Terminator::Return {
            values: vec![zero, flags],
        });
    }

    fn branch_to(&mut self, off: i32) -> BlockTarget {
        let t = self.pc.branch(off);
        Self::goto(self.block(t))
    }

    fn next_pc(&self, op: JSOp) -> Pc {
        self.pc + op.len()
    }

    /// Lower one op. Immediates are read from `p`.
    fn op(&mut self, p: &mut BytecodeParser, op: JSOp) -> R<()> {
        if self.op_consts(p, op)? {
            return Ok(());
        }
        if self.op_values(p, op)? {
            return Ok(());
        }
        if self.op_locals(p, op)? {
            return Ok(());
        }
        if self.op_arith(p, op)? {
            return Ok(());
        }
        if self.op_control(p, op)? {
            return Ok(());
        }
        if self.op_access(p, op)? {
            return Ok(());
        }
        if self.op_objects(p, op)? {
            return Ok(());
        }
        if self.op_async(p, op)? {
            return Ok(());
        }
        if self.op_rare(p, op)? {
            return Ok(());
        }
        Err(format!("BUG: unhandled op {op:?}"))
    }

    // --- inline fast paths ------------------------------------------------------

    /// An inline fast path (`docs/BASELINE.md` §3.1): `fast` runs when
    /// `cond` holds and may branch to the slow block it is given, which
    /// runs `slow`, the op's ordinary lowering. Both arms leave their result
    /// in the frame and rejoin at a fresh block with no params, so nothing
    /// either arm computes is live after the op. Each op gets its own slow
    /// block, never a shared one (fan-in; see `error_return`).
    fn fast_path(
        &mut self,
        cond: Value,
        fast: impl FnOnce(&mut Self, Block),
        slow: impl FnOnce(&mut Self),
    ) {
        let fast_b = self.body.add_block();
        let slow_b = self.body.add_block();
        let join = self.body.add_block();
        self.cond_br(cond, Self::goto(fast_b), Self::goto(slow_b));
        (self.cur, self.live) = (fast_b, true);
        fast(self, slow_b);
        self.br(Self::goto(join));
        (self.cur, self.live) = (slow_b, true);
        slow(self);
        if self.live {
            self.br(Self::goto(join));
        }
        (self.cur, self.live) = (join, true);
    }

    /// Leave the fast path for `slow` when `cond` holds.
    fn bail_if(&mut self, cond: Value, slow: Block) {
        let cont = self.body.add_block();
        self.cond_br(cond, Self::goto(slow), Self::goto(cont));
        (self.cur, self.live) = (cont, true);
    }

    /// Leave the fast path for `slow` unless `cond` holds.
    fn bail_unless(&mut self, cond: Value, slow: Block) {
        let cont = self.body.add_block();
        self.cond_br(cond, Self::goto(cont), Self::goto(slow));
        (self.cur, self.live) = (cont, true);
    }

    /// Element `key` of `obj` (an object and an int32) when it is an
    /// initialized, non-hole dense element of a native object; else
    /// branches to `slow`. Pure reads: a typed array's dense
    /// initializedLength is 0, and a proxy fails the native check before
    /// `elements_` is read.
    fn dense_element(&mut self, obj: Value, key: Value, slow: Block) -> Value {
        let objptr = self.unop(Operator::I32WrapI64, obj, Type::I32);
        let shape = self.load_i32(objptr, SHAPE_OFFSET);
        let flags = self.load_i32(shape, SHAPE_IMMUTABLE_FLAGS_OFFSET);
        let bit = self.i32c(SHAPE_IS_NATIVE_BIT);
        let native = self.binop(Operator::I32And, flags, bit, Type::I32);
        self.bail_unless(native, slow);
        let elements = self.load_i32(objptr, OBJ_ELEMENTS_OFFSET);
        let back = self.i32c(ELEMENTS_INITLEN_BACK);
        let header = self.binop(Operator::I32Sub, elements, back, Type::I32);
        let initlen = self.load_i32(header, 0);
        // Unsigned, so a negative index fails too.
        let idx = self.unop(Operator::I32WrapI64, key, Type::I32);
        let in_bounds = self.binop(Operator::I32LtU, idx, initlen, Type::I32);
        self.bail_unless(in_bounds, slow);
        let eight = self.i32c(8);
        let off = self.binop(Operator::I32Mul, idx, eight, Type::I32);
        let addr = self.binop(Operator::I32Add, elements, off, Type::I32);
        let v = self.load_i64(addr, 0);
        let hole = self.tag_eq(v, TAG_MAGIC);
        self.bail_if(hole, slow);
        v
    }

    /// A check on the top value, which stays: `helper(top value, extra...)`.
    fn check(&mut self, f: Func, extra: &[Value]) {
        let v = self.slot(self.d - 1);
        let mut args = vec![v];
        args.extend_from_slice(extra);
        self.rt(f, &args);
    }

    /// Whether the op after this one consumes a name lookup's result as
    /// `typeof` does (so an unbound name reads as undefined, not a
    /// ReferenceError).
    fn next_is_typeof(&self, op: JSOp) -> bool {
        let next = usize::try_from(self.next_pc(op).get()).unwrap();
        self.script
            .bytecode
            .get(next)
            .and_then(|&b| JSOp::from_byte(b))
            .is_some_and(|o| matches!(o, JSOp::Typeof | JSOp::TypeofEq))
    }

    fn table_switch(&mut self, p: &mut BytecodeParser) -> R<()> {
        let default_off = p.next_int32().unwrap();
        let low = p.next_int32().unwrap();
        let high = p.next_int32().unwrap();
        let first_resume = p.next_uint24().unwrap();
        let d = self.d;
        let v = self.slot(d - 1);
        // An int32 or an int32-valued double selects a case; anything else
        // (including -0's tag-free friends) takes the default.
        let low_v = self.i32c(low as u32);
        let oob = self.i32c(u32::MAX);
        let is_int = self.tag_eq(v, TAG_INT32);
        let payload = self.unop(Operator::I32WrapI64, v, Type::I32);
        let v_int = self.binop(Operator::I32Sub, payload, low_v, Type::I32);
        let tag = self.tag_of(v);
        let int_tag = self.i32c(TAG_INT32 as u32);
        let is_dbl = self.binop(Operator::I32LtU, tag, int_tag, Type::I32);
        let f = self.unop(Operator::F64ReinterpretI64, v, Type::F64);
        let ti = self.unop(Operator::I32TruncSatF64S, f, Type::I32);
        let back = self.unop(Operator::F64ConvertI32S, ti, Type::F64);
        let exact = self.binop(Operator::F64Eq, back, f, Type::I32);
        let v_dbl = self.binop(Operator::I32Sub, ti, low_v, Type::I32);
        let v_dbl = self.select(Type::I32, v_dbl, oob, exact);
        let v_other = self.select(Type::I32, v_dbl, oob, is_dbl);
        let value = self.select(Type::I32, v_int, v_other, is_int);
        let count = usize::try_from((i64::from(high) - i64::from(low) + 1).max(0)).unwrap();
        let mut targets = Vec::with_capacity(count);
        for i in 0..count {
            let ri = usize::try_from(first_resume).unwrap() + i;
            let target_pc = *self
                .script
                .resume_offsets
                .get(ri)
                .ok_or_else(|| format!("TableSwitch resume index {ri} out of range"))?;
            targets.push(Self::goto(self.block(target_pc)));
        }
        let default = self.branch_to(default_off);
        self.terminate(Terminator::Select {
            value,
            targets,
            default,
        });
        Ok(())
    }
}

/// Whether the body keeps an env chain in its frame: it has env ops, or an
/// op whose helper reads the chain (direct eval, `EnvCallee`, module and
/// resource-management ops).
/// Whether `script`'s activation environment is its callee's own
/// environment: a function body scope with no environment of its own
/// (no call object, no named-lambda scope). The prologue then just loads
/// it from the callee.
pub(crate) fn env_is_plain(source: &crate::source::Source, script: &Script) -> bool {
    let Some(bs) = script.body_scope else {
        return false;
    };
    let SourceObject::Scope(ScopeData {
        kind: 0,
        has_environment: false,
        enclosing,
        ..
    }) = source.object(bs)
    else {
        return false;
    };
    // A named lambda's own-name environment is built by the setup.
    !enclosing.is_some_and(|e| {
        matches!(
            source.object(e),
            SourceObject::Scope(ScopeData {
                is_named_lambda: true,
                has_environment: true,
                ..
            })
        )
    })
}

pub(crate) fn needs_env(script: &Script) -> bool {
    crate::wasm::translate::uses_env_ops(script)
        || script.parser().opcodes().any(|op| {
            matches!(
                op,
                JSOp::Eval
                    | JSOp::StrictEval
                    | JSOp::SpreadEval
                    | JSOp::StrictSpreadEval
                    | JSOp::NonSyntacticGlobalThis
                    | JSOp::EnvCallee
                    | JSOp::GetImport
                    | JSOp::AddDisposable
                    | JSOp::TakeDisposeCapability
            )
        })
}

fn skip(p: &mut BytecodeParser, op: JSOp) {
    let imm = usize::try_from(op.len()).unwrap() - 1;
    if imm > 0 {
        p.advance(imm).expect("operand bytes present");
    }
}

/// The scopes of `script` that own an environment object: the operands of
/// the ops that push one.
fn env_scopes_of(script: &Script) -> Vec<u32> {
    let mut out = Vec::new();
    let mut p = script.parser();
    while let Some(op) = p.next_op() {
        let len = usize::try_from(op.len()).unwrap();
        if matches!(
            op,
            JSOp::PushLexicalEnv | JSOp::PushClassBodyEnv | JSOp::PushVarEnv | JSOp::EnterWith
        ) {
            match p.next_uint32() {
                Some(idx) => out.push(idx),
                None => break,
            }
            if len > 5 && p.advance(len - 5).is_none() {
                break;
            }
        } else if len > 1 && p.advance(len - 1).is_none() {
            break;
        }
    }
    out
}
