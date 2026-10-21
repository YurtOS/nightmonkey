//! resume: methods of `Gen`, split out of `codegen.rs` unchanged.

use super::*;

impl<'a> Gen<'a> {
    /// The block that starts at `pc`.
    pub(super) fn block(&mut self, pc: Pc) -> Block {
        if let Some(&b) = self.blocks.get(&pc) {
            return b;
        }
        let b = self.body.add_block();
        self.blocks.insert(pc, b);
        if self.needs_dispatch(pc) {
            let code = self.body.add_block();
            self.dispatch.insert(pc, (b, code));
        }
        b
    }

    /// Whether the loop headed at `pc` contains a resume target (other than
    /// itself), so resumes must be dispatched through it.
    pub(super) fn needs_dispatch(&self, pc: Pc) -> bool {
        self.loops
            .iter()
            .any(|&(h, e)| h == pc && self.resume_targets.iter().any(|&t| t > h && t < e))
    }

    /// The dispatching loop headers enclosing `t`, outermost first.
    pub(super) fn dispatch_chain(&self, t: Pc) -> Vec<Pc> {
        let mut hs: Vec<(Pc, Pc)> = self
            .loops
            .iter()
            .copied()
            .filter(|&(h, e)| h < t && t < e)
            .collect();
        // Outermost first: earlier header, then the longer interval.
        hs.sort_by_key(|&(h, e)| (h, std::cmp::Reverse(e)));
        hs.into_iter().map(|(h, _)| h).collect()
    }

    pub(super) fn env_is_plain(&self) -> bool {
        env_is_plain(self.ctx.source, self.script)
    }

    /// `argc` without its flag bits, and `vp`. Emitted in the entry block,
    /// ahead of any fork, so every path (fresh call or resume) sees them. A
    /// generator resume passes `argc = 0`, so its `vp` is `sp`.
    pub(super) fn frame_regs(&mut self) {
        let nargs = u32::from(self.script.nargs);
        let flags = self.i32c(!ARGC_FLAGS);
        self.argc = self.binop(Operator::I32And, self.argc, flags, Type::I32);
        if self.layout.rebase_vp {
            let n = self.i32c(nargs);
            let extra = self.binop(Operator::I32Sub, self.argc, n, Type::I32);
            let more = self.binop(Operator::I32GtU, self.argc, n, Type::I32);
            let zero = self.i32c(0);
            let extra = self.select(Type::I32, extra, zero, more);
            let eight = self.i32c(8);
            let bytes = self.binop(Operator::I32Mul, extra, eight, Type::I32);
            self.vp = self.binop(Operator::I32Add, self.sp, bytes, Type::I32);
        }
    }

    /// Find every yield's resume label and landing.
    pub(super) fn scan_resumes(&mut self) -> R<()> {
        let len = self.script.bytecode.len();
        let mut p = self.script.parser();
        loop {
            let pc = Pc::new(u32::try_from(len - p.remaining()).unwrap());
            let Some(op) = p.next_op() else { break };
            if matches!(op, JSOp::InitialYield | JSOp::Yield | JSOp::Await) {
                let k = p.next_uint24().ok_or("yield: missing resume index")?;
                let popped = if op == JSOp::InitialYield { 1 } else { 2 };
                if let Some(d) = self.depths.at(pc) {
                    let landing = pc + op.len();
                    self.gen_resume.push((k, landing, d - popped));
                    self.resume_targets.insert(landing);
                    self.resume_words.insert(ResumeWord {
                        pc: landing,
                        mode: ResumeMode::Continue,
                    });
                }
            } else {
                skip(&mut p, op);
            }
        }
        Ok(())
    }

    pub(super) fn resume_word(&mut self) -> Value {
        self.load_i32(self.vp, self.layout.resume())
    }

    pub(super) fn set_resume_word(&mut self, payload: u32) {
        let v = self.i64c((TAG_INT32 << 32) | u64::from(payload));
        self.store_i64(self.vp, self.layout.resume(), v);
    }

