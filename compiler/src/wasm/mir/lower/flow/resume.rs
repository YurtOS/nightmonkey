//! resume: methods of `Lower`, split out of `flow.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    /// The entry. Under `ARGC_ONRAMP_BIT` (baseline at a loop header),
    /// enter the onramp root the resume word names, with its params read
    /// from the baseline frame. Otherwise, pad the formals the caller did
    /// not pass with undefined (the frame below the rooting slots must hold
    /// valid Values), then enter the entry root with callee, `this` and the
    /// formals.
    pub(in crate::wasm::mir::lower) fn entry(&mut self) -> R<()> {
        if self.forward_resume {
            // An inlined copy's exit finishes the call in baseline through
            // this entry (§5.5): hand the frame, as it is, to the baseline
            // body.
            let rb = self.i32c(ARGC_RESUME_BIT);
            let resume = self.bin(Operator::I32And, self.argc, rb, Type::I32);
            let (fwd, rest) = (self.body.add_block(), self.body.add_block());
            self.cond_br(resume, Self::to(fwd), Self::to(rest));
            self.cur = fwd;
            let argc = self.argc;
            self.tail_to_baseline(argc);
            self.cur = rest;
        }
        let bit = self.i32c(ARGC_ONRAMP_BIT);
        self.onramp_flag = self.bin(Operator::I32And, self.argc, bit, Type::I32);
        let flags = self.i32c(!ARGC_FLAGS);
        self.argc = self.bin(Operator::I32And, self.argc, flags, Type::I32);
        if self.layout.rebase_vp {
            // Past the actuals beyond the formals, as baseline's `vp`.
            let n = self.i32c(self.layout.nargs);
            let extra = self.bin(Operator::I32Sub, self.argc, n, Type::I32);
            let more = self.bin(Operator::I32GtU, self.argc, n, Type::I32);
            let zero = self.i32c(0);
            let extra = self.select(Type::I32, extra, zero, more);
            let eight = self.i32c(8);
            let bytes = self.bin(Operator::I32Mul, extra, eight, Type::I32);
            self.vp = self.bin(Operator::I32Add, self.sp, bytes, Type::I32);
        }
        let onramps: Vec<(Pc, mir::Block)> = self
            .f
            .roots
            .iter()
            .filter_map(|r| match r.kind {
                RootKind::Onramp(pc) => Some((pc, r.block)),
                RootKind::Entry | RootKind::Resume { .. } => None,
            })
            .collect();
        if !onramps.is_empty() {
            let disp = self.body.add_block();
            let fresh = self.body.add_block();
            self.cond_br(self.onramp_flag, Self::to(disp), Self::to(fresh));
            self.cur = disp;
            let word = self.load_i32(self.vp, self.layout.resume());
            for (pc, root) in onramps {
                let w = ResumeWord {
                    pc,
                    mode: ResumeMode::Continue,
                };
                let k = self.i32c(w.encode() as u32);
                let hit = self.bin(Operator::I32Eq, word, k, Type::I32);
                let (yes, no) = (self.body.add_block(), self.body.add_block());
                self.cond_br(hit, Self::to(yes), Self::to(no));
                self.cur = yes;
                self.enter_onramp(pc, root)?;
                self.cur = no;
            }
            self.terminate(Terminator::Unreachable);
            self.cur = fresh;
        }
        if self.is_gen {
            // A generator's resume stages the generator-closing magic as
            // `this`, which no call passes (`EnterNightResume`).
            let thisv = self.load_i64(self.sp, FrameLayout::THIS);
            let magic = self.i64c((TAG_MAGIC << 32) | MAGIC_GENERATOR_CLOSING);
            let is_resume = self.bin(Operator::I64Eq, thisv, magic, Type::I32);
            let (res, fresh) = (self.body.add_block(), self.body.add_block());
            self.cond_br(is_resume, Self::to(res), Self::to(fresh));
            self.cur = res;
            self.gen_resume_dispatch()?;
            self.cur = fresh;
        }
        let root = self
            .f
            .roots
            .iter()
            .find(|r| r.kind == RootKind::Entry)
            .ok_or("lowering: no entry root")?
            .block;
        let undef = self.i64c(UNDEF);
        let mut vals = vec![
            self.load_i64(self.sp, FrameLayout::CALLEE),
            self.load_i64(self.sp, FrameLayout::THIS),
        ];
        for i in 0..self.layout.nargs {
            let off = self.layout.arg(i);
            let cur = self.load_i64(self.sp, off);
            let iv = self.i32c(i);
            let keep = self.bin(Operator::I32LtU, iv, self.argc, Type::I32);
            let v = self.select(Type::I64, cur, undef, keep);
            self.store_i64(self.sp, off, v);
            vals.push(v);
        }
        // The rest of the baseline frame, as its prologue would set it: the
        // GC traces it (it is below every `top` MIR publishes), and exits
        // leave dead slots as they find them.
        let l = self.layout;
        let vp = self.vp;
        for j in 0..l.nlocals {
            self.store_i64(vp, l.local(j), undef);
        }
        if l.has_args_obj {
            self.store_i64(vp, l.args_obj(), undef);
        }
        self.store_i64(vp, l.rval(), undef);
        // The environment: the callee's own, or none (§5.1).
        let env = if self.plain_env {
            let callee = self.load_i64(self.sp, 0);
            let f = self.un(Operator::I32WrapI64, callee, Type::I32);
            self.load_i64(f, FUNC_ENV_SLOT_OFFSET)
        } else {
            undef
        };
        if l.has_env {
            self.store_i64(vp, l.env(), env);
        }
        if l.has_new_target {
            self.store_i64(vp, l.new_target(), self.new_target);
        }
        let zero = self.i64c(TAG_INT32 << 32);
        self.store_i64(vp, l.resume(), zero);
        self.store_i64(vp, l.backoff(), zero);
        if self.is_gen {
            self.init_root_area();
        } else {
            // Not written here: every slot may hold a dead frame's value,
            // which the first may-GC call on each path clears (`root`'s
            // stale slots), as a loop's entry edges do where the loop may
            // GC (`enter_block`).
            self.dirty = (0..self.nslots).collect();
        }
        if self.own_env {
            // The CallObject inline from the script's row, where it is
            // armed: no GC, so nothing is rooted and the frame's values
            // stand. Else the helper, which fills the row.
            let cell = self.i32c(CONSTRUCT_CELL_ADDR_PLACEHOLDER);
            let idx = self.atoms.next_construct_cell();
            self.construct_cell_patches.push((cell, idx + 1));
            let slow = self.body.add_block();
            let env = self.env_inline(cell, slow);
            self.store_i64(vp, l.env(), env);
            let dirty = self.dirty.clone();
            self.enter_entry_root(root, &vals)?;
            self.cur = slow;
            self.dirty = dirty;
            // The GC may run: nothing is live yet, so its scan stops at the
            // fixed frame, below every rooting slot. Failing, the throw has
            // no handler (baseline's prologue: pc 0, depth 0).
            self.gc_top = self.root_base;
            let top = self.add_off(self.vp, self.top_off(0));
            let script = self.script_ptr();
            let ok = self.call1(
                self.h.env_setup,
                &[self.cx, top, self.sp, script, cell],
                Type::I32,
            );
            let (made, fail) = (self.body.add_block(), self.body.add_block());
            self.cond_br(ok, Self::to(made), Self::to(fail));
            self.cur = fail;
            let one = self.i32c(1);
            self.ret(one);
            self.cur = made;
            let env = self.load_i64(top, 0);
            self.store_i64(vp, l.env(), env);
            // The GC updated the frame, not the values read from it before.
            vals = vec![
                self.load_i64(self.sp, FrameLayout::CALLEE),
                self.load_i64(self.sp, FrameLayout::THIS),
            ];
            for i in 0..self.layout.nargs {
                vals.push(self.load_i64(self.sp, self.layout.arg(i)));
            }
        }
        self.enter_entry_root(root, &vals)
    }

    /// The entry's last step: enter the entry root with callee, `this`
    /// and the formals, `vals`.
    pub(in crate::wasm::mir::lower) fn enter_entry_root(
        &mut self,
        root: mir::Block,
        vals: &[Value],
    ) -> R<()> {
        let params = self.f.blocks[root].params.clone();
        if params.len() != vals.len() {
            return Err("lowering: the entry root's params are not the frame".into());
        }
        let mut args = vec![];
        for (&p, &v) in params.iter().zip(vals) {
            let t = self.ty(p);
            if machine(&t).is_some() {
                args.push(self.unboxed_managed(&t, v));
            }
        }
        let t = self.edge_into(root, args)?;
        self.terminate(Terminator::Br { target: t });
        Ok(())
    }

    /// Enter onramp root `root` (loop header `pc`) with the baseline
    /// frame's state at `pc`: `this`, formals, locals, rval and the
    /// operand stack.
    /// A generator's resume (baseline's `finalize_gen_dispatch`): read
    /// the descriptor `EnterNightResume` staged over the locals, set the
    /// fixed slots a fresh prologue would, restore the locals, operands
    /// and environment from the generator, push the resume protocol's
    /// `[sent value, generator, resume kind]` at the landing's depth, and
    /// enter the `Resume` root for the saved resume index.
    pub(in crate::wasm::mir::lower) fn gen_resume_dispatch(&mut self) -> R<()> {
        let l = self.layout;
        let vp = self.vp;
        let desc = self.add_off(vp, l.local_base());
        let ridx = self.load_i32(desc, 0);
        let roots: Vec<(u32, Pc, mir::Block)> = self
            .f
            .roots
            .iter()
            .filter_map(|r| match r.kind {
                RootKind::Resume { index, pc } => Some((index, pc, r.block)),
                _ => None,
            })
            .collect();
        // A yield this body has no root for (one in catch code, which is
        // baseline's, suspended there after an exit): baseline's own
        // resume dispatch, with the descriptor untouched.
        let other = self.body.add_block();
        let max_k = roots.iter().map(|&(k, _, _)| k).max().unwrap_or(0);
        let mut targets = vec![Self::to(other); (max_k + 1) as usize];
        let disp = self.cur;
        for (k, pc, root) in roots {
            let b = self.body.add_block();
            targets[k as usize] = Self::to(b);
            self.cur = b;
            let desc = self.add_off(vp, l.local_base());
            let rkind = self.load_i32(desc, 4);
            let rgen = self.load_i64(desc, 8);
            let rarg = self.load_i64(desc, 16);
            let undef = self.i64c(UNDEF);
            let mut fixed = l.optional_slots();
            fixed.push(l.rval());
            for off in fixed {
                self.store_i64(vp, off, undef);
            }
            let zero = self.i64c(TAG_INT32 << 32);
            self.store_i64(vp, l.resume(), zero);
            self.store_i64(vp, l.backoff(), zero);
            let lp = self.add_off(vp, l.local_base());
            let nl = self.i32c(l.nlocals);
            let ep = if self.plain_env || self.own_env {
                self.add_off(vp, l.env())
            } else {
                self.i32c(0)
            };
            let ops = self.add_off(vp, l.operand_base());
            self.call(
                self.h.gen_restore,
                &[self.cx, rgen, lp, nl, ep, ops],
                &[Type::I32],
            );
            let depth = *self
                .f
                .frame
                .depths
                .get(&pc)
                .ok_or("lowering: no depth at a resume root")?;
            let saved = depth
                .checked_sub(3)
                .ok_or("lowering: a resume landing below depth 3")?;
            self.store_i64(vp, l.operand(saved), rarg);
            self.store_i64(vp, l.operand(saved + 1), rgen);
            let kind = self.box_tagged(TAG_INT32, rkind);
            self.store_i64(vp, l.operand(saved + 2), kind);
            self.enter_onramp(pc, root)?;
        }
        self.cur = disp;
        let known: Vec<(u32, BlockTarget)> = targets
            .iter()
            .enumerate()
            .filter(|(_, t)| t.block != other)
            .map(|(k, t)| (u32::try_from(k).unwrap(), t.clone()))
            .collect();
        if known.len() <= RESUME_CHAIN_MAX {
            // A few yields (the usual generator, an async function's
            // awaits): compares, each a predicted branch, rather than a
            // `br_table`'s bounds check and indirect jump.
            for (k, t) in known {
                let kv = self.i32c(k);
                let hit = self.bin(Operator::I32Eq, ridx, kv, Type::I32);
                let next = self.body.add_block();
                self.cond_br(hit, t, Self::to(next));
                self.cur = next;
            }
            self.terminate(Terminator::Br {
                target: Self::to(other),
            });
        } else {
            self.terminate(Terminator::Select {
                value: ridx,
                targets,
                default: Self::to(other),
            });
        }
        self.cur = other;
        let argc = self.argc;
        self.tail_to_baseline(argc);
        Ok(())
    }

    pub(in crate::wasm::mir::lower) fn enter_onramp(&mut self, pc: Pc, root: mir::Block) -> R<()> {
        let l = self.layout;
        let (sp, vp) = (self.sp, self.vp);
        let depth = *self
            .f
            .frame
            .depths
            .get(&pc)
            .ok_or("lowering: no depth at an onramp")?;
        let mut vals = vec![self.load_i64(sp, FrameLayout::THIS)];
        for i in 0..l.nargs {
            vals.push(self.load_i64(sp, l.arg(i)));
        }
        for j in 0..l.nlocals {
            vals.push(self.load_i64(vp, l.local(j)));
        }
        vals.push(self.load_i64(vp, l.rval()));
        for k in 0..depth {
            vals.push(self.load_i64(vp, l.operand(k)));
        }
        if vals.len() != self.f.blocks[root].params.len() {
            return Err("lowering: an onramp root's params are not the frame".into());
        }
        // After reading the operands: the rooting area overlaps them.
        self.init_root_area();
        let t = self.edge_into(root, vals)?;
        self.terminate(Terminator::Br { target: t });
        Ok(())
    }
}
