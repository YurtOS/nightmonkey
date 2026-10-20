//! inst calls: the lowering of one run of arms of `Lower::inst`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    pub(super) fn inst_calls(
        &mut self,
        inst: mir::Inst,
        d: &mir::func::InstData,
        a: &[Value],
    ) -> R<bool> {
        match d.op {
            Opcode::Call => self.js_call_op(inst, a, false)?,
            Opcode::CallIter => self.js_call_op(inst, a, true)?,
            Opcode::CallEval(pc) => self.js_eval_op(inst, a, pc)?,
            Opcode::GuardScript(sid) => self.guard_script(inst, d, a, sid)?,
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
            Opcode::InlineEnter => self.inline_enter(d, a)?,
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
            _ => return Ok(false),
        }
        Ok(true)
    }
}
