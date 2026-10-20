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
        use JSOp::*;
        let d = self.d;
        let h = self.h;
        match op {
            Nop | Lineno | JumpTarget | LoopHead | Debugger | NopDestructuring | NopIsAssignOp
            | TryDestructuring | DebugLeaveLexicalEnv | Finally | Try | DebugCheckSelfHosted => {
                skip(p, op);
            }

            // --- constants ---
            Undefined => self.push_const(d, UNDEF),
            Null => self.push_const(d, TAG_NULL << 32),
            False => self.push_const(d, TAG_BOOLEAN << 32),
            True => self.push_const(d, (TAG_BOOLEAN << 32) | 1),
            Zero => self.push_const(d, TAG_INT32 << 32),
            One => self.push_const(d, (TAG_INT32 << 32) | 1),
            Int8 => {
                let i = p.next_int8().unwrap();
                self.push_const(d, (TAG_INT32 << 32) | u64::from(i32::from(i) as u32));
            }
            Uint16 => {
                let i = p.next_uint16().unwrap();
                self.push_const(d, (TAG_INT32 << 32) | u64::from(i));
            }
            Uint24 => {
                let i = p.next_uint24().unwrap();
                self.push_const(d, (TAG_INT32 << 32) | u64::from(i));
            }
            Int32 => {
                let i = p.next_int32().unwrap();
                self.push_const(d, (TAG_INT32 << 32) | u64::from(i as u32));
            }
            Double => {
                let bits = p.next_uint64().unwrap();
                self.push_const(d, bits);
            }
            Hole => self.push_const(d, (TAG_MAGIC << 32) | MAGIC_ELEMENTS_HOLE),
            Uninitialized => self.push_const(d, (TAG_MAGIC << 32) | MAGIC_UNINITIALIZED_LEXICAL),
            IsConstructing => self.push_const(d, (TAG_MAGIC << 32) | MAGIC_IS_CONSTRUCTING),
            Void => self.push_const(d - 1, UNDEF),
            String => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let av = self.i32c(a);
                let r = self.rt(h.string, &[av]);
                self.set_slot(d, r);
            }
            GetIntrinsic => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let av = self.i32c(a);
                let r = self.rt(h.get_intrinsic, &[av]);
                self.set_slot(d, r);
            }
            Object | CallSiteObj => {
                let idx = p.next_uint32().unwrap();
                let iv = self.i32c(idx);
                let script = self.script_ptr();
                let r = self
                    .call(h.object, &[self.cx, script, iv], Some(Type::I64))
                    .unwrap();
                self.set_slot(d, r);
            }
            Symbol => {
                let code = p.next_uint8().unwrap();
                let c = self.i32c(u32::from(code));
                let r = self.call(h.symbol, &[self.cx, c], Some(Type::I64)).unwrap();
                self.set_slot(d, r);
            }
            RegExp => {
                let idx = p.next_uint32().unwrap();
                let iv = self.i32c(idx);
                let script = self.script_ptr();
                let r = self.rt(h.regexp, &[script, iv]);
                self.set_slot(d, r);
            }
            BuiltinObject => {
                let kind = p.next_uint8().unwrap();
                let k = self.i32c(u32::from(kind));
                let r = self.rt(h.builtin_object, &[k]);
                self.set_slot(d, r);
            }

            // --- checks and coercions ---
            IsNullOrUndefined => {
                let v = self.slot(d - 1);
                let u = self.tag_eq(v, TAG_UNDEFINED);
                let n = self.tag_eq(v, TAG_NULL);
                let b = self.binop(Operator::I32Or, u, n, Type::I32);
                let r = self.boxed_bool(b);
                self.set_slot(d, r);
            }
            ToString => self.unary(h.tostring, d),
            ToPropertyKey => self.unary(h.to_property_key, d),
            ToNumeric => {
                // Identity on a number (int32 or double).
                let v = self.slot(d - 1);
                let is_num = self.is_number(v);
                self.fast_path(is_num, |_, _| {}, |g| g.unary(h.tonumeric, d));
            }
            Pos => {
                // Identity on a number.
                let v = self.slot(d - 1);
                let is_num = self.is_number(v);
                self.fast_path(is_num, |_, _| {}, |g| g.unary(h.pos, d));
            }
            ImplicitThis => self.unary(h.implicit_this, d),
            ObjWithProto => self.unary(h.obj_with_proto, d),
            OptimizeSpreadCall => self.unary(h.optimize_spread_call, d),
            CheckObjCoercible => self.check(h.check_obj_coercible, &[]),
            CheckClassHeritage => self.check(h.check_class_heritage, &[]),
            CheckThis => self.check(h.check_this, &[]),
            CheckThisReinit => self.check(h.check_this_reinit, &[]),
            CheckIsObj => {
                let kind = p.next_uint8().unwrap();
                let k = self.i32c(u32::from(kind));
                self.check(h.check_is_obj, &[k]);
            }
            CheckLexical | CheckAliasedLexical => {
                skip(p, op);
                let script = self.script_ptr();
                let pcv = self.i32c(self.pc.get());
                self.check(h.check_lexical, &[script, pcv]);
            }
            CheckReturn => {
                let thisv = self.slot(d - 1);
                let rval = self.load_i64(self.vp, self.layout.rval());
                let r = self.rt(h.check_return, &[thisv, rval]);
                self.set_slot(d - 1, r);
            }
            ThrowSetConst => {
                skip(p, op);
                let script = self.script_ptr();
                let pcv = self.i32c(self.pc.get());
                self.rt_throw(h.throw_set_const, &[script, pcv]);
            }
            ThrowMsg => {
                let kind = p.next_uint8().unwrap();
                let k = self.i32c(u32::from(kind));
                self.rt_throw(h.throw_msg, &[k]);
            }
            Throw => {
                let v = self.slot(d - 1);
                self.rt_throw(h.throw, &[v]);
            }
            ThrowWithStack => {
                let v = self.slot(d - 2);
                let s = self.slot(d - 1);
                self.rt_throw(h.throw_with_stack, &[v, s]);
            }

            // --- frame values ---
            Callee => {
                let c = self.load_i64(self.sp, 0);
                self.set_slot(d, c);
            }
            NewTarget => {
                let nt = self.load_i64(self.vp, self.layout.new_target());
                self.set_slot(d, nt);
            }
            FunctionThis => self.function_this(),
            GlobalThis => {
                let r = self.rt(h.global_this, &[]);
                self.set_slot(d, r);
            }
            ArgumentsLength => {
                let r = self.boxed_int32(self.argc);
                self.set_slot(d, r);
            }
            GetActualArg => {
                let i = self.slot(d - 1);
                let i = self.unop(Operator::I32WrapI64, i, Type::I32);
                let eight = self.i32c(8);
                let off = self.binop(Operator::I32Mul, i, eight, Type::I32);
                let a = self.binop(Operator::I32Add, self.sp, off, Type::I32);
                let v = self.load_i64(a, FrameLayout::ARGS);
                self.set_slot(d - 1, v);
            }
            Rest => {
                let nformal = u32::from(self.script.nargs).saturating_sub(1);
                let nf = self.i32c(nformal);
                let r = self.rt(h.rest, &[self.sp, self.argc, nf]);
                self.set_slot(d, r);
            }
            Arguments => self.arguments(),
            GetArg | GetFrameArg => {
                let n = u32::from(p.next_uint16().unwrap());
                let v = if op == GetArg && self.script.has_mapped_args {
                    let obj = self.load_i64(self.vp, self.layout.args_obj());
                    let i = self.i32c(n);
                    self.call(h.get_mapped_arg, &[obj, i], Some(Type::I64))
                        .unwrap()
                } else {
                    self.load_i64(self.sp, self.layout.arg(n))
                };
                self.set_slot(d, v);
            }
            SetArg => {
                let n = u32::from(p.next_uint16().unwrap());
                let v = self.slot(d - 1);
                if self.script.has_mapped_args {
                    let obj = self.load_i64(self.vp, self.layout.args_obj());
                    let i = self.i32c(n);
                    self.call(h.set_mapped_arg, &[obj, i, v], None);
                } else {
                    self.store_i64(self.sp, self.layout.arg(n), v);
                }
            }
            GetLocal => {
                let n = p.next_uint24().unwrap();
                let v = self.load_i64(self.vp, self.layout.local(n));
                self.set_slot(d, v);
            }
            SetLocal | InitLexical => {
                let n = p.next_uint24().unwrap();
                let v = self.slot(d - 1);
                self.store_i64(self.vp, self.layout.local(n), v);
            }
            GetRval => {
                let v = self.load_i64(self.vp, self.layout.rval());
                self.set_slot(d, v);
            }
            SetRval => {
                let v = self.slot(d - 1);
                self.store_i64(self.vp, self.layout.rval(), v);
            }

            // --- environments ---
            GetAliasedVar | GetAliasedDebugVar => {
                let hops = u32::from(p.next_uint16().unwrap());
                let s = p.next_uint24().unwrap();
                let env = self.env();
                let (hv, sv) = (self.i32c(hops), self.i32c(s));
                let v = self
                    .call(h.get_aliased, &[self.cx, env, hv, sv], Some(Type::I64))
                    .unwrap();
                self.set_slot(d, v);
            }
            SetAliasedVar | InitAliasedLexical => {
                let hops = u32::from(p.next_uint16().unwrap());
                let s = p.next_uint24().unwrap();
                let env = self.env();
                let (hv, sv) = (self.i32c(hops), self.i32c(s));
                let v = self.slot(d - 1);
                self.call(h.set_aliased, &[self.cx, env, hv, sv, v], None);
            }
            Lambda => {
                let fi = p.next_uint32().unwrap();
                let env = self.env();
                let script = self.script_ptr();
                let f = self.i32c(fi);
                let no_cell = self.i32c(0);
                let r = self.rt(h.lambda, &[env, script, f, no_cell]);
                self.set_slot(d, r);
            }
            FunWithProto => {
                let fi = p.next_uint32().unwrap();
                let proto = self.slot(d - 1);
                let env = self.env();
                let script = self.script_ptr();
                let f = self.i32c(fi);
                let r = self.rt(h.fun_with_proto, &[env, proto, script, f]);
                self.set_slot(d - 1, r);
            }
            PushLexicalEnv | PushClassBodyEnv | PushVarEnv => {
                skip(p, op);
                let f = match op {
                    PushLexicalEnv => h.push_lexical_env,
                    PushClassBodyEnv => h.push_class_body_env,
                    _ => h.push_var_env,
                };
                let env = self.env();
                let script = self.script_ptr();
                let pcv = self.i32c(self.pc.get());
                let r = self.rt(f, &[env, script, pcv]);
                self.set_env(r);
            }
            EnterWith => {
                skip(p, op);
                let v = self.slot(d - 1);
                let env = self.env();
                let script = self.script_ptr();
                let pcv = self.i32c(self.pc.get());
                let r = self.rt(h.enter_with, &[env, v, script, pcv]);
                self.set_env(r);
            }
            FreshenLexicalEnv | RecreateLexicalEnv => {
                skip(p, op);
                let f = if op == FreshenLexicalEnv {
                    h.freshen_lexical_env
                } else {
                    h.recreate_lexical_env
                };
                let env = self.env();
                let r = self.rt(f, &[env]);
                self.set_env(r);
            }
            PopLexicalEnv | LeaveWith => {
                let env = self.env();
                let ptr = self.unop(Operator::I32WrapI64, env, Type::I32);
                let enclosing = self.load_i64(ptr, FIXED_SLOTS_BASE);
                self.set_env(enclosing);
            }
            SetFunName => {
                let prefix = p.next_uint8().unwrap();
                let name = self.slot(d - 1);
                let fun = self.slot(d - 2);
                let k = self.i32c(u32::from(prefix));
                self.rt(h.set_fun_name, &[fun, name, k]);
            }
            InitGLexical => {
                skip(p, op);
                let v = self.slot(d - 1);
                let script = self.script_ptr();
                let pcv = self.i32c(self.pc.get());
                self.rt(h.init_glexical, &[v, script, pcv]);
            }
            GetName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let for_typeof = self.next_is_typeof(op);
                let env = self.env();
                let (av, tv) = (self.i32c(a), self.i32c(u32::from(for_typeof)));
                let r = self.rt(h.get_name, &[env, av, tv]);
                self.set_slot(d, r);
            }
            BindName | BindUnqualifiedName | DelName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let f = match op {
                    BindName => h.bind_name,
                    BindUnqualifiedName => h.bind_unqualified_name,
                    _ => h.del_name,
                };
                let env = self.env();
                let av = self.i32c(a);
                let r = self.rt(f, &[env, av]);
                self.set_slot(d, r);
            }
            GetBoundName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let env = self.slot(d - 1);
                let av = self.i32c(a);
                let r = self.rt(h.get_bound_name, &[env, av]);
                self.set_slot(d - 1, r);
            }
            BindVar => {
                let env = self.env();
                let r = self.rt(h.bind_var, &[env]);
                self.set_slot(d, r);
            }
            GlobalOrEvalDeclInstantiation => {
                let idx = p.next_uint32().unwrap();
                let script = self.script_ptr();
                let iv = self.i32c(idx);
                self.rt(h.global_decl_instantiation, &[script, iv]);
            }
            GetGName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let for_typeof = self.next_is_typeof(op);
                let (av, tv) = (self.i32c(a), self.i32c(u32::from(for_typeof)));
                let r = self.rt(h.get_gname, &[av, tv]);
                self.set_slot(d, r);
            }
            BindUnqualifiedGName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let av = self.i32c(a);
                let r = self.rt(h.bind_unqualified_gname, &[av]);
                self.set_slot(d, r);
            }
            SetGName | StrictSetGName | SetName | StrictSetName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let strict = matches!(op, StrictSetGName | StrictSetName);
                let env = self.slot(d - 2);
                let v = self.slot(d - 1);
                let (av, sv) = (self.i32c(a), self.i32c(u32::from(strict)));
                self.rt(h.set_name, &[env, av, v, sv]);
                let v = self.slot(d - 1);
                self.set_slot(d - 2, v);
            }

            // --- stack manipulation ---
            Pop => {}
            PopN => {
                p.next_uint16().unwrap();
            }
            Dup => {
                let v = self.slot(d - 1);
                self.set_slot(d, v);
            }
            Dup2 => {
                let a = self.slot(d - 2);
                let b = self.slot(d - 1);
                self.set_slot(d, a);
                self.set_slot(d + 1, b);
            }
            DupAt => {
                let n = p.next_uint24().unwrap();
                let v = self.slot(d - 1 - n);
                self.set_slot(d, v);
            }
            Swap => {
                let a = self.slot(d - 2);
                let b = self.slot(d - 1);
                self.set_slot(d - 2, b);
                self.set_slot(d - 1, a);
            }
            Pick => {
                // Move slot d-1-n to the top, shifting the ones above down.
                let n = u32::from(p.next_uint8().unwrap());
                let v = self.slot(d - 1 - n);
                for k in (d - 1 - n)..(d - 1) {
                    let w = self.slot(k + 1);
                    self.set_slot(k, w);
                }
                self.set_slot(d - 1, v);
            }
            Unpick => {
                // Move the top down to slot d-1-n, shifting the ones there up.
                let n = u32::from(p.next_uint8().unwrap());
                let v = self.slot(d - 1);
                for k in ((d - 1 - n)..(d - 1)).rev() {
                    let w = self.slot(k);
                    self.set_slot(k + 1, w);
                }
                self.set_slot(d - 1 - n, v);
            }

            // --- arithmetic and comparison ---
            // Inline arms as in the portable baseline interpreter: int32
            // operands, then any numbers (as doubles), then the helper
            // (`number_arms`).
            Add | Sub | Mul | Div | Mod | BitAnd | BitOr | BitXor | Lsh | Rsh | Ursh => {
                let a = self.slot(d - 2);
                let b = self.slot(d - 1);
                let int: Option<Int32Op<'a>> = match op {
                    Add => Some(Self::add_i32),
                    Sub => Some(Self::sub_i32),
                    Mul => Some(Self::mul_i32),
                    Mod => Some(Self::mod_i32),
                    BitAnd => Some(|g, x, y, _| g.binop(Operator::I32And, x, y, Type::I32)),
                    BitOr => Some(|g, x, y, _| g.binop(Operator::I32Or, x, y, Type::I32)),
                    BitXor => Some(|g, x, y, _| g.binop(Operator::I32Xor, x, y, Type::I32)),
                    // Wasm masks shift counts to 5 bits, as JS does.
                    Lsh => Some(|g, x, y, _| g.binop(Operator::I32Shl, x, y, Type::I32)),
                    Rsh => Some(|g, x, y, _| g.binop(Operator::I32ShrS, x, y, Type::I32)),
                    Ursh => Some(Self::ursh_i32),
                    _ => None,
                };
                // Double `%` is fmod, which Wasm lacks: the helper does it.
                let num = match op {
                    Add => Some(Operator::F64Add),
                    Sub => Some(Operator::F64Sub),
                    Mul => Some(Operator::F64Mul),
                    Div => Some(Operator::F64Div),
                    _ => None,
                };
                let int_arm = int.map(|f| {
                    move |g: &mut Self, v: &[Value], bail| {
                        let r = f(g, v[0], v[1], bail);
                        g.boxed_int32(r)
                    }
                });
                let num_arm = num.map(|o| {
                    move |g: &mut Self, v: &[Value]| {
                        let r = g.binop(o, v[0], v[1], Type::F64);
                        g.box_number(r)
                    }
                });
                self.number_arms(
                    &[a, b],
                    int_arm.as_ref().map(|f| f as &IntArm<'_, 'a>),
                    num_arm.as_ref().map(|f| f as &NumArm<'_, 'a>),
                    |g| {
                        let r = if op == Add {
                            g.rt(h.add, &[a, b])
                        } else {
                            let k = g.i32c(binop_kind(op));
                            g.rt(h.binop, &[k, a, b])
                        };
                        g.set_slot(d - 2, r);
                    },
                );
            }
            Inc | Dec | Neg | BitNot => {
                let a = self.slot(d - 1);
                let int_arm = |g: &mut Self, v: &[Value], bail| {
                    let x = v[0];
                    let r = if op == BitNot {
                        let m1 = g.i32c(u32::MAX);
                        g.binop(Operator::I32Xor, x, m1, Type::I32)
                    } else {
                        // Inc and Dec overflow only from the extreme value;
                        // Neg of 0 is -0 and of INT32_MIN overflows.
                        let (limit, bin) = match op {
                            Inc => (i32::MAX as u32, Operator::I32Add),
                            Dec => (i32::MIN as u32, Operator::I32Sub),
                            _ => (i32::MIN as u32, Operator::I32Sub),
                        };
                        let l = g.i32c(limit);
                        let ovf = g.binop(Operator::I32Eq, x, l, Type::I32);
                        g.bail_if(ovf, bail);
                        if op == Neg {
                            let zero = g.unop(Operator::I32Eqz, x, Type::I32);
                            g.bail_if(zero, bail);
                            let z = g.i32c(0);
                            g.binop(Operator::I32Sub, z, x, Type::I32)
                        } else {
                            let one = g.i32c(1);
                            g.binop(bin, x, one, Type::I32)
                        }
                    };
                    g.boxed_int32(r)
                };
                let num_arm = |g: &mut Self, v: &[Value]| {
                    let r = match op {
                        Neg => g.unop(Operator::F64Neg, v[0], Type::F64),
                        _ => {
                            let one = g.f64c(1.0);
                            let bin = if op == Inc {
                                Operator::F64Add
                            } else {
                                Operator::F64Sub
                            };
                            g.binop(bin, v[0], one, Type::F64)
                        }
                    };
                    g.box_number(r)
                };
                self.number_arms(
                    &[a],
                    Some(&int_arm),
                    (op != BitNot).then_some(&num_arm as &NumArm<'_, 'a>),
                    |g| {
                        let r = match op {
                            Neg => g.rt(h.neg, &[a]),
                            _ => {
                                let k = g.i32c(binop_kind(op));
                                g.rt(h.binop, &[k, a, a])
                            }
                        };
                        g.set_slot(d - 1, r);
                    },
                );
            }
            Pow => self.binary(h.pow, &[], d),
            Lt | Gt | Le | Ge => {
                let (kind, icmp, fcmp) = match op {
                    Lt => (CMP_LT, Operator::I32LtS, Operator::F64Lt),
                    Gt => (CMP_GT, Operator::I32GtS, Operator::F64Gt),
                    Le => (CMP_LE, Operator::I32LeS, Operator::F64Le),
                    _ => (CMP_GE, Operator::I32GeS, Operator::F64Ge),
                };
                let a = self.slot(d - 2);
                let b = self.slot(d - 1);
                self.number_arms(
                    &[a, b],
                    Some(&|g: &mut Self, v: &[Value], _| {
                        let r = g.binop(icmp, v[0], v[1], Type::I32);
                        g.boxed_bool(r)
                    }),
                    Some(&|g: &mut Self, v: &[Value]| {
                        // False on NaN, as JS wants.
                        let r = g.binop(fcmp, v[0], v[1], Type::I32);
                        g.boxed_bool(r)
                    }),
                    |g| g.compare(kind, a, b),
                );
            }
            Eq | Ne | StrictEq | StrictNe => {
                let (kind, negate) = match op {
                    Eq => (CMP_EQ, false),
                    Ne => (CMP_NE, true),
                    StrictEq => (CMP_STRICTEQ, false),
                    _ => (CMP_STRICTNE, true),
                };
                let a = self.slot(d - 2);
                let b = self.slot(d - 1);
                let (inline, eq) = self.equality_inline(a, b);
                self.fast_path(
                    inline,
                    |g, _| {
                        let r = if negate {
                            g.unop(Operator::I32Eqz, eq, Type::I32)
                        } else {
                            eq
                        };
                        let r = g.boxed_bool(r);
                        g.set_slot(d - 2, r);
                    },
                    // Two numbers (at least one a double): loose and strict
                    // equality are both IEEE equality (NaN unequal, -0 == 0).
                    |g| {
                        g.number_arms(
                            &[a, b],
                            None,
                            Some(&|g: &mut Self, v: &[Value]| {
                                let o = if negate {
                                    Operator::F64Ne
                                } else {
                                    Operator::F64Eq
                                };
                                let r = g.binop(o, v[0], v[1], Type::I32);
                                g.boxed_bool(r)
                            }),
                            |g| g.compare(kind, a, b),
                        )
                    },
                );
            }
            Not => {
                let v = self.slot(d - 1);
                let (fast, truth) = self.truthy_inline(v);
                self.fast_path(
                    fast,
                    |g, _| {
                        let n = g.unop(Operator::I32Eqz, truth, Type::I32);
                        let r = g.boxed_bool(n);
                        g.set_slot(d - 1, r);
                    },
                    |g| {
                        let t = g.truthy(v);
                        let n = g.unop(Operator::I32Eqz, t, Type::I32);
                        let r = g.boxed_bool(n);
                        g.set_slot(d - 1, r);
                    },
                );
            }
            Typeof | TypeofExpr => {
                let v = self.slot(d - 1);
                let r = self
                    .call(h.typeof_, &[self.cx, v], Some(Type::I64))
                    .unwrap();
                self.set_slot(d - 1, r);
            }
            TypeofEq => {
                let operand = p.next_uint8().unwrap();
                let v = self.slot(d - 1);
                let o = self.i32c(u32::from(operand));
                let b = self
                    .call(h.typeof_eq, &[self.cx, v, o], Some(Type::I32))
                    .unwrap();
                let r = self.boxed_bool(b);
                self.set_slot(d - 1, r);
            }
            StrictConstantEq | StrictConstantNe => {
                let operand = p.next_uint16().unwrap();
                let v = self.slot(d - 1);
                let mut b = self.constant_strict_eq(v, operand);
                if op == StrictConstantNe {
                    let z = self.i32c(0);
                    b = self.binop(Operator::I32Eq, b, z, Type::I32);
                }
                let r = self.boxed_bool(b);
                self.set_slot(d - 1, r);
            }

            // --- control flow ---
            Goto => {
                let off = p.next_int32().unwrap();
                let t = self.branch_to(off);
                self.br(t);
            }
            JumpIfFalse | JumpIfTrue | And | Or => {
                let off = p.next_int32().unwrap();
                let v = self.slot(d - 1);
                let taken = self.branch_to(off);
                let next = Self::goto(self.block(self.next_pc(op)));
                if matches!(op, JumpIfTrue | Or) {
                    self.branch_truthy(v, taken, next);
                } else {
                    self.branch_truthy(v, next, taken);
                }
            }
            Case => {
                let off = p.next_int32().unwrap();
                let v = self.slot(d - 1);
                let taken = self.branch_to(off);
                let next = Self::goto(self.block(self.next_pc(op)));
                self.branch_truthy(v, taken, next);
            }
            Coalesce => {
                let off = p.next_int32().unwrap();
                let v = self.slot(d - 1);
                let u = self.tag_eq(v, TAG_UNDEFINED);
                let n = self.tag_eq(v, TAG_NULL);
                let nullish = self.binop(Operator::I32Or, u, n, Type::I32);
                let taken = self.branch_to(off);
                let next = Self::goto(self.block(self.next_pc(op)));
                self.cond_br(nullish, next, taken);
            }
            Default => {
                let off = p.next_int32().unwrap();
                let t = self.branch_to(off);
                self.br(t);
            }
            TableSwitch => self.table_switch(p)?,
            Return => {
                let v = self.slot(d - 1);
                self.ret(v);
            }
            RetRval => {
                let v = self.load_i64(self.vp, self.layout.rval());
                self.ret(v);
            }

            // --- calls ---
            Call | CallContent | CallIgnoresRv | CallIter | CallContentIter => {
                let argc = u32::from(p.next_uint16().unwrap());
                if matches!(op, CallIter | CallContentIter) {
                    self.call_op(h.call_iter, argc, argc + 2, &[]);
                } else {
                    self.call_direct_or_generic(argc);
                }
            }
            New | NewContent | SuperCall => {
                let argc = u32::from(p.next_uint16().unwrap());
                // The construct site's sized allocation and early stamp word
                // (`construct_nslots`, `typed_alloc_word`): the ctor's exit
                // stamp needs both.
                let site = Site::new(self.sid, self.pc);
                let mono = match self.ctx.facts.scripted_targets(site) {
                    [s] => Some(*s),
                    _ => None,
                };
                let (n, w) = if op == SuperCall {
                    (NO_NSLOTS, 0)
                } else {
                    (
                        construct_nslots(self.ctx, mono, site),
                        typed_alloc_word(self.ctx, mono, site),
                    )
                };
                let nslots = self.i32c(n);
                let stamp = self.i32c(w);
                self.call_op(h.construct, argc, argc + 3, &[nslots, stamp]);
            }
            SpreadCall | SpreadNew | SpreadSuperCall => {
                let construct = op != SpreadCall;
                let need = if construct { 4 } else { 3 };
                let callee = self.slot(d - need);
                let thisv = self.slot(d - need + 1);
                let arr = self.slot(d - need + 2);
                let nt = if construct {
                    self.slot(d - 1)
                } else {
                    self.i64c(TAG_NULL << 32)
                };
                let c = self.i32c(u32::from(construct));
                let r = self.rt(h.spread_call, &[callee, thisv, arr, nt, c]);
                self.set_slot(d - need, r);
            }

            // --- properties and elements ---
            GetProp => {
                // The site's inline cache: the module's shared probe
                // `night_ic_get` (own and holder ways, then the megamorphic
                // table), and on a miss the generic get, which fills the
                // site's ways.
                let a = self.atom(p.next_uint32().unwrap())?;
                let recv = self.slot(d - 1);
                let av = self.i32c(a);
                let cache = self.atoms.next_prop_cache();
                let way = self.i32c(IC_WAY_ADDR_PLACEHOLDER);
                self.prop_ic_patches.push((way, cache * INLINE_IC_STRIDE));
                let r = self
                    .call(h.ic_get_poly, &[recv, av, way], Some(Type::I64))
                    .unwrap();
                let hit = self.tag_eq(r, TAG_MAGIC);
                let hit = self.unop(Operator::I32Eqz, hit, Type::I32);
                self.fast_path(
                    hit,
                    |g, _| g.set_slot(d - 1, r),
                    |g| {
                        let c = g.i32c(cache);
                        let r = g.rt(h.get_prop_ic_miss, &[recv, av, c]);
                        g.set_slot(d - 1, r);
                    },
                );
            }
            SetProp | StrictSetProp => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let recv = self.slot(d - 2);
                let v = self.slot(d - 1);
                let (av, sv) = (self.i32c(a), self.i32c(u32::from(op == StrictSetProp)));
                let vouch = self.vouch_types(a, recv, v);
                let sv = self.binop(Operator::I32Or, sv, vouch, Type::I32);
                self.rt(h.set_property, &[recv, av, v, sv]);
                let v = self.slot(d - 1);
                self.set_slot(d - 2, v);
            }
            GetElem => {
                let a = self.slot(d - 2);
                let b = self.slot(d - 1);
                let obj = self.tag_eq(a, TAG_OBJECT);
                let int = self.tag_eq(b, TAG_INT32);
                let cond = self.binop(Operator::I32And, obj, int, Type::I32);
                self.fast_path(
                    cond,
                    |g, slow| {
                        let v = g.dense_element(a, b, slow);
                        g.set_slot(d - 2, v);
                    },
                    |g| {
                        let r = g.rt(h.get_element, &[a, b]);
                        g.set_slot(d - 2, r);
                    },
                );
            }
            SetElem | StrictSetElem => {
                let recv = self.slot(d - 3);
                let key = self.slot(d - 2);
                let v = self.slot(d - 1);
                let sv = self.i32c(u32::from(op == StrictSetElem));
                self.rt(h.set_element, &[recv, key, v, sv]);
                let v = self.slot(d - 1);
                self.set_slot(d - 3, v);
            }
            DelProp | StrictDelProp => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let v = self.slot(d - 1);
                let (av, sv) = (self.i32c(a), self.i32c(u32::from(op == StrictDelProp)));
                let r = self.rt(h.del_prop, &[v, av, sv]);
                self.set_slot(d - 1, r);
            }
            DelElem | StrictDelElem => {
                let sv = self.i32c(u32::from(op == StrictDelElem));
                self.binary(h.del_elem, &[sv], d);
            }
            In => self.binary(h.in_, &[], d),
            HasOwn => self.binary(h.has_own, &[], d),
            Instanceof => {
                let z = self.i32c(0);
                self.binary(h.instanceof_, &[z], d);
            }
            SuperBase => self.unary(h.super_base, d),
            SuperFun => self.unary(h.super_fun, d),
            GetPropSuper => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let recv = self.slot(d - 2);
                let base = self.slot(d - 1);
                let av = self.i32c(a);
                let r = self.rt(h.get_prop_super, &[recv, base, av]);
                self.set_slot(d - 2, r);
            }
            GetElemSuper => {
                let recv = self.slot(d - 3);
                let key = self.slot(d - 2);
                let base = self.slot(d - 1);
                let r = self.rt(h.get_elem_super, &[recv, key, base]);
                self.set_slot(d - 3, r);
            }
            SetPropSuper | StrictSetPropSuper => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let recv = self.slot(d - 3);
                let base = self.slot(d - 2);
                let v = self.slot(d - 1);
                let av = self.i32c(a);
                let sv = self.i32c(u32::from(op == StrictSetPropSuper));
                let r = self.rt(h.set_prop_super, &[recv, base, av, v, sv]);
                self.set_slot(d - 3, r);
            }
            SetElemSuper | StrictSetElemSuper => {
                let recv = self.slot(d - 4);
                let key = self.slot(d - 3);
                let base = self.slot(d - 2);
                let v = self.slot(d - 1);
                let sv = self.i32c(u32::from(op == StrictSetElemSuper));
                let r = self.rt(h.set_elem_super, &[recv, key, base, v, sv]);
                self.set_slot(d - 4, r);
            }

            // --- literals ---
            NewInit | NewObject => {
                skip(p, op);
                let site = crate::ids::Site::new(self.sid, self.pc);
                let z = self.i32c(0);
                let n = self.i32c(self.ctx.lit_nslots_in.get(&site).copied().unwrap_or(0));
                let r = self.rt(h.new_object, &[z, n]);
                let w = self
                    .ctx
                    .lit_stamps_in
                    .get(&site)
                    .map(|&l| (l + 1) | CLASS_WORD_SLOTS);
                self.stamp_fresh(r, w);
                self.set_slot(d, r);
            }
            NewArray => {
                let len = p.next_uint32().unwrap();
                let (lv, z) = (self.i32c(len), self.i32c(0));
                let r = self.rt(h.new_array, &[lv, z]);
                let site = crate::ids::Site::new(self.sid, self.pc);
                let w = self.ctx.array_stamp_in.get(&site).copied();
                self.stamp_fresh(r, w);
                self.set_slot(d, r);
            }
            InitProp | InitHiddenProp | InitLockedProp => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let attrs = match op {
                    InitProp => INIT_ATTR_ENUMERATE,
                    InitHiddenProp => INIT_ATTR_HIDDEN,
                    _ => INIT_ATTR_LOCKED,
                };
                let obj = self.slot(d - 2);
                let v = self.slot(d - 1);
                let (av, at, none) = (self.i32c(a), self.i32c(attrs), self.i32c(u32::MAX));
                self.rt(h.init_prop, &[obj, av, v, at, none]);
            }
            InitElem | InitHiddenElem | InitLockedElem => {
                let attrs = match op {
                    InitElem => INIT_ATTR_ENUMERATE,
                    InitHiddenElem => INIT_ATTR_HIDDEN,
                    _ => INIT_ATTR_LOCKED,
                };
                let obj = self.slot(d - 3);
                let key = self.slot(d - 2);
                let v = self.slot(d - 1);
                let at = self.i32c(attrs);
                self.rt(h.init_elem, &[obj, key, v, at]);
            }
            InitElemArray => {
                let index = p.next_uint32().unwrap();
                let obj = self.slot(d - 2);
                let v = self.slot(d - 1);
                let key = self.i64c((TAG_INT32 << 32) | u64::from(index));
                let at = self.i32c(INIT_ATTR_ENUMERATE);
                self.rt(h.init_elem, &[obj, key, v, at]);
            }
            InitElemInc => {
                let obj = self.slot(d - 3);
                let idx = self.slot(d - 2);
                let v = self.slot(d - 1);
                let at = self.i32c(INIT_ATTR_ENUMERATE);
                self.rt(h.init_elem, &[obj, idx, v, at]);
                // The index is an int32 (array literal positions are small).
                let idx = self.slot(d - 2);
                let i = self.unop(Operator::I32WrapI64, idx, Type::I32);
                let one = self.i32c(1);
                let n = self.binop(Operator::I32Add, i, one, Type::I32);
                let r = self.boxed_int32(n);
                self.set_slot(d - 2, r);
            }
            MutateProto => {
                let obj = self.slot(d - 2);
                let proto = self.slot(d - 1);
                self.rt(h.mutate_proto, &[obj, proto]);
            }
            InitHomeObject => {
                let f = self.slot(d - 2);
                let home = self.slot(d - 1);
                self.rt(h.init_home_object, &[f, home]);
            }
            InitPropGetter | InitHiddenPropGetter | InitPropSetter | InitHiddenPropSetter => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let kind = u32::from(matches!(op, InitPropSetter | InitHiddenPropSetter))
                    | (u32::from(matches!(op, InitHiddenPropGetter | InitHiddenPropSetter)) << 1);
                let obj = self.slot(d - 2);
                let f = self.slot(d - 1);
                let (av, kv) = (self.i32c(a), self.i32c(kind));
                self.rt(h.init_prop_getset, &[obj, av, f, kv]);
            }
            InitElemGetter | InitHiddenElemGetter | InitElemSetter | InitHiddenElemSetter => {
                let kind = u32::from(matches!(op, InitElemSetter | InitHiddenElemSetter))
                    | (u32::from(matches!(op, InitHiddenElemGetter | InitHiddenElemSetter)) << 1);
                let obj = self.slot(d - 3);
                let key = self.slot(d - 2);
                let f = self.slot(d - 1);
                let kv = self.i32c(kind);
                self.rt(h.init_elem_getset, &[obj, key, f, kv]);
            }
            NewPrivateName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let av = self.i32c(a);
                let r = self.rt(h.new_private_name, &[av]);
                self.set_slot(d, r);
            }
            CheckPrivateField => {
                let cond = p.next_uint8().unwrap();
                let kind = p.next_uint8().unwrap();
                let obj = self.slot(d - 2);
                let key = self.slot(d - 1);
                let (cv, kv) = (self.i32c(u32::from(cond)), self.i32c(u32::from(kind)));
                let r = self.rt(h.check_private_field, &[obj, key, cv, kv]);
                self.set_slot(d, r);
            }

            // --- exceptions ---
            Exception => {
                let r = self.rt(h.exception, &[]);
                self.set_slot(d, r);
            }
            ExceptionAndStack => {
                let exc = self.top_addr(d);
                let stk = self.top_addr(d + 1);
                let gef = h.get_exception_for_finally;
                let ok = self
                    .call(gef, &[self.cx, exc, stk], Some(Type::I32))
                    .unwrap();
                self.branch_on_err(ok);
            }

            // --- iteration ---
            Iter => self.unary(h.iter_, d),
            MoreIter => {
                let it = self.slot(d - 1);
                let r = self
                    .call(h.more_iter, &[self.cx, it], Some(Type::I64))
                    .unwrap();
                self.set_slot(d, r);
            }
            IsNoIter => {
                let v = self.slot(d - 1);
                let m = self.i64c((TAG_MAGIC << 32) | MAGIC_NO_ITER_VALUE);
                let b = self.binop(Operator::I64Eq, v, m, Type::I32);
                let r = self.boxed_bool(b);
                self.set_slot(d, r);
            }
            EndIter => {
                let it = self.slot(d - 2);
                self.call(h.end_iter, &[self.cx, it], None);
            }
            OptimizeGetIterator => {
                let v = self.slot(d - 1);
                let b = self
                    .call(h.optimize_get_iterator, &[self.cx, v], Some(Type::I32))
                    .unwrap();
                let r = self.boxed_bool(b);
                self.set_slot(d - 1, r);
            }
            CloseIter => {
                let kind = p.next_uint8().unwrap();
                let it = self.slot(d - 1);
                let k = self.i32c(u32::from(kind));
                self.rt(h.close_iter, &[it, k]);
            }
            ToAsyncIter => self.binary(h.to_async_iter, &[], d),

            // --- generators and async ---
            Generator => {
                let callee = self.load_i64(self.sp, 0);
                let env = if self.needs_env {
                    self.env()
                } else {
                    self.i64c(UNDEF)
                };
                let r = self.rt(h.create_generator, &[callee, env]);
                self.set_slot(d, r);
            }
            InitialYield | Yield | Await => {
                let k = p.next_uint24().unwrap();
                self.suspend(op, k);
            }
            AfterYield => skip(p, op),
            FinalYieldRval => {
                let g = self.slot(d - 1);
                self.rt(h.gen_final, &[g]);
                let v = self.load_i64(self.vp, self.layout.rval());
                self.ret(v);
            }
            ResumeKind => {
                let kind = p.next_uint8().unwrap();
                self.push_const(d, (TAG_INT32 << 32) | u64::from(kind));
            }
            IsGenClosing => {
                let v = self.slot(d - 1);
                let m = self.i64c((TAG_MAGIC << 32) | MAGIC_GENERATOR_CLOSING);
                let b = self.binop(Operator::I64Eq, v, m, Type::I32);
                let r = self.boxed_bool(b);
                self.set_slot(d, r);
            }
            CheckResumeKind => {
                // [val, gen, kind] -> [val]: Next continues; Throw and
                // Return raise through the helper (Return by staging the
                // value as rval and raising the generator-closing magic).
                let kind = self.slot(d - 1);
                let kind = self.unop(Operator::I32WrapI64, kind, Type::I32);
                let is_next = self.unop(Operator::I32Eqz, kind, Type::I32);
                let raise = self.body.add_block();
                let cont = self.body.add_block();
                self.cond_br(is_next, Self::goto(cont), Self::goto(raise));
                self.cur = raise;
                self.live = true;
                let g = self.slot(d - 2);
                let v = self.slot(d - 3);
                let k = self.slot(d - 1);
                let k = self.unop(Operator::I32WrapI64, k, Type::I32);
                let rval = self.add_off(self.vp, self.layout.rval());
                let top = self.top_addr(d);
                self.call(
                    h.gen_check_resume,
                    &[self.cx, top, g, v, k, rval],
                    Some(Type::I32),
                );
                let t = self.exc_target(self.pc, d);
                self.br(t);
                self.cur = cont;
                self.live = true;
            }
            AsyncAwait | AsyncResolve => {
                let f = if op == AsyncAwait {
                    h.async_await
                } else {
                    h.async_resolve
                };
                let v = self.slot(d - 2);
                let g = self.slot(d - 1);
                let r = self.rt(f, &[g, v]);
                self.set_slot(d - 2, r);
            }
            AsyncReject => {
                let reason = self.slot(d - 3);
                let stack = self.slot(d - 2);
                let g = self.slot(d - 1);
                let r = self.rt(h.async_reject, &[g, reason, stack]);
                self.set_slot(d - 3, r);
            }
            CanSkipAwait => {
                let v = self.slot(d - 1);
                let r = self.rt(h.can_skip_await, &[v]);
                self.set_slot(d, r);
            }
            MaybeExtractAwaitValue => {
                let v = self.slot(d - 2);
                let can = self.slot(d - 1);
                let can = self.unop(Operator::I32WrapI64, can, Type::I32);
                let r = self.rt(h.maybe_extract_await, &[v, can]);
                self.set_slot(d - 2, r);
            }

            // --- the rest (docs/BASELINE.md §6) ---
            BigInt => {
                let idx = p.next_uint32().unwrap();
                let script = self.script_ptr();
                let iv = self.i32c(idx);
                let r = self.rt(h.bigint, &[script, iv]);
                self.set_slot(d, r);
            }
            NonSyntacticGlobalThis => {
                let env = self.env();
                let r = self.rt(h.non_syntactic_global_this, &[env]);
                self.set_slot(d, r);
            }
            SetIntrinsic => {
                skip(p, op);
                let script = self.script_ptr();
                let pcv = self.i32c(self.pc.get());
                let v = self.slot(d - 1);
                self.rt(h.set_intrinsic, &[script, pcv, v]);
            }
            EnvCallee => {
                let hops = match op.len() {
                    2 => u32::from(p.next_uint8().unwrap()),
                    _ => u32::from(p.next_uint16().unwrap()),
                };
                let env = self.env();
                let hv = self.i32c(hops);
                let r = self
                    .call(h.env_callee, &[self.cx, env, hv], Some(Type::I64))
                    .unwrap();
                self.set_slot(d, r);
            }
            // Direct eval reads the caller's interpreter frame (its
            // `arguments`, its Debugger frame), which a compiled frame does
            // not carry: the script is interpreted.
            Eval | StrictEval | SpreadEval | StrictSpreadEval => {
                return Err(format!("{} ({op:?})", super::UNSUPPORTED_OP));
            }
            DynamicImport => {
                let spec = self.slot(d - 2);
                let opts = self.slot(d - 1);
                let script = self.script_ptr();
                let r = self.rt(h.dynamic_import, &[script, spec, opts]);
                self.set_slot(d - 2, r);
            }
            ImportMeta => {
                let script = self.script_ptr();
                let r = self.rt(h.import_meta, &[script]);
                self.set_slot(d, r);
            }
            GetImport => {
                skip(p, op);
                let env = self.env();
                let script = self.script_ptr();
                let pcv = self.i32c(self.pc.get());
                let r = self.rt(h.get_import, &[env, script, pcv]);
                self.set_slot(d, r);
            }
            AddDisposable => {
                let hint = p.next_uint8().unwrap();
                let env = self.env();
                let v = self.slot(d - 3);
                let method = self.slot(d - 2);
                let nc = self.slot(d - 1);
                let hv = self.i32c(u32::from(hint));
                self.rt(h.add_disposable, &[env, v, method, nc, hv]);
            }
            TakeDisposeCapability => {
                let env = self.env();
                let r = self.rt(h.take_dispose_capability, &[env]);
                self.set_slot(d, r);
            }
            CreateSuppressedError => {
                let e = self.slot(d - 2);
                let sup = self.slot(d - 1);
                let r = self.rt(h.create_suppressed_error, &[e, sup]);
                self.set_slot(d - 2, r);
            }
            Resume => {
                let g = self.slot(d - 3);
                let v = self.slot(d - 2);
                let k = self.slot(d - 1);
                let r = self.rt(h.resume, &[g, v, k]);
                self.set_slot(d - 3, r);
            }
            ForceInterpreter => return Err(super::FORCE_INTERPRETER.into()),
        }
        Ok(())
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
