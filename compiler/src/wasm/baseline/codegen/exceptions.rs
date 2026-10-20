//! exceptions: methods of `Gen`, split out of `codegen.rs` unchanged.

use super::*;

impl<'a> Gen<'a> {
    pub(super) fn branch_on_err(&mut self, ok: Value) {
        let t = self.exc_target(self.pc, self.d);
        let cont = self.body.add_block();
        self.cond_br(ok, Self::goto(cont), t);
        self.cur = cont;
        self.live = true;
    }

    /// Where an exception with no handler goes. Outside a generator, each
    /// throwing pc gets its own two-instruction return block rather than a
    /// shared one: a shared block would have one predecessor per helper
    /// call in the body, and on a huge body that fan-in makes dominator
    /// computation (here, and in waffle's backend) quadratic.
    pub(super) fn error_return(&mut self) -> Block {
        if self.is_gen {
            return self.error_block();
        }
        let saved = (self.cur, self.live);
        let b = self.fresh();
        let one = self.i32c(1);
        let flags = self.i32c(FLAGS_ALL);
        self.terminate(Terminator::Return {
            values: vec![one, flags],
        });
        (self.cur, self.live) = saved;
        b
    }

    pub(super) fn error_block(&mut self) -> Block {
        if let Some(b) = self.error_blk {
            return b;
        }
        let saved = (self.cur, self.live);
        let b = self.fresh();
        if self.is_gen {
            // A pending generator-closing magic is a forced `.return()`
            // that finished unwinding its finallys: return rval normally
            // (the interpreter's `HandleError` for generator frames).
            let gc = self.h.gen_closing;
            let closing = self.call(gc, &[self.cx], Some(Type::I32)).unwrap();
            let ret_blk = self.body.add_block();
            let err_blk = self.body.add_block();
            self.cond_br(closing, Self::goto(ret_blk), Self::goto(err_blk));
            self.cur = ret_blk;
            self.live = true;
            let v = self.load_i64(self.vp, self.layout.rval());
            self.ret(v);
            self.cur = err_blk;
            self.live = true;
        }
        let one = self.i32c(1);
        let flags = self.i32c(FLAGS_ALL);
        self.terminate(Terminator::Return {
            values: vec![one, flags],
        });
        (self.cur, self.live) = saved;
        self.error_blk = Some(b);
        b
    }

    /// The try-note walk of the interpreter's `ProcessTryNotes` for an
    /// exception at `pc` with operand depth `depth`: the closes on the way
    /// and the handler (pc, depth, is-finally), if any.
    pub(super) fn walk_try_notes(
        &self,
        pc: Pc,
        depth: u32,
    ) -> (Vec<Close>, Option<(Pc, u32, bool)>) {
        self.walk_try_notes_impl(pc, depth, false)
    }

    pub(super) fn walk_try_notes_impl(
        &self,
        pc: Pc,
        depth: u32,
        skip_catches: bool,
    ) -> (Vec<Close>, Option<(Pc, u32, bool)>) {
        let notes = &self.script.try_notes;
        let n = notes.len();
        let in_range = |t: &crate::bytecode::TryNote| pc >= t.start && pc < t.start + t.length;
        let mut closes = Vec::new();
        let mut i = 0;
        while i < n {
            let t = &notes[i];
            if !in_range(t) {
                i += 1;
                continue;
            }
            if matches!(t.kind, TryNoteKind::ForOfIterClose) {
                // Skip to the matching ForOf: the iterator is being closed
                // already.
                let mut nest = 1u32;
                i += 1;
                while nest > 0 && i < n {
                    let u = &notes[i];
                    if in_range(u) {
                        match u.kind {
                            TryNoteKind::ForOfIterClose => nest += 1,
                            TryNoteKind::ForOf => nest -= 1,
                            _ => {}
                        }
                    }
                    i += 1;
                }
                continue;
            }
            if t.stack_depth > depth {
                i += 1;
                continue;
            }
            match t.kind {
                TryNoteKind::Catch if skip_catches => {}
                TryNoteKind::Catch => {
                    return (closes, Some((t.start + t.length, t.stack_depth, false)))
                }
                TryNoteKind::Finally => {
                    return (closes, Some((t.start + t.length, t.stack_depth, true)))
                }
                TryNoteKind::ForIn => closes.push(Close::ForIn(t.stack_depth)),
                TryNoteKind::Destructuring => closes.push(Close::Destructuring(t.stack_depth)),
                TryNoteKind::ForOf | TryNoteKind::Loop | TryNoteKind::ForOfIterClose => {}
            }
            i += 1;
        }
        (closes, None)
    }

    /// Where an exception raised at `pc` (operand depth `depth`) goes:
    /// through its closes and env unwinding, to the catch handler, the
    /// finally landing, or the error return.
    pub(super) fn exc_target(&mut self, pc: Pc, depth: u32) -> BlockTarget {
        if let Some(t) = self.exc_targets.get(&(pc, depth)) {
            return t.clone();
        }
        let (closes, handler) = self.walk_try_notes(pc, depth);
        if self.is_gen && matches!(handler, Some((_, _, false))) {
            let t = self.gen_catch_target(pc, depth, closes, handler);
            self.exc_targets.insert((pc, depth), t.clone());
            return t;
        }
        let target = if closes.is_empty() {
            self.handler_dest(pc, handler)
        } else {
            let saved = (self.cur, self.live);
            let t = self.fresh();
            self.run_closes(&closes, depth);
            let dest = self.handler_dest(pc, handler);
            self.br(dest);
            (self.cur, self.live) = saved;
            Self::goto(t)
        };
        self.exc_targets.insert((pc, depth), target.clone());
        target
    }

