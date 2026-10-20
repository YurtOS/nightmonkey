//! guards: methods of `Lower`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    /// A fallible check: `ok` to the `ok` edge with `outs`, else `fail`.
    /// Under the stress mode, a guard whose failure exits also fails
    /// whenever the stress helper says so. (One whose failure merges back,
    /// a tag test, must keep its meaning.)
    pub(super) fn guard(&mut self, inst: mir::Inst, ok: Value, outs: &[Value]) -> R<()> {
        let fail_blk = self.f.insts[inst].succs[1].block;
        let exits = self
            .f
            .terminator(fail_blk)
            .is_some_and(|t| self.f.insts[t].op.exit_shape().is_some());
        let ok = if self.stress == 0 || !exits {
            ok
        } else {
            let n = self.i32c(self.stress);
            let fail = self.call1(self.h.mir_stress, &[n], Type::I32);
            let pass = self.un(Operator::I32Eqz, fail, Type::I32);
            self.bin(Operator::I32And, ok, pass, Type::I32)
        };
        let t = self.edge(inst, 0, outs)?;
        let f = self.edge(inst, 1, &[])?;
        let Some(census) = self
            .exit_census
            .filter(|_| self.guard_exits(fail_blk).is_some())
        else {
            self.cond_br(ok, t, f);
            return Ok(());
        };
        // `--mir-exit-census`: which guard failed (an exit's block is
        // shared by every guard at its pc).
        let counted = self.body.add_block();
        self.cond_br(ok, t, Self::to(counted));
        self.cur = counted;
        let id = self.census_guard(inst, fail_blk);
        let (k, i) = (
            self.i32c(crate::options::MIR_GUARD_CENSUS_KIND),
            self.i32c(id),
        );
        self.call1(census, &[k, i], Type::I32);
        if let Some((w, idx, keys)) = self.guard_word {
            // A layout guard's miss, by the word: `id * 32 +` sentinel 16,
            // no identity 8, identity in range 4, TYPES 2, SLOTS 1.
            let bit = |l: &mut Self, v: Value, sh: u32| {
                let s = l.i32c(sh);
                l.bin(Operator::I32Shl, v, s, Type::I32)
            };
            let c31 = self.i32c(31);
            let sent = self.bin(Operator::I32ShrU, w, c31, Type::I32);
            let z = self.i32c(0);
            let none = self.bin(Operator::I32Eq, idx, z, Type::I32);
            let lo = self.i32c(keys.lo.get() + 1);
            let rel = self.bin(Operator::I32Sub, idx, lo, Type::I32);
            let span = self.i32c(keys.hi.get() - keys.lo.get());
            let inr = self.bin(Operator::I32LeU, rel, span, Type::I32);
            let tb = self.i32c(CLASS_WORD_SHALLOW);
            let ty = self.bin(Operator::I32And, w, tb, Type::I32);
            let ty = self.bin(Operator::I32Ne, ty, z, Type::I32);
            let sbit = self.i32c(CLASS_WORD_SLOTS);
            let sl = self.bin(Operator::I32And, w, sbit, Type::I32);
            let sl = self.bin(Operator::I32Ne, sl, z, Type::I32);
            let mut r = sl;
            for (v, sh) in [(ty, 1), (inr, 2), (none, 3), (sent, 4)] {
                let b = bit(self, v, sh);
                r = self.bin(Operator::I32Or, r, b, Type::I32);
            }
            let base = self.i32c(id * 32);
            let rid = self.bin(Operator::I32Add, base, r, Type::I32);
            let k = self.i32c(crate::options::MIR_GUARD_CLASS_CENSUS_KIND);
            self.call1(census, &[k, rid], Type::I32);
            // The object itself (the runtime's trace prints it).
            let obj = self.guard_obj.unwrap();
            let k = self.i32c(crate::options::MIR_GUARD_OBJECT_CENSUS_KIND);
            self.call1(census, &[k, obj], Type::I32);
        }
        self.terminate(Terminator::Br { target: f });
        Ok(())
    }

    /// The pc of the exit (root or inline) guard failure block `b` ends in.
    pub(super) fn guard_exits(&self, b: mir::Block) -> Option<Pc> {
        let t = self.f.terminator(b)?;
        match self.f.insts[t].op {
            Opcode::Exit { pc, .. }
            | Opcode::ExitThrow { pc, .. }
            | Opcode::ExitInline { pc, .. } => Some(pc),
            _ => None,
        }
    }

    /// The static record of a guard-failure census id: the guard, its
    /// operands' types, and the exit it takes.
    pub(super) fn census_guard(&mut self, inst: mir::Inst, fail_blk: mir::Block) -> u32 {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = &self.f.insts[inst];
        let tys: Vec<String> = d
            .args
            .iter()
            .map(|&a| format!("{:?}", self.ty(a)))
            .collect();
        let pc = self.guard_exits(fail_blk).unwrap();
        let t = self.f.terminator(fail_blk).unwrap();
        let inl = matches!(self.f.insts[t].op, Opcode::ExitInline { .. });
        crate::diag_line!(
            "night: mir guard {id} sid#{} exit pc {pc}{} {} {:?} [{}]",
            self.f.script,
            if inl { " (inline)" } else { "" },
            mir::print::mnemonic(&d.op),
            d.op,
            tys.join(", ")
        );
        id
    }

    /// A get IC's inline ways: with
    /// `recv` an object whose shape one of the site's ways names (way 0 at
    /// `way0`, the rest `INLINE_IC_WAY_BYTES` apart from its row offset
    /// `way_off`), the value from its own fixed slot, or through the way's
    /// holder (a prototype method) while the holder keeps its shape,
    /// taking `ok_clean`; else `probe`.
    /// A property IC's receiver test: an object, or `miss`. Skipped when
    /// the receiver's type (`args[0]`) already says object.
    pub(super) fn check_recv_object(&mut self, inst: mir::Inst, recv: Value, miss: Block) {
        let tags = value_tags(&self.ty(self.f.insts[inst].args[0]));
        if tags.is_nonempty_subset_of(TagSet::OBJECT) {
            return;
        }
        let tag = self.tag_of(recv);
        let is_obj = self.tag_is(tag, TAG_OBJECT as u32);
        self.check(is_obj, miss);
    }

    /// Branch to `fail` unless `cond`.
    pub(super) fn check(&mut self, cond: Value, fail: Block) {
        let cont = self.body.add_block();
        self.cond_br(cond, Self::to(cont), Self::to(fail));
        self.cur = cont;
    }
}