    /// A MIR onramp at loop header `h`, before the header's code: count the
    /// frame's backoff down, or, when it is zero, call the MIR body at its
    /// onramp root for `h` with this frame. The MIR body returns normally
    /// (baseline returns its result), or DEOPT with the frame rewritten
    /// and a resume word set (baseline resumes through its dispatch). MIR
    /// exits set the backoff, so baseline runs some iterations before it
    /// tries again.
    ///
    /// The backoff's low 16 bits count down; the bits above are how many
    /// attempts in a row made no progress, which doubles the wait (see
    /// `ONRAMP_LEVEL_SHIFT`).
    pub(super) fn onramp(&mut self, h: Pc) {
        let backoff = self.load_i32(self.vp, self.layout.backoff());
        let mask = self.i32c(ONRAMP_COUNT_MASK);
        let count = self.binop(Operator::I32And, backoff, mask, Type::I32);
        let z = self.i32c(0);
        let ready = self.binop(Operator::I32Eq, count, z, Type::I32);
        let (try_blk, wait_blk, cont) = (
            self.body.add_block(),
            self.body.add_block(),
            self.body.add_block(),
        );
        self.cond_br(ready, Self::goto(try_blk), Self::goto(wait_blk));
        self.cur = wait_blk;
        self.live = true;
        let one = self.i32c(1);
        let left = self.binop(Operator::I32Sub, backoff, one, Type::I32);
        let boxed = self.boxed_int32(left);
        self.store_i64(self.vp, self.layout.backoff(), boxed);
        self.br(Self::goto(cont));
        self.cur = try_blk;
        self.live = true;
        let w = ResumeWord {
            pc: h,
            mode: ResumeMode::Continue,
        };
        self.set_resume_word(w.encode() as u32);
        let bit = self.i32c(ARGC_ONRAMP_BIT);
        let argc = self.binop(Operator::I32Or, self.argc, bit, Type::I32);
        let args = [
            self.cx,
            self.sp,
            argc,
            self.retval_out,
            self.script_param,
            self.new_target,
        ];
        let args = self.body.arg_pool.from_iter(args.into_iter());
        let tys = self
            .body
            .type_pool
            .from_iter([Type::I32, Type::I32].into_iter());
        let call = self.push_val(ValueDef::Operator(
            Operator::Call {
                function_index: Func::invalid(),
            },
            args,
            tys,
        ));
        self.main_calls.push(call);
        let err = self.push_val(ValueDef::PickOutput(call, 0, Type::I32));
        let eff = self.push_val(ValueDef::PickOutput(call, 1, Type::I32));
        let (done, other, deopt) = (
            self.body.add_block(),
            self.body.add_block(),
            self.body.add_block(),
        );
        let z = self.i32c(0);
        let is_done = self.binop(Operator::I32Eq, err, z, Type::I32);
        self.cond_br(is_done, Self::goto(done), Self::goto(other));
        self.cur = done;
        self.live = true;
        self.terminate(Terminator::Return {
            values: vec![err, eff],
        });
        self.cur = other;
        self.live = true;
        let d = self.i32c(ERR_DEOPT);
        let is_deopt = self.binop(Operator::I32Eq, err, d, Type::I32);
        let error = self.error_return();
        self.cond_br(is_deopt, Self::goto(deopt), Self::goto(error));
        // DEOPT: route by the resume word the MIR exit wrote, among those
        // this onramp's root reaches -- only pcs inside the loops around
        // it or after it, which route reducibly (`deopt_route`). Not
        // through the entry's resume dispatch: that block is also entered
        // from the function entry, so a loop reaching it would have two
        // entries.
        let words = self.onramps[&h].clone();
        let hops: Vec<(ResumeWord, Block)> =
            words.iter().map(|&w| (w, self.deopt_route(h, w))).collect();
        self.cur = deopt;
        self.live = true;
        let word = self.resume_word();
        self.escalate_backoff(h, word, backoff);
        let bad = self.body.add_block();
        self.dispatch_on(word, hops, Self::goto(bad));
        self.cur = bad;
        self.live = true;
        self.terminate(Terminator::Unreachable);
        self.cur = cont;
        self.live = true;
    }

