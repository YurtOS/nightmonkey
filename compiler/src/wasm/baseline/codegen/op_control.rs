//! op control: the lowering of one run of arms of `Gen::op`, split out of `codegen.rs` unchanged.

use super::*;

impl<'a> Gen<'a> {
    pub(super) fn op_control(&mut self, p: &mut BytecodeParser, op: JSOp) -> R<bool> {
        use JSOp::*;
        let d = self.d;
        let h = self.h;
        match op {
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
            _ => return Ok(false),
        }
        Ok(true)
    }
}