    /// A catch handler in a generator body: a forced `.return()` unwind (a
    /// pending generator-closing magic) passes catches over and runs only
    /// finallys, so the edge tests for it at runtime (the peek helper, so
    /// the magic stays pending) and takes the catch-skipping walk's
    /// target when set -- `ProcessTryNotes`' `isClosingGenerator` rule.
    pub(super) fn gen_catch_target(
        &mut self,
        pc: Pc,
        depth: u32,
        closes: Vec<Close>,
        handler: Option<(Pc, u32, bool)>,
    ) -> BlockTarget {
        let (all_closes, closing_handler) = self.walk_try_notes_impl(pc, depth, true);
        let saved = (self.cur, self.live);
        let t = self.fresh();
        self.run_closes(&closes, depth);
        let closing_blk = self.body.add_block();
        let catch_blk = self.body.add_block();
        let gic = self.h.gen_is_closing;
        let closing = self.call(gic, &[self.cx], Some(Type::I32)).unwrap();
        self.cond_br(closing, Self::goto(closing_blk), Self::goto(catch_blk));
        self.cur = catch_blk;
        self.live = true;
        let dest = self.handler_dest(pc, handler);
        self.br(dest);
        self.cur = closing_blk;
        self.live = true;
        self.run_closes(&all_closes[closes.len()..], depth);
        let dest = self.handler_dest(pc, closing_handler);
        self.br(dest);
        (self.cur, self.live) = saved;
        Self::goto(t)
    }

    /// The handler block for an exception at `pc`, after popping the block
    /// environments the handler does not see.
    pub(super) fn handler_dest(&mut self, pc: Pc, handler: Option<(Pc, u32, bool)>) -> BlockTarget {
        let dest = match handler {
            None => return Self::goto(self.error_return()),
            Some((hpc, _, false)) => self.block(hpc),
            Some((hpc, hd, true)) => self.finally_landing(hpc, hd),
        };
        let pops = match handler {
            Some((hpc, _, _)) if self.needs_env => self.env_pops(pc, hpc),
            _ => 0,
        };
        if pops == 0 {
            return Self::goto(dest);
        }
        let saved = (self.cur, self.live);
        let t = self.fresh();
        self.pop_envs(pops);
        self.br(Self::goto(dest));
        (self.cur, self.live) = saved;
        Self::goto(t)
    }

    pub(super) fn pop_envs(&mut self, pops: u32) {
        let mut env = self.env();
        for _ in 0..pops {
            let p = self.unop(Operator::I32WrapI64, env, Type::I32);
            env = self.load_i64(p, FIXED_SLOTS_BASE);
        }
        self.set_env(env);
    }

    pub(super) fn run_closes(&mut self, closes: &[Close], depth: u32) {
        for c in closes {
            match *c {
                Close::ForIn(sd) => {
                    let it = self.slot(sd - 1);
                    let ei = self.h.end_iter;
                    self.call(ei, &[self.cx, it], None);
                }
                Close::Destructuring(sd) => {
                    let done = self.slot(sd - 1);
                    let it = self.slot(sd - 2);
                    let top = self.top_addr(depth);
                    let f = self.h.close_iter_for_exception;
                    self.call(f, &[self.cx, top, done, it], None);
                }
            }
        }
    }

    /// How many block environments the frame's env chain holds at `pc`
    /// beyond those in effect at the `Try` of the handler at `handler_pc`:
    /// what the interpreter's `UnwindEnvironmentToTryPc` pops.
    pub(super) fn env_pops(&self, pc: Pc, handler_pc: Pc) -> u32 {
        let Some(try_start) = self
            .script
            .try_notes
            .iter()
            .find(|t| {
                matches!(t.kind, TryNoteKind::Catch | TryNoteKind::Finally)
                    && t.start + t.length == handler_pc
            })
            .map(|t| t.start)
        else {
            return 0;
        };
        let try_pc = Pc::new(try_start.get() - 1);
        self.env_depth_at(pc)
            .saturating_sub(self.env_depth_at(try_pc))
    }

    pub(super) fn env_depth_at(&self, pc: Pc) -> u32 {
        let n = self
            .script
            .scope_notes
            .iter()
            .filter(|n| pc >= n.start && pc < n.start + n.length)
            .filter(|n| self.env_scopes.contains(&n.gcthing_index))
            .count();
        u32::try_from(n).unwrap()
    }

    /// The exceptional entry to a finally block: push the pending
    /// exception, its stack and `true` (throwing) at the note's depth.
    pub(super) fn finally_landing(&mut self, handler_pc: Pc, sd: u32) -> Block {
        if let Some(&b) = self.finally_landing.get(&handler_pc) {
            return b;
        }
        let saved = (self.cur, self.live, self.pc, self.d);
        let lp = self.fresh();
        self.finally_landing.insert(handler_pc, lp);
        let exc = self.top_addr(sd);
        let stk = self.top_addr(sd + 1);
        let gef = self.h.get_exception_for_finally;
        let ok = self
            .call(gef, &[self.cx, exc, stk], Some(Type::I32))
            .unwrap();
        (self.pc, self.d) = (handler_pc, sd);
        self.branch_on_err(ok);
        let t = self.i64c((TAG_BOOLEAN << 32) | 1);
        self.set_slot(sd + 2, t);
        let target = Self::goto(self.block(handler_pc));
        self.br(target);
        (self.cur, self.live, self.pc, self.d) = saved;
        lp
    }
}