    /// After a DEOPT from the onramp at `h`: when the MIR body exited at
    /// the very header it was entered at, the attempt made no progress --
    /// typically the root's own entry guards refused this frame (a local
    /// that has become a double where the loop was built for int32), and
    /// the frame will not change back. Waiting the exit's fixed backoff
    /// and retrying would pay a call, a frame write-back and a dispatch
    /// every 32 iterations for the rest of the loop, so each such attempt
    /// doubles the wait instead, up to `ONRAMP_LEVEL_MAX` doublings. Any
    /// exit elsewhere made progress, and keeps the exit's own (level-0)
    /// backoff. `before` is the backoff read before the attempt: its count
    /// was zero, so it holds just the level.
    pub(super) fn escalate_backoff(&mut self, h: Pc, word: Value, before: Value) {
        let at_h = ResumeWord {
            pc: h,
            mode: ResumeMode::Continue,
        };
        let hw = self.i32c(at_h.encode() as u32);
        let stuck = self.binop(Operator::I32Eq, word, hw, Type::I32);
        let shift = self.i32c(ONRAMP_LEVEL_SHIFT);
        let level = self.binop(Operator::I32ShrU, before, shift, Type::I32);
        let one = self.i32c(1);
        let up = self.binop(Operator::I32Add, level, one, Type::I32);
        let max = self.i32c(ONRAMP_LEVEL_MAX);
        let over = self.binop(Operator::I32GtU, up, max, Type::I32);
        let level = self.select(Type::I32, max, up, over);
        let base = self.i32c(ONRAMP_BACKOFF);
        let count = self.binop(Operator::I32Shl, base, level, Type::I32);
        let tag = self.binop(Operator::I32Shl, level, shift, Type::I32);
        let raised = self.binop(Operator::I32Or, tag, count, Type::I32);
        let left = self.load_i32(self.vp, self.layout.backoff());
        let next = self.select(Type::I32, raised, left, stuck);
        let boxed = self.boxed_int32(next);
        self.store_i64(self.vp, self.layout.backoff(), boxed);
    }

    /// Where an onramp at header `h` sends a DEOPT for resume word `w`,
    /// keeping the CFG reducible: to the dispatch block of the innermost
    /// loop around the onramp that contains `w.pc` (a back edge to that
    /// loop's header, which routes inward), or, when none does, along
    /// `w`'s route from outside every loop (a loop exit, then entries only
    /// through headers).
    pub(super) fn deopt_route(&mut self, h: Pc, w: ResumeWord) -> Block {
        let mut around: Vec<(Pc, Pc)> = self
            .loops
            .iter()
            .copied()
            .filter(|&(hh, e)| hh <= h && h < e)
            .collect();
        // Innermost first.
        around.sort_by_key(|&(hh, e)| (std::cmp::Reverse(hh), e));
        match around.iter().find(|&&(hh, e)| hh < w.pc && w.pc < e) {
            Some(&(hh, _)) => self.block(hh),
            None => self.resume_route(w),
        }
    }

    /// The first hop toward resume word `w` from outside every loop: the
    /// outermost dispatching header enclosing its pc, or its landing.
    pub(super) fn resume_route(&mut self, w: ResumeWord) -> Block {
        match self.dispatch_chain(w.pc).first() {
            Some(&h) => self.block(h),
            None => self.resume_landing(w),
        }
    }

