//! exits: methods of `Lower`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    /// `inline.enter` (§5.5): the inlined callee's frame, as its
    /// prologue would write it, from `callee, this, formals`. Every fixed
    /// slot is made a valid Value: helpers' GC scan limit inside the callee
    /// is the frame's top (`frame_top`), past them.
    pub(super) fn inline_enter(&mut self, d: &mir::func::InstData, a: &[Value]) -> R<()> {
        let fid = self.cur_frame as usize;
        self.framed.retain(|&(f, _), _| f as usize != fid);
        if self.f.inline_frames[fid - 1].parent == 0 {
            // The frame overlays the rooting slots from `inline_k` up,
            // which no value live in its region has: what they hold is
            // gone, and they are dirty again from here.
            let k = self.inline_k[fid];
            let home = &self.home;
            self.slotted.retain(|v| home.get(v).is_some_and(|&h| h < k));
            self.dirty.extend(k..self.nslots);
        }
        let (base, vbase, l) = (
            self.frame_off[fid],
            self.frame_voff[fid],
            self.frame_layouts[fid],
        );
        // Every actual for a callee reading them, else every formal.
        let nact = self.f.inline_frames[fid - 1]
            .argc
            .map_or(l.nargs, |n| n.max(l.nargs));
        // A construct also passes its new.target.
        let construct = a.len() == 3 + nact as usize;
        if a.len() != 2 + nact as usize && !construct {
            return Err("lowering: inline.enter needs callee, this and every formal".into());
        }
        let mut boxed = vec![];
        for (&v, &mv) in a.iter().zip(&d.args) {
            let t = self.ty(mv);
            boxed.push(self.boxed(&t, v)?);
        }
        // Inline frames are placed from `vp`.
        let sp = self.vp;
        self.store_i64(sp, base + FrameLayout::CALLEE, boxed[0]);
        self.store_i64(sp, base + FrameLayout::THIS, boxed[1]);
        for i in 0..nact {
            self.store_i64(sp, base + l.arg(i), boxed[2 + i as usize]);
        }
        let undef = self.i64c(UNDEF);
        for j in 0..l.nlocals {
            self.store_i64(sp, vbase + l.local(j), undef);
        }
        if self.fixed[fid] == Fixed::None {
            return Ok(());
        }
        // The callee's own environment: what its prologue loads, for a
        // script whose activation has none of its own (the only kind
        // inlined).
        let fun = self.un(Operator::I32WrapI64, boxed[0], Type::I32);
        let env = self.load_i64(fun, FUNC_ENV_SLOT_OFFSET);
        if l.has_env {
            self.store_i64(sp, vbase + l.env(), env);
        }
        if self.fixed[fid] == Fixed::Env {
            return Ok(());
        }
        if l.has_args_obj {
            self.store_i64(sp, vbase + l.args_obj(), undef);
        }
        if l.has_new_target {
            let nt = if construct {
                boxed[boxed.len() - 1]
            } else {
                undef
            };
            self.store_i64(sp, vbase + l.new_target(), nt);
        }
        self.store_i64(sp, vbase + l.rval(), undef);
        let zero = self.i64c(TAG_INT32 << 32);
        self.store_i64(sp, vbase + l.resume(), zero);
        self.store_i64(sp, vbase + l.backoff(), zero);
        // The operand stack is past the frame's GC scan limit
        // (`frame_top`): its exits write it, as they write the fixed slots
        // a leaner frame leaves out (`Fixed`).
        Ok(())
    }

    /// Count this exit (`--mir-exit-census`), with its static record.
    pub(super) fn census_exit(&mut self, script: ScriptId, pc: Pc, what: &str) {
        let Some(census) = self.exit_census else {
            return;
        };
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        crate::diag_line!("night: mir exit {id} sid#{script} pc {pc} {what}");
        let (k, i) = (
            self.i32c(crate::options::MIR_EXIT_CENSUS_KIND),
            self.i32c(id),
        );
        self.call1(census, &[k, i], Type::I32);
    }

    /// `exit.inline` (§5.5): finish the inlined callee in its baseline
    /// body from `pc`. Write the operands (the frame holds only those
    /// written through, the formals of a mapped `arguments`), the resume
    /// word, then call the callee's entry with `ARGC_RESUME_BIT` on its
    /// frame. The caller continues on `ok` with the callee's result, or on
    /// `err`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn exit_inline(
        &mut self,
        inst: mir::Inst,
        d: &mir::func::InstData,
        a: &[Value],
        pc: Pc,
        nargs: u32,
        nlocals: u32,
        throw: bool,
    ) -> R<()> {
        let fid = self.cur_frame;
        let callee = self.f.inline_frames[fid as usize - 1].script;
        let max_depth = self.f.inline_frames[fid as usize - 1].max_depth;
        let what = format!(
            "inline{} in sid#{}",
            if throw { " throw" } else { "" },
            self.f.script
        );
        self.census_exit(callee, pc, &what);
        // Rooted here, on the site's own state: the caller's managed values
        // live across the call (the same set at every exit of this frame,
        // which all continue at the call's join).
        let live = self.live_across(inst);
        self.root(&live)?;
        // The whole frame, boxed: a dead slot and the stack above this
        // pc's depth as undefined (baseline writes those before reading).
        let undef = self.i64c(UNDEF);
        let mut ops = vec![];
        for (&v, &mv) in a.iter().zip(&d.args) {
            let dead = matches!(
                self.f.values[mv].def,
                mir::func::ValueDef::Result(i, _)
                    if self.f.insts[i].op == Opcode::ConstVal(ConstVal::Dead)
            );
            ops.push(if dead {
                undef
            } else {
                let t = self.ty(mv);
                self.boxed(&t, v)?
            });
        }
        let full = (3 + nargs + nlocals + max_depth) as usize;
        if ops.len() > full {
            return Err("lowering: an exit.inline deeper than its frame".into());
        }
        ops.resize(full, undef);
        if !self.inline_hubs.contains_key(&fid) {
            let hub = self.inline_hub(fid, nargs, nlocals, max_depth)?;
            self.inline_hubs.insert(fid, hub);
        }
        let (hub, _, _, _, _, tails) = self.inline_hubs.get_mut(&fid).unwrap();
        let hub = *hub;
        let idx = u32::try_from(tails.len()).unwrap();
        let tail = self.body.add_block();
        let err = self.body.add_blockparam(tail, Type::I32);
        let same = self.body.add_blockparam(tail, Type::I32);
        self.inline_hubs.get_mut(&fid).unwrap().5.push(tail);
        let mode = if throw {
            ResumeMode::Throw
        } else {
            ResumeMode::Continue
        };
        let w = ResumeWord { pc, mode };
        let mut args = vec![self.i32c(w.encode() as u32), self.i32c(idx)];
        args.extend(ops);
        self.terminate(Terminator::Br {
            target: BlockTarget { block: hub, args },
        });
        // The site's tail: back from the callee's baseline body, with the
        // site's own state after the call.
        self.cur = tail;
        self.after_gc(&live);
        let end = self.frame_end[fid as usize];
        let result = self.load_i64(self.vp, end);
        let ok = self.un(Operator::I32Eqz, err, Type::I32);
        self.clean_or_dirty(inst, ok, same, &[result])
    }

    /// The `exit.inline` hub for inline frame `fid`: params the resume
    /// word, the site index, and the whole frame boxed (`this`, formals,
    /// locals, rval, the stack to its deepest); it writes the callee's
    /// frame, the resume word and the backoff, then runs the callee's
    /// baseline body on it. Its terminator, a dispatch to the sites'
    /// tails, is set once every site is lowered.
    pub(super) fn inline_hub(
        &mut self,
        fid: u32,
        nargs: u32,
        nlocals: u32,
        max_depth: u32,
    ) -> R<(Block, Block, Value, Value, Value, Vec<Block>)> {
        let saved = self.cur;
        let hub = self.body.add_block();
        let word = self.body.add_blockparam(hub, Type::I32);
        let site = self.body.add_blockparam(hub, Type::I32);
        let n = 3 + nargs + nlocals + max_depth;
        let ops: Vec<Option<Value>> = (0..n)
            .map(|_| Some(self.body.add_blockparam(hub, Type::I64)))
            .collect();
        self.cur = hub;
        let fp = frame_parts(&ops, nargs, nlocals).ok_or("lowering: malformed exit.inline")?;
        let f = fid as usize;
        let (base, vbase, end, l) = (
            self.frame_off[f],
            self.frame_voff[f],
            self.frame_end[f],
            self.frame_layouts[f],
        );
        // Inline frames are placed from `vp`.
        let sp = self.vp;
        if let Some(v) = *fp.this {
            self.store_i64(sp, base + FrameLayout::THIS, v);
        }
        for (i, v) in fp.args.iter().enumerate() {
            if let Some(v) = *v {
                self.store_i64(sp, base + l.arg(u32::try_from(i).unwrap()), v);
            }
        }
        for (j, v) in fp.locals.iter().enumerate() {
            if let Some(v) = *v {
                self.store_i64(sp, vbase + l.local(u32::try_from(j).unwrap()), v);
            }
        }
        if let Some(v) = *fp.rval {
            self.store_i64(sp, vbase + l.rval(), v);
        }
        for (k, v) in fp.stack.iter().enumerate() {
            if let Some(v) = *v {
                self.store_i64(sp, vbase + l.operand(u32::try_from(k).unwrap()), v);
            }
        }
        let w64 = self.un(Operator::I64ExtendI32U, word, Type::I64);
        let tag = self.i64c(TAG_INT32 << 32);
        let wv = self.bin(Operator::I64Or, w64, tag, Type::I64);
        self.store_i64(sp, vbase + l.resume(), wv);
        let backoff = self.i64c((TAG_INT32 << 32) | u64::from(ONRAMP_BACKOFF));
        self.store_i64(sp, vbase + l.backoff(), backoff);
        // The fixed slots the frame's entry left out (`Fixed`).
        if self.fixed[f] == Fixed::None && l.has_env {
            let callee = self.load_i64(sp, base + FrameLayout::CALLEE);
            let fun = self.un(Operator::I32WrapI64, callee, Type::I32);
            let env = self.load_i64(fun, FUNC_ENV_SLOT_OFFSET);
            self.store_i64(sp, vbase + l.env(), env);
        }
        if self.fixed[f] != Fixed::All {
            let undef = self.i64c(UNDEF);
            if l.has_args_obj {
                self.store_i64(sp, vbase + l.args_obj(), undef);
            }
            if l.has_new_target {
                self.store_i64(sp, vbase + l.new_target(), undef);
            }
        }
        let callee = self.load_i64(sp, base + FrameLayout::CALLEE);
        let (funcidx, script) = self.classify(callee);
        let off = self.i32c(u32::MAX);
        self.body_off_patches.push(off);
        let body_idx = self.bin(Operator::I32Sub, funcidx, off, Type::I32);
        let frame = self.add_off(sp, base);
        let top = self.add_off(sp, end);
        // The call's actual count, for a callee reading its actuals: its
        // `vp` rebase is the frame's.
        let n = self.f.inline_frames[f - 1].argc.unwrap_or(nargs);
        let argc = self.i32c(n | ARGC_RESUME_BIT);
        let undef = self.i64c(UNDEF);
        let args = self
            .body
            .arg_pool
            .from_iter([self.cx, frame, argc, top, script, undef, body_idx].into_iter());
        let tys = self
            .body
            .type_pool
            .from_iter([Type::I32, Type::I32].into_iter());
        // Whether the callee's baseline rest demoted a class word: the
        // sites continue on their clean edge only if not.
        let pre = self.epoch();
        let call = self.push_val(ValueDef::Operator(
            Operator::CallIndirect {
                sig_index: self.h.night_abi_sig2,
                table_index: self.h.indirect_table,
            },
            args,
            tys,
        ));
        let err = self.push_val(ValueDef::PickOutput(call, 0, Type::I32));
        let post = self.epoch();
        let same = self.bin(Operator::I32Eq, pre, post, Type::I32);
        // The dispatch goes where the body ends (classify branches).
        let last = self.cur;
        self.cur = saved;
        Ok((hub, last, site, err, same, vec![]))
    }

    /// Leave MIR at `w` with the frame state `ops` (this, formals,
    /// locals, rval, stack; of types `tys`, `dead` ones left as the frame
    /// has them): a branch to the function's exit hub for this shape of
    /// frame, carrying the resume word and the operands as they are.
    pub(super) fn exit(
        &mut self,
        w: ResumeWord,
        ops: &[Value],
        tys: &[MType],
        dead: &[bool],
        nargs: u32,
        nlocals: u32,
    ) -> R<()> {
        self.census_exit(self.f.script, w.pc, &format!("{:?}", w.mode));
        let mut shape = Vec::with_capacity(ops.len());
        for (t, &d) in tys.iter().zip(dead) {
            shape.push(if d { None } else { Some(BoxKind::of(t)?) });
        }
        let hub = match self.exit_hubs.get(&shape) {
            Some(&b) => b,
            None => {
                let b = self.exit_hub(&shape, tys, nargs, nlocals)?;
                self.exit_hubs.insert(shape.clone(), b);
                b
            }
        };
        let mut args = vec![self.i32c(w.encode() as u32)];
        args.extend(ops.iter().zip(dead).filter(|(_, &d)| !d).map(|(&v, _)| v));
        self.terminate(Terminator::Br {
            target: BlockTarget { block: hub, args },
        });
        Ok(())
    }

    /// The exit hub for frames of `shape` (§5.1): its params are the
    /// resume word and the live operands in their own representations. It
    /// boxes each once, writes the baseline frame, then runs the baseline
    /// body from there (or returns DEOPT to an onramping baseline caller).
    /// One hub serves every exit of the same shape, so a function's exit
    /// code grows with its distinct frame shapes, not with its exits.
    pub(super) fn exit_hub(
        &mut self,
        shape: &[Option<BoxKind>],
        tys: &[MType],
        nargs: u32,
        nlocals: u32,
    ) -> R<Block> {
        let saved = self.cur;
        let hub = self.body.add_block();
        let word = self.body.add_blockparam(hub, Type::I32);
        let mut raw = vec![];
        for (k, t) in shape.iter().zip(tys) {
            if k.is_some() {
                let m = machine(t).ok_or("lowering: an exit operand without a representation")?;
                raw.push(Some(self.body.add_blockparam(hub, m)));
            } else {
                raw.push(None);
            }
        }
        self.cur = hub;
        let mut boxed = vec![];
        for (v, t) in raw.iter().zip(tys) {
            boxed.push(match v {
                Some(v) => Some(self.boxed(t, *v)?),
                None => None,
            });
        }
        self.write_frame(&boxed, nargs, nlocals)?;
        let vp = self.vp;
        let l = self.layout;
        // The fixed slots MIR does not keep current. The env slot is
        // written through (scopes), and the arguments-object slot holds the
        // one `args.object` made, if any (the fresh entry cleared it).
        if l.has_new_target {
            self.store_i64(vp, l.new_target(), self.new_target);
        }
        let w64 = self.un(Operator::I64ExtendI32U, word, Type::I64);
        let tag = self.i64c(TAG_INT32 << 32);
        let wv = self.bin(Operator::I64Or, w64, tag, Type::I64);
        self.store_i64(vp, l.resume(), wv);
        // Baseline waits this many loop-header visits before it tries an
        // onramp again, so it makes progress from here.
        let backoff = self.i64c((TAG_INT32 << 32) | u64::from(ONRAMP_BACKOFF));
        self.store_i64(vp, l.backoff(), backoff);
        if self.has_onramps {
            // Entered by an onramp: the baseline caller resumes itself.
            let (deopt, call_blk) = (self.body.add_block(), self.body.add_block());
            self.cond_br(self.onramp_flag, Self::to(deopt), Self::to(call_blk));
            self.cur = deopt;
            let d = self.i32c(ERR_DEOPT);
            self.ret(d);
            self.cur = call_blk;
        }
        let bit = self.i32c(ARGC_RESUME_BIT);
        let argc = self.bin(Operator::I32Or, self.argc, bit, Type::I32);
        self.tail_to_baseline(argc);
        self.cur = saved;
        Ok(hub)
    }

    /// Write frame state `boxed` (this, formals, locals, rval, stack; a
    /// dead operand's slot keeps the frame's valid value) to the baseline
    /// frame.
    pub(super) fn write_frame(
        &mut self,
        boxed: &[Option<Value>],
        nargs: u32,
        nlocals: u32,
    ) -> R<()> {
        let fp = frame_parts(boxed, nargs, nlocals).ok_or("lowering: malformed exit")?;
        let (sp, vp) = (self.sp, self.vp);
        let l = self.layout;
        if let Some(v) = *fp.this {
            self.store_i64(sp, FrameLayout::THIS, v);
        }
        for (i, v) in fp.args.iter().enumerate() {
            if let Some(v) = *v {
                self.store_i64(sp, l.arg(u32::try_from(i).unwrap()), v);
            }
        }
        for (j, v) in fp.locals.iter().enumerate() {
            if let Some(v) = *v {
                self.store_i64(vp, l.local(u32::try_from(j).unwrap()), v);
            }
        }
        if let Some(v) = *fp.rval {
            self.store_i64(vp, l.rval(), v);
        }
        // Every stack slot: MIR's roots overlap them.
        for (k, v) in fp.stack.iter().enumerate() {
            let v = match *v {
                Some(v) => v,
                None => self.i64c(UNDEF),
            };
            self.store_i64(vp, l.operand(u32::try_from(k).unwrap()), v);
        }
        Ok(())
    }

    /// `gen.suspend` (baseline's `suspend`): write the frame, save its
    /// locals, the operands below the yielded ones and the environment
    /// into the generator under resume index `index`, and return the
    /// yielded value (the generator, for `InitialYield`).
    pub(super) fn gen_suspend(
        &mut self,
        inst: mir::Inst,
        a: &[Value],
        index: u32,
        nargs: u32,
        nlocals: u32,
        initial: bool,
    ) -> R<()> {
        let d = self.f.insts[inst].clone();
        let mut boxed = vec![];
        for (&v, &mv) in a.iter().zip(&d.args) {
            let dead = matches!(
                self.f.values[mv].def,
                mir::func::ValueDef::Result(i, _) if self.f.insts[i].op == Opcode::ConstVal(ConstVal::Dead)
            );
            boxed.push(if dead {
                None
            } else {
                Some(self.boxed(&self.ty(mv), v)?)
            });
        }
        let depth = frame_parts(&boxed, nargs, nlocals)
            .ok_or("lowering: malformed gen.suspend")?
            .stack
            .len();
        let popped = if initial { 1 } else { 2 };
        let saved = u32::try_from(
            depth
                .checked_sub(popped)
                .ok_or("lowering: gen.suspend stack too shallow")?,
        )
        .unwrap();
        let g = boxed[boxed.len() - 1].ok_or("lowering: gen.suspend without its generator")?;
        let rv = if initial {
            g
        } else {
            boxed[boxed.len() - 2].ok_or("lowering: gen.suspend without its value")?
        };
        // A dead local goes into the generator as undefined, not as the
        // frame's stale copy: a binding out of scope must not keep its
        // last value alive while the generator is suspended.
        let undef = self.i64c(UNDEF);
        let nlead = 1 + nargs as usize;
        for (i, b) in boxed.iter_mut().enumerate() {
            if b.is_none() && i >= nlead && i <= nlead + nlocals as usize {
                *b = Some(undef);
            }
        }
        self.write_frame(&boxed, nargs, nlocals)?;
        let l = self.layout;
        let env = if self.plain_env || self.own_env {
            self.frame_env()
        } else {
            self.i64c(0)
        };
        let lp = self.add_off(self.vp, l.local_base());
        let nl = self.i32c(l.nlocals);
        let ops = self.add_off(self.vp, l.operand_base());
        let (kv, dv) = (self.i32c(index), self.i32c(saved));
        self.call(
            self.h.gen_suspend,
            &[self.cx, g, kv, lp, nl, ops, dv, env],
            &[Type::I32],
        );
        self.store_i64(self.retval_out, 0, rv);
        let z = self.i32c(0);
        self.ret(z);
        Ok(())
    }

    /// Run this script's baseline body on the frame at `sp` with `argc`
    /// (resume and onramp bits included), and return its result.
    pub(super) fn tail_to_baseline(&mut self, argc: Value) {
        // The script from the frame's callee, not the `script` param: a
        // param used anywhere is copied at entry, and that i32 stack
        // argument read as a 64-bit slot defeats store forwarding from the
        // caller's store on every call.
        let callee = self.load_i64(self.sp, FrameLayout::CALLEE);
        let fp = self.un(Operator::I32WrapI64, callee, Type::I32);
        let script = self.load_i32(fp, FUNC_SCRIPT_SLOT_OFFSET);
        let call = self.call(
            Func::invalid(),
            &[
                self.cx,
                self.sp,
                argc,
                self.retval_out,
                script,
                self.new_target,
            ],
            &[Type::I32, Type::I32],
        );
        self.baseline_calls.push(call);
        let err = self.push_val(ValueDef::PickOutput(call, 0, Type::I32));
        let eff = self.push_val(ValueDef::PickOutput(call, 1, Type::I32));
        self.terminate(Terminator::Return {
            values: vec![err, eff],
        });
    }
}
