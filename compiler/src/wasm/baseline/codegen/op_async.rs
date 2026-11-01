//! op async: the lowering of one run of arms of `Gen::op`, split out of `codegen.rs` unchanged.

use super::*;

impl<'a> Gen<'a> {
    pub(super) fn op_async(&mut self, p: &mut BytecodeParser, op: JSOp) -> R<bool> {
        use JSOp::*;
        let d = self.d;
        let h = self.h;
        match op {
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
            #[cfg(feature = "ff147")]
            IsGenClosing => {
                use crate::wasm::translate::MAGIC_GENERATOR_CLOSING;
                let v = self.slot(d - 1);
                let m = self.i64c((TAG_MAGIC << 32) | MAGIC_GENERATOR_CLOSING);
                let b = self.binop(Operator::I64Eq, v, m, Type::I32);
                let r = self.boxed_bool(b);
                self.set_slot(d, r);
            }
            #[cfg(feature = "ff147")]
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
            _ => return Ok(false),
        }
        Ok(true)
    }
}