    /// The last hop for resume word `w`: clear the resume word, then
    /// - `Continue`: enter `w.pc`, through its own dispatch block if it is
    ///   a dispatching header, so no loop is entered anywhere but its
    ///   header node;
    /// - `Throw` (a MIR throw exit, the exception pending): take `w.pc`'s
    ///   exception landing, as the op there would have on failure.
    pub(super) fn resume_landing(&mut self, w: ResumeWord) -> Block {
        let saved = (self.cur, self.live, self.pc, self.d);
        let l = self.fresh();
        self.set_resume_word(RESUME_NONE);
        let target = match w.mode {
            ResumeMode::Continue => Self::goto(self.block(w.pc)),
            ResumeMode::Throw => {
                let d = self.depths.at(w.pc).expect("resume pcs have a depth");
                (self.pc, self.d) = (w.pc, d);
                self.exc_target(w.pc, d)
            }
        };
        self.br(target);
        (self.cur, self.live, self.pc, self.d) = saved;
        l
    }

    /// Emit each dispatching header's dispatch block: not resuming, run the
    /// header; resuming to a target inside, take the next hop toward it.
    pub(super) fn finalize_dispatch(&mut self) {
        let headers: Vec<(Pc, Block, Block)> = self
            .dispatch
            .iter()
            .map(|(&h, &(d, c))| (h, d, c))
            .collect();
        let targets: Vec<ResumeWord> = self.resume_words.iter().copied().collect();
        for (h, disp, code) in headers {
            let mut hops = vec![];
            for &t in &targets {
                let chain = self.dispatch_chain(t.pc);
                if let Some(i) = chain.iter().position(|&x| x == h) {
                    let next = match chain.get(i + 1) {
                        Some(&inner) => self.block(inner),
                        None => self.resume_landing(t),
                    };
                    hops.push((t, next));
                }
            }
            self.cur = disp;
            self.live = true;
            let w = self.resume_word();
            self.dispatch_on(w, hops, Self::goto(code));
        }
    }

    /// Branch on the resume word `w` to each hop's block; anything else
    /// goes to `default`.
    pub(super) fn dispatch_on(
        &mut self,
        w: Value,
        hops: Vec<(ResumeWord, Block)>,
        default: BlockTarget,
    ) {
        for (t, next) in hops {
            let k = self.i32c(t.encode() as u32);
            let hit = self.binop(Operator::I32Eq, w, k, Type::I32);
            let no = self.body.add_block();
            self.cond_br(hit, Self::goto(next), Self::goto(no));
            self.cur = no;
            self.live = true;
        }
        self.br(default);
    }

    /// The `ARGC_RESUME_BIT` entry (a MIR exit): route by the resume word
    /// to one of `ext_resumes`. The MIR body wrote the whole frame, so the
    /// prologue does not run. A word outside the set is a compiler bug.
    pub(super) fn finalize_resume_dispatch(&mut self, disp: Block) {
        self.cur = disp;
        self.live = true;
        // MIR's write-through stores keep a double a double: box the
        // formals, locals and rval as this body would (an integral double
        // an int32), since it passes them on to callees whose MIR entries
        // guard int32 formals.
        for i in 0..self.layout.nargs {
            let off = self.layout.arg(i);
            self.canon_slot(self.sp, off);
        }
        for j in 0..self.layout.nlocals {
            let off = self.layout.local(j);
            self.canon_slot(self.vp, off);
        }
        let off = self.layout.rval();
        self.canon_slot(self.vp, off);
        let bad = self.body.add_block();
        let words = self.ext_resumes.clone();
        let hops: Vec<(ResumeWord, Block)> =
            words.iter().map(|&w| (w, self.resume_route(w))).collect();
        self.cur = disp;
        self.live = true;
        let w = self.resume_word();
        self.dispatch_on(w, hops, Self::goto(bad));
        self.cur = bad;
        self.live = true;
        self.terminate(Terminator::Unreachable);
    }

    /// The generator resume entry: read the descriptor `EnterNightResume`
    /// staged, restore the frame from the generator object, push the
    /// resume protocol's `[sent value, generator, resume kind]` at the
    /// label's depth, and route to its landing.
    pub(super) fn finalize_gen_dispatch(&mut self) {
        let Some(disp) = self.gen_dispatch_blk else {
            return;
        };
        self.cur = disp;
        self.live = true;
        // The descriptor [index i32, kind i32, gen, arg] overlaps the
        // locals: read it all before writing the frame.
        let desc = self.add_off(self.vp, self.layout.local_base());
        let ridx = self.load_i32(desc, 0);
        let rkind = self.load_i32(desc, 4);
        let m = self.mem(3, 8);
        let rgen = self.unop(Operator::I64Load { memory: m }, desc, Type::I64);
        let m = self.mem(3, 16);
        let rarg = self.unop(Operator::I64Load { memory: m }, desc, Type::I64);
        // The fixed slots the skipped prologue would have set.
        let undef = self.i64c(UNDEF);
        let mut fixed = self.layout.optional_slots();
        fixed.push(self.layout.rval());
        for off in fixed {
            self.store_i64(self.vp, off, undef);
        }
        let zero = self.i64c(TAG_INT32 << 32);
        self.store_i64(self.vp, self.layout.backoff(), zero);
        self.set_resume_word(RESUME_NONE);
        let lp = self.add_off(self.vp, self.layout.local_base());
        let nl = self.i32c(self.layout.nlocals);
        let ep = if self.needs_env {
            self.add_off(self.vp, self.layout.env())
        } else {
            self.i32c(0)
        };
        let ops = self.add_off(self.vp, self.layout.operand_base());
        let gr = self.h.gen_restore;
        self.call(gr, &[self.cx, rgen, lp, nl, ep, ops], Some(Type::I32));
        let bad = self.body.add_block();
        let labels = std::mem::take(&mut self.gen_resume);
        let max_k = labels.iter().map(|&(k, _, _)| k).max().unwrap_or(0);
        let mut targets = vec![Self::goto(bad); (max_k + 1) as usize];
        for &(k, landing, depth) in &labels {
            let pre = self.fresh();
            self.set_slot(depth, rarg);
            self.set_slot(depth + 1, rgen);
            let kb = self.boxed_int32(rkind);
            self.set_slot(depth + 2, kb);
            let enc = layout::ResumeWord {
                pc: landing,
                mode: layout::ResumeMode::Continue,
            }
            .encode() as u32;
            self.set_resume_word(enc);
            let first = self.resume_route(ResumeWord {
                pc: landing,
                mode: ResumeMode::Continue,
            });
            self.br(Self::goto(first));
            targets[k as usize] = Self::goto(pre);
        }
        self.cur = disp;
        self.live = true;
        self.terminate(Terminator::Select {
            value: ridx,
            targets,
            default: Self::goto(bad),
        });
        // A resume index with no label: report an error.
        self.cur = bad;
        self.live = true;
        let one = self.i32c(1);
        let flags = self.i32c(FLAGS_ALL);
        self.terminate(Terminator::Return {
            values: vec![one, flags],
        });
    }

    /// `InitialYield` (`[gen] ->`) / `Yield`, `Await` (`[rval, gen] ->`):
    /// save the locals and the live operands into the generator object
    /// under resume index `k`, and return to the caller -- the generator
    /// for `InitialYield`, the result value otherwise.
    pub(super) fn suspend(&mut self, op: JSOp, k: u32) {
        let d = self.d;
        let g = self.slot(d - 1);
        let (rv, saved) = if op == JSOp::InitialYield {
            (g, d - 1)
        } else {
            (self.slot(d - 2), d - 2)
        };
        let env = if self.needs_env {
            self.env()
        } else {
            self.i64c(0)
        };
        let lp = self.add_off(self.vp, self.layout.local_base());
        let nl = self.i32c(self.layout.nlocals);
        let ops = self.add_off(self.vp, self.layout.operand_base());
        let (kv, dv) = (self.i32c(k), self.i32c(saved));
        let gs = self.h.gen_suspend;
        self.call(gs, &[self.cx, g, kv, lp, nl, ops, dv, env], Some(Type::I32));
        self.ret(rv);
    }
}
