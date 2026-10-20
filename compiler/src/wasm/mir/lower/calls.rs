//! calls: methods of `Lower`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    /// A generic JS op through its helper (`ok_clean`, `ok_dirty`, `err`).
    /// With no dynamic effect report yet, success takes `ok_dirty`, which
    /// is always sound.
    pub(super) fn js_op(&mut self, inst: mir::Inst, op: &Opcode, a: &[Value]) -> R<()> {
        if let Opcode::JsCompare(cc) = *op {
            self.equality_arms(inst, cc, a)?;
        }
        let (f, args, bool_out) = self.op_helper(op, a);
        self.js_call(inst, f, &args, bool_out)
    }

    /// The runtime helper of a generic numeric op or compare, its
    /// arguments, and whether its result is a bool.
    pub(super) fn op_helper(&mut self, op: &Opcode, a: &[Value]) -> (Func, Vec<Value>, bool) {
        let h = self.h;
        match *op {
            Opcode::JsAdd => (h.add, vec![a[0], a[1]], false),
            Opcode::JsBinop(b) => {
                let kind = match b {
                    JsBinop::Sub => BINOP_SUB,
                    JsBinop::Mul => BINOP_MUL,
                    JsBinop::Div => BINOP_DIV,
                    JsBinop::Mod => BINOP_MOD,
                    JsBinop::BitAnd => BINOP_BITAND,
                    JsBinop::BitOr => BINOP_BITOR,
                    JsBinop::BitXor => BINOP_BITXOR,
                    JsBinop::Lsh => BINOP_LSH,
                    JsBinop::Rsh => BINOP_RSH,
                    JsBinop::Ursh => BINOP_URSH,
                    JsBinop::Pow => return (h.pow, vec![a[0], a[1]], false),
                };
                let k = self.i32c(kind);
                (h.binop, vec![k, a[0], a[1]], false)
            }
            Opcode::JsUnop(u) => match u {
                JsUnop::Neg => (h.neg, vec![a[0]], false),
                JsUnop::Pos => (h.pos, vec![a[0]], false),
                JsUnop::BitNot | JsUnop::Inc | JsUnop::Dec => {
                    let kind = match u {
                        JsUnop::BitNot => BINOP_BITNOT,
                        JsUnop::Inc => BINOP_INC,
                        _ => BINOP_DEC,
                    };
                    let k = self.i32c(kind);
                    (h.binop, vec![k, a[0], a[0]], false)
                }
            },
            Opcode::JsCompare(cc) => {
                let kind = match cc {
                    JsCc::Eq => CMP_EQ,
                    JsCc::Ne => CMP_NE,
                    JsCc::StrictEq => CMP_STRICTEQ,
                    JsCc::StrictNe => CMP_STRICTNE,
                    JsCc::Lt => CMP_LT,
                    JsCc::Le => CMP_LE,
                    JsCc::Gt => CMP_GT,
                    JsCc::Ge => CMP_GE,
                };
                let k = self.i32c(kind);
                (h.compare, vec![k, a[0], a[1]], true)
            }
            Opcode::JsToNumeric => (h.tonumeric, vec![a[0]], false),
            _ => unreachable!(),
        }
    }

    /// `prim.*` (`Opcode::Prim`): the generic op's inline arms taking
    /// `ok`; then `fail` where an operand's conversion would call user
    /// code (an object whose ToPrimitive is not Object.prototype's own,
    /// unless an equality compares it with an object, null or undefined;
    /// never in a strict one); then its helper, which runs no user code on
    /// such operands: `ok` with the result, or `err` for the op's own
    /// TypeError or RangeError.
    pub(super) fn prim_op(
        &mut self,
        inst: mir::Inst,
        p: crate::mir::ops::PrimOp,
        a: &[Value],
    ) -> R<()> {
        use crate::mir::ops::PrimOp;
        let g = p.generic();
        self.numeric_fast_arms(inst, &g, a)?;
        if p == PrimOp::Add {
            let d = self.f.insts[inst].clone();
            self.concat_arm(inst, &d, a)?;
        }
        if let PrimOp::Compare(cc) = p {
            self.equality_arms(inst, cc, a)?;
        }
        let is_obj = |l: &mut Self, v: Value| {
            let t = l.tag_of(v);
            l.tag_is(t, TAG_OBJECT as u32)
        };
        let user = match p {
            PrimOp::Compare(JsCc::StrictEq | JsCc::StrictNe) => None,
            PrimOp::Compare(JsCc::Eq | JsCc::Ne) => {
                // Converted only against a primitive other than null or
                // undefined.
                let (oa, ob) = (is_obj(self, a[0]), is_obj(self, a[1]));
                let one = self.bin(Operator::I32Xor, oa, ob, Type::I32);
                let nullish = |l: &mut Self, v: Value| {
                    let t = l.tag_of(v);
                    let u = l.tag_is(t, TAG_UNDEFINED as u32);
                    let n = l.tag_is(t, TAG_NULL as u32);
                    l.bin(Operator::I32Or, u, n, Type::I32)
                };
                let (na, nb) = (nullish(self, a[0]), nullish(self, a[1]));
                let either = self.bin(Operator::I32Or, na, nb, Type::I32);
                let neither = self.un(Operator::I32Eqz, either, Type::I32);
                let conv = self.bin(Operator::I32And, one, neither, Type::I32);
                let (ua, ub) = (self.converts_user(a[0]), self.converts_user(a[1]));
                let u = self.bin(Operator::I32Or, ua, ub, Type::I32);
                Some(self.bin(Operator::I32And, conv, u, Type::I32))
            }
            PrimOp::Unop(_) | PrimOp::ToNumeric => Some(self.converts_user(a[0])),
            _ => {
                let (ua, ub) = (self.converts_user(a[0]), self.converts_user(a[1]));
                Some(self.bin(Operator::I32Or, ua, ub, Type::I32))
            }
        };
        if let Some(user) = user {
            let fail = self.edge(inst, 1, &[])?;
            let go = self.body.add_block();
            self.cond_br(user, fail, Self::to(go));
            self.cur = go;
        }
        let (f, args, bool_out) = self.op_helper(&g, a);
        self.slow_census(inst);
        let live = self.live_across(inst);
        let (ok, result) = self.gc_call(f, &args, &live)?;
        self.epoch_same = None;
        let out = if bool_out {
            self.un(Operator::I32WrapI64, result, Type::I32)
        } else {
            result
        };
        let t = self.edge(inst, 0, &[out])?;
        let e = self.edge(inst, 2, &[])?;
        self.cond_br(ok, t, e);
        Ok(())
    }

    /// Call `f` rooted; on success take `ok_dirty` with the result (its
    /// boolean payload if `bool_out`), else `err`.
    pub(super) fn js_call(
        &mut self,
        inst: mir::Inst,
        f: Func,
        args: &[Value],
        bool_out: bool,
    ) -> R<()> {
        let live = self.live_across(inst);
        let (ok, result) = self.gc_call(f, args, &live)?;
        let out = if bool_out {
            self.un(Operator::I32WrapI64, result, Type::I32)
        } else {
            result
        };
        let same = self.epoch_same.take().expect("gc_call sampled the epoch");
        self.clean_or_dirty(inst, ok, same, &[out])
    }

    /// Where environment object `obj`'s slot `slot` is, as a base and an
    /// offset: decided statically, as Ion's `GetAliasedVar` is
    /// (`MAX_FIXED_SLOTS`).
    pub(super) fn env_slot(&mut self, obj: Value, slot: u32) -> (Value, u32) {
        if slot < MAX_FIXED_SLOTS {
            (obj, FIXED_SLOTS_BASE + 8 * slot)
        } else {
            let slots = self.load_i32(obj, NATIVE_SLOTS_OFFSET);
            (slots, 8 * (slot - MAX_FIXED_SLOTS))
        }
    }

    /// The script's `JSScript*`, re-derived from the callee (a rooted frame
    /// slot), as baseline's `script_ptr`.
    pub(super) fn script_ptr(&mut self) -> Value {
        let fid = self.cur_frame as usize;
        let callee = if fid == 0 {
            self.load_i64(self.sp, FrameLayout::CALLEE)
        } else {
            self.load_i64(self.vp, self.frame_off[fid] + FrameLayout::CALLEE)
        };
        let f = self.un(Operator::I32WrapI64, callee, Type::I32);
        self.load_i32(f, FUNC_SCRIPT_SLOT_OFFSET)
    }

    /// `lhs instanceof rhs` inline, taking
    /// `ok_clean`: with the site's cell describing `rhs` (its shape, under
    /// the live IC generation), read its `.prototype` from the cached slot
    /// and walk `lhs`'s prototype chain (bounded) for it. Anything else
    /// falls through to the helper, which also fills the cell.
    pub(super) fn instanceof_arms(
        &mut self,
        inst: mir::Inst,
        lhs: Value,
        rhs: Value,
        cell: Value,
    ) -> R<()> {
        let slow = self.body.add_block();
        // (An object by its type: a predicted global's function, say.)
        if !value_tags(&self.ty(self.f.insts[inst].args[1])).is_nonempty_subset_of(TagSet::OBJECT) {
            let rt = self.tag_of(rhs);
            let r_obj = self.tag_is(rt, TAG_OBJECT as u32);
            self.check(r_obj, slow);
        }
        let rptr = self.un(Operator::I32WrapI64, rhs, Type::I32);
        let rshape = self.load_i32(rptr, SHAPE_OFFSET);
        let cshape = self.load_i32(cell, 0);
        let cgen = self.load_i32(cell, IOF_CELL_GEN);
        let gen_addr = self.i32c(self.h.prop_ic_gen_base);
        let live_gen = self.load_i32(gen_addr, 0);
        let s_ok = self.bin(Operator::I32Eq, rshape, cshape, Type::I32);
        let g_ok = self.bin(Operator::I32Eq, cgen, live_gen, Type::I32);
        let hit = self.bin(Operator::I32And, s_ok, g_ok, Type::I32);
        self.check(hit, slow);
        // The live `.prototype`, from the cached slot.
        let enc = self.load_i32(cell, IOF_CELL_SLOTENC);
        let one = self.i32c(1);
        let dynamic = self.bin(Operator::I32And, enc, one, Type::I32);
        let not1 = self.i32c(!1);
        let off = self.bin(Operator::I32And, enc, not1, Type::I32);
        let slots = self.load_i32(rptr, NATIVE_SLOTS_OFFSET);
        let sb = self.select(Type::I32, slots, rptr, dynamic);
        let addr = self.bin(Operator::I32Add, sb, off, Type::I32);
        let pval = self.load_i64(addr, 0);
        let pt = self.tag_of(pval);
        let p_obj = self.tag_is(pt, TAG_OBJECT as u32);
        self.check(p_obj, slow);
        let pptr = self.un(Operator::I32WrapI64, pval, Type::I32);
        let (t_b, f_b) = (self.body.add_block(), self.body.add_block());
        let lt = self.tag_of(lhs);
        let l_obj = self.tag_is(lt, TAG_OBJECT as u32);
        let lptr = self.un(Operator::I32WrapI64, lhs, Type::I32);
        let walk = self.body.add_block();
        let cur = self.body.add_blockparam(walk, Type::I32);
        let depth = self.body.add_blockparam(walk, Type::I32);
        let d0 = self.i32c(crate::constants::IOF_WALK_DEPTH);
        // A primitive is never an instance.
        self.cond_br(
            l_obj,
            BlockTarget {
                block: walk,
                args: vec![lptr, d0],
            },
            Self::to(f_b),
        );
        self.cur = walk;
        let ws = self.load_i32(cur, SHAPE_OFFSET);
        let wb = self.load_i32(ws, SHAPE_BASESHAPE_OFFSET);
        let proto = self.load_i32(wb, BASESHAPE_PROTO_OFFSET);
        let found = self.bin(Operator::I32Eq, proto, pptr, Type::I32);
        let wa = self.body.add_block();
        self.cond_br(found, Self::to(t_b), Self::to(wa));
        // A TaggedProto below 2 is null (0: not an instance) or lazy (1).
        self.cur = wa;
        let two = self.i32c(2);
        let low = self.bin(Operator::I32LtU, proto, two, Type::I32);
        let (wb2, wc) = (self.body.add_block(), self.body.add_block());
        self.cond_br(low, Self::to(wb2), Self::to(wc));
        self.cur = wb2;
        let z = self.i32c(0);
        let is_null = self.bin(Operator::I32Eq, proto, z, Type::I32);
        self.cond_br(is_null, Self::to(f_b), Self::to(slow));
        self.cur = wc;
        let z = self.i32c(0);
        let spent = self.bin(Operator::I32Eq, depth, z, Type::I32);
        let next = self.body.add_block();
        self.cond_br(spent, Self::to(slow), Self::to(next));
        self.cur = next;
        let one = self.i32c(1);
        let nd = self.bin(Operator::I32Sub, depth, one, Type::I32);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: walk,
                args: vec![proto, nd],
            },
        });
        for (b, v) in [(t_b, 1u64), (f_b, 0u64)] {
            self.cur = b;
            let r = self.i64c((TAG_BOOLEAN << 32) | v);
            let t = self.edge(inst, 0, &[r])?;
            self.terminate(Terminator::Br { target: t });
        }
        self.cur = slow;
        Ok(())
    }

    /// The helpers' atom id for MIR atom `a`, as an i32 constant.
    pub(super) fn atom(&mut self, a: mir::entity::AtomId) -> Value {
        let id = self.atoms.intern_chars(self.mm.atoms[a].chars());
        self.i32c(id)
    }

    /// A JS call (`callee`, `this`, args; all boxed). The callee's frame
    /// is written just above the rooting slots, and the call enters a
    /// compiled callee's body directly when it can (`call_indirect`, as
    /// baseline's calls do: JS call depth is then bounded by the
    /// NightStack, not the native stack), else the generic helper. The
    /// result is at the frame's top. Success takes `ok_dirty`.
    /// The op's frame's actuals: where its callee, `this` and actuals
    /// start, and its actual count. An inlined callee's are its inline
    /// frame's, its count the call's (`InlineFrame::argc`).
    pub(super) fn actuals(&mut self) -> (Value, Value) {
        let fid = self.cur_frame as usize;
        if fid == 0 {
            return (self.sp, self.argc);
        }
        let fr = &self.f.inline_frames[fid - 1];
        let n = fr.argc.unwrap_or(fr.shape.formals);
        let base = self.add_off(self.vp, self.frame_off[fid]);
        (base, self.i32c(n))
    }

    /// The op's frame's env slot offset from `vp`.
    pub(super) fn frame_env_off(&self) -> u32 {
        let fid = self.cur_frame as usize;
        self.frame_voff[fid] + self.frame_layouts[fid].env()
    }

    /// The op's frame's current environment (boxed), from its env slot.
    pub(super) fn frame_env(&mut self) -> Value {
        let fid = self.cur_frame as usize;
        if self.fixed[fid] == Fixed::None {
            // Not in the frame: the callee's own (an inlined callee has no
            // activation environment of its own).
            let callee = self.load_i64(self.vp, self.frame_off[fid] + FrameLayout::CALLEE);
            let fun = self.un(Operator::I32WrapI64, callee, Type::I32);
            return self.load_i64(fun, FUNC_ENV_SLOT_OFFSET);
        }
        let off = self.frame_env_off();
        self.load_i64(self.vp, off)
    }

    /// A direct `eval` (baseline's `Eval`): the call's frame
    /// `[callee, this, args]` above the live values, and the helper, which
    /// evaluates in the frame's environment (or calls a callee that is not
    /// `eval`).
    pub(super) fn js_eval_op(&mut self, inst: mir::Inst, ops: &[Value], pc: u32) -> R<()> {
        let live = self.live_across(inst);
        self.root(&live)?;
        let call_pre = self.epoch();
        let frame = self.top_off(live.len());
        for (k, &v) in ops.iter().enumerate() {
            self.store_i64(self.vp, frame + 8 * u32::try_from(k).unwrap(), v);
        }
        let argc = u32::try_from(ops.len() - 2).unwrap();
        let top_off = frame + 8 * (argc + 2);
        let base = self.add_off(self.vp, frame);
        let top = self.add_off(self.vp, top_off);
        let av = self.i32c(argc);
        let env = self.frame_env();
        let script = self.script_ptr();
        let pcv = self.i32c(pc);
        let ok = self.call1(
            self.h.eval,
            &[self.cx, top, base, av, env, script, pcv],
            Type::I32,
        );
        self.after_gc(&live);
        let result = self.load_i64(self.vp, top_off);
        let post = self.epoch();
        let same = self.bin(Operator::I32Eq, call_pre, post, Type::I32);
        self.clean_or_dirty(inst, ok, same, &[result])
    }

    pub(super) fn js_call_op(&mut self, inst: mir::Inst, ops: &[Value], iter: bool) -> R<()> {
        /// Headroom a compiled body may use past its actuals (the runtime
        /// entries' `kNightStackHeadroomSlots`).
        const HEADROOM: u32 = 64 * 1024;
        // A callee whose type proves its script (a predicted global's
        // read, a guarded closure): a compiled body (what proved it tested
        // that), no native, its table index in its script.
        let proven = match self.ty(self.f.insts[inst].args[0]) {
            MType::Val(v) if v.tags.is_nonempty_subset_of(TagSet::OBJECT) => match v.obj.kind {
                ObjKind::Function(Some(k)) => self.mm.script_addrs.get(&k).copied(),
                _ => None,
            },
            _ => None,
        };
        if let Some(addr) = proven {
            return self.js_call_proven(inst, ops, iter, addr, HEADROOM);
        }
        if ops.len() == 3 && !iter {
            let call = self.body.add_block();
            self.char_arms(inst, ops, call)?;
            self.cur = call;
        }
        if (3..=3 + PUSH_ARM_MAX_EXTRA).contains(&ops.len()) && !iter && self.names_atom("push") {
            let call = self.body.add_block();
            self.push_arm(inst, ops, call)?;
            self.cur = call;
        }
        if ops.len() == 3 && !iter && self.names_atom("get") {
            let call = self.body.add_block();
            self.map_get_arm(inst, ops, call)?;
            self.cur = call;
        }
        if ops.len() == 3 && !iter && (self.names_atom("exec") || self.names_atom("test")) {
            let call = self.body.add_block();
            self.regexp_arm(inst, ops, call)?;
            self.cur = call;
        }
        if ops.len() == 2 && !iter && self.names_atom("pop") {
            let call = self.body.add_block();
            self.pop_arm(inst, ops, call)?;
            self.cur = call;
        }
        // A leaf: nothing to root yet.
        let (funcidx, script, native) = self.classify_native(ops[0]);
        if (ops.len() == 3 || ops.len() == 4) && !iter {
            let call = self.body.add_block();
            self.math_arms(inst, ops, native, call)?;
            self.cur = call;
        }
        let live = self.live_across(inst);
        self.root(&live)?;
        let call_pre = self.epoch();
        let frame = self.top_off(live.len());
        for (k, &v) in ops.iter().enumerate() {
            self.store_i64(self.vp, frame + 8 * u32::try_from(k).unwrap(), v);
        }
        let argc = u32::try_from(ops.len() - 2).unwrap();
        let top_off = frame + 8 * (argc + 2);
        let base = self.add_off(self.vp, frame);
        let top = self.add_off(self.vp, top_off);
        let z = self.i32c(0);
        let fits = self.stack_fits(top, HEADROOM);
        let compiled = self.bin(Operator::I32Ne, funcidx, z, Type::I32);
        let direct = self.bin(Operator::I32And, compiled, fits, Type::I32);
        let (direct_b, generic_b, join) = (
            self.body.add_block(),
            self.body.add_block(),
            self.body.add_block(),
        );
        let ok = self.body.add_blockparam(join, Type::I32);
        self.cond_br(direct, Self::to(direct_b), Self::to(generic_b));

        self.cur = direct_b;
        let argc_v = self.i32c(argc);
        let undef = self.i64c(UNDEF);
        // The site's likely callees (one, or a guard
        // chain for a small polymorphic set): each one's table index (a
        // patched const; `u32::MAX` while uncompiled, so the arm is dead)
        // against the classified funcidx, and on a match a static `call`
        // of its body, which wasmtime may inline.
        let likely: Vec<u32> = self.f.insts[inst]
            .attach
            .map(|a| {
                self.f.attachments[a]
                    .targets
                    .iter()
                    .map(|k| k.get())
                    .collect()
            })
            .unwrap_or_default();
        for sid in likely {
            let expected = self.i32c(u32::MAX);
            let is_likely = self.bin(Operator::I32Eq, funcidx, expected, Type::I32);
            let (likely_b, indirect_b) = (self.body.add_block(), self.body.add_block());
            self.cond_br(is_likely, Self::to(likely_b), Self::to(indirect_b));
            self.cur = likely_b;
            let args = self
                .body
                .arg_pool
                .from_iter([self.cx, base, argc_v, top, script, undef].into_iter());
            let tys = self
                .body
                .type_pool
                .from_iter([Type::I32, Type::I32].into_iter());
            let call = self.push_val(ValueDef::Operator(
                Operator::Call {
                    function_index: self.h.direct_call_stub2,
                },
                args,
                tys,
            ));
            self.likely_patches.push((expected, call, sid));
            let err = self.push_val(ValueDef::PickOutput(call, 0, Type::I32));
            let ok_likely = self.un(Operator::I32Eqz, err, Type::I32);
            self.terminate(Terminator::Br {
                target: BlockTarget {
                    block: join,
                    args: vec![ok_likely],
                },
            });
            self.cur = indirect_b;
        }
        // Bodies sit `N` table slots below their adapters (`wasm/mod.rs`).
        let off = self.i32c(u32::MAX);
        self.body_off_patches.push(off);
        let body_idx = self.bin(Operator::I32Sub, funcidx, off, Type::I32);
        let args = self
            .body
            .arg_pool
            .from_iter([self.cx, base, argc_v, top, script, undef, body_idx].into_iter());
        let tys = self
            .body
            .type_pool
            .from_iter([Type::I32, Type::I32].into_iter());
        let call = self.push_val(ValueDef::Operator(
            Operator::CallIndirect {
                sig_index: self.h.night_abi_sig2,
                table_index: self.h.indirect_table,
            },
            args,
            tys,
        ));
        let err = self.push_val(ValueDef::PickOutput(call, 0, Type::I32));
        let ok_direct = self.un(Operator::I32Eqz, err, Type::I32);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: join,
                args: vec![ok_direct],
            },
        });

        self.cur = generic_b;
        let argc_v = self.i32c(argc);
        {
            // A native callee (the classify proved a function with no
            // script) goes straight to its JSNative;
            // `native_dispatch` punts the natives that need the generic
            // call's handling back to it.
            let (nat_b, gen_b) = (self.body.add_block(), self.body.add_block());
            self.cond_br(native, Self::to(nat_b), Self::to(gen_b));
            self.cur = nat_b;
            let ok_nat = self.call1(
                self.h.native_dispatch,
                &[self.cx, top, base, argc_v],
                Type::I32,
            );
            self.terminate(Terminator::Br {
                target: BlockTarget {
                    block: join,
                    args: vec![ok_nat],
                },
            });
            self.cur = gen_b;
        }
        // An iterator method's call reports an uncallable callee as the
        // iterator protocol does.
        let generic_f = if iter { self.h.call_iter } else { self.h.call };
        let ok_generic = self.call1(generic_f, &[self.cx, top, base, argc_v], Type::I32);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: join,
                args: vec![ok_generic],
            },
        });

        self.cur = join;
        self.after_gc(&live);
        let result = self.load_i64(self.vp, top_off);
        let post = self.epoch();
        let same = self.bin(Operator::I32Eq, call_pre, post, Type::I32);
        self.clean_or_dirty(inst, ok, same, &[result])
    }

    /// `js_call_op` for a callee of a known compiled script at `addr`: no
    /// native arms and no classify; the script's table index, and the
    /// site's static call of that body where it is the one the site
    /// predicts. (Such a callee is never a class constructor, whose call
    /// throws: no binding predicts one.)
    pub(super) fn js_call_proven(
        &mut self,
        inst: mir::Inst,
        ops: &[Value],
        iter: bool,
        addr: u32,
        headroom: u32,
    ) -> R<()> {
        let live = self.live_across(inst);
        self.root(&live)?;
        let call_pre = self.epoch();
        let frame = self.top_off(live.len());
        for (k, &v) in ops.iter().enumerate() {
            self.store_i64(self.vp, frame + 8 * u32::try_from(k).unwrap(), v);
        }
        let argc = u32::try_from(ops.len() - 2).unwrap();
        let top_off = frame + 8 * (argc + 2);
        let base = self.add_off(self.vp, frame);
        let top = self.add_off(self.vp, top_off);
        let fits = self.stack_fits(top, headroom);
        let (direct_b, generic_b, join) = (
            self.body.add_block(),
            self.body.add_block(),
            self.body.add_block(),
        );
        let ok = self.body.add_blockparam(join, Type::I32);
        self.cond_br(fits, Self::to(direct_b), Self::to(generic_b));

        self.cur = direct_b;
        let script = self.i32c(addr);
        let funcidx = self.load_i32(script, BASESCRIPT_NIGHTFUNCINDEX_OFFSET);
        let argc_v = self.i32c(argc);
        let undef = self.i64c(UNDEF);
        let likely: Vec<u32> = self.f.insts[inst]
            .attach
            .map(|a| {
                self.f.attachments[a]
                    .targets
                    .iter()
                    .map(|k| k.get())
                    .collect()
            })
            .unwrap_or_default();
        if let Some(&sid) = likely.first().filter(|_| likely.len() == 1) {
            let expected = self.i32c(u32::MAX);
            let is_likely = self.bin(Operator::I32Eq, funcidx, expected, Type::I32);
            let (likely_b, indirect_b) = (self.body.add_block(), self.body.add_block());
            self.cond_br(is_likely, Self::to(likely_b), Self::to(indirect_b));
            self.cur = likely_b;
            let args = self
                .body
                .arg_pool
                .from_iter([self.cx, base, argc_v, top, script, undef].into_iter());
            let tys = self
                .body
                .type_pool
                .from_iter([Type::I32, Type::I32].into_iter());
            let call = self.push_val(ValueDef::Operator(
                Operator::Call {
                    function_index: self.h.direct_call_stub2,
                },
                args,
                tys,
            ));
            self.likely_patches.push((expected, call, sid));
            let err = self.push_val(ValueDef::PickOutput(call, 0, Type::I32));
            let ok_likely = self.un(Operator::I32Eqz, err, Type::I32);
            self.terminate(Terminator::Br {
                target: BlockTarget {
                    block: join,
                    args: vec![ok_likely],
                },
            });
            self.cur = indirect_b;
        }
        let off = self.i32c(u32::MAX);
        self.body_off_patches.push(off);
        let body_idx = self.bin(Operator::I32Sub, funcidx, off, Type::I32);
        let args = self
            .body
            .arg_pool
            .from_iter([self.cx, base, argc_v, top, script, undef, body_idx].into_iter());
        let tys = self
            .body
            .type_pool
            .from_iter([Type::I32, Type::I32].into_iter());
        let call = self.push_val(ValueDef::Operator(
            Operator::CallIndirect {
                sig_index: self.h.night_abi_sig2,
                table_index: self.h.indirect_table,
            },
            args,
            tys,
        ));
        let err = self.push_val(ValueDef::PickOutput(call, 0, Type::I32));
        let ok_direct = self.un(Operator::I32Eqz, err, Type::I32);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: join,
                args: vec![ok_direct],
            },
        });

        // Out of stack: the generic call, which reports the overflow.
        self.cur = generic_b;
        let argc_v = self.i32c(argc);
        let generic_f = if iter { self.h.call_iter } else { self.h.call };
        let ok_generic = self.call1(generic_f, &[self.cx, top, base, argc_v], Type::I32);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: join,
                args: vec![ok_generic],
            },
        });

        self.cur = join;
        self.after_gc(&live);
        let result = self.load_i64(self.vp, top_off);
        let post = self.epoch();
        let same = self.bin(Operator::I32Eq, call_pre, post, Type::I32);
        self.clean_or_dirty(inst, ok, same, &[result])
    }

    /// Whether the function names atom `s` (a builtin arm's gate: an arm
    /// costs code where it never fires).
    pub(super) fn names_atom(&self, s: &str) -> bool {
        self.mm
            .atoms
            .iter()
            .any(|(_, a)| a.chars().iter().copied().eq(s.encode_utf16()))
    }

    /// Whether boxed `callee` is the pristine builtin of cell `idx`.
    pub(super) fn builtin_is(&mut self, callee: Value, idx: u32) -> Value {
        let c = self.i32c(self.h.builtin_cells_base + 8 * idx);
        let bits = self.load_i64(c, 0);
        self.bin(Operator::I64Eq, callee, bits, Type::I32)
    }

    /// The pristine `RegExp.prototype.exec(s)` / `.test(s)` decided by the
    /// runtime's leaf (a non-global, non-sticky regexp with an AOT matcher:
    /// no frame, no GC, the heap untouched), taking `ok_clean` with the
    /// result; else `other` (which also builds a successful exec's result).
    pub(super) fn regexp_arm(&mut self, inst: mir::Inst, ops: &[Value], other: Block) -> R<()> {
        let (callee, this, s) = (ops[0], ops[1], ops[2]);
        let is_exec = self.builtin_is(callee, BC_REGEXP_EXEC);
        let is_test = self.builtin_is(callee, BC_REGEXP_TEST);
        let either = self.bin(Operator::I32Or, is_exec, is_test, Type::I32);
        self.check(either, other);
        let leaf = self.body.add_block();
        self.regexp_no_match(inst, this, s, is_test, leaf)?;
        self.cur = leaf;
        let r = self.call1(self.h.regexp_leaf, &[self.cx, this, s, is_test], Type::I64);
        let tag = self.tag_of(r);
        let miss = self.tag_is(tag, TAG_MAGIC as u32);
        let t = self.edge(inst, 0, &[r])?;
        self.cond_br(miss, Self::to(other), t);
        Ok(())
    }

    /// The no-match half of `re.exec(s)` / `re.test(s)` (the builtins),
    /// taking edge 0 with `null` / `false`: where `re` has the optimizable
    /// regexp shape the leaf armed (a plain RegExpObject of this realm's
    /// prototype, no own properties but `lastIndex`) and the prototype's
    /// fuse is intact, its `lastIndex` is a number (reading it runs
    /// nothing), its shared has a row (armed only for one neither global nor
    /// sticky, with AOT matchers), and `s` is a linear string, the matcher
    /// runs here, as the JIT's RegExp stubs call theirs. A failed match
    /// changes nothing (no statics, no `lastIndex`), so that answer is
    /// final; a match or a retry goes to `leaf`, which runs it again and
    /// builds what a match needs.
    pub(super) fn regexp_no_match(
        &mut self,
        inst: mir::Inst,
        re: Value,
        s: Value,
        is_test: Value,
        leaf: Block,
    ) -> R<()> {
        use crate::region_shape::{
            REGEX_LEAF_BT_ELEMS_OFF, REGEX_LEAF_BT_STACK_OFF, REGEX_LEAF_PAIRS_OFF,
            REGEX_LEAF_ROWS, REGEX_LEAF_ROWS_OFF, REGEX_LEAF_ROW_BYTES, STRLIT_REGEX_LEAF_OFF,
            STRLIT_REG_EXP_FUSE_ADDR_OFF, STRLIT_REG_EXP_SHAPE_OFF,
        };
        use crate::wasm::mir::abi::{REGEXP_LAST_INDEX_OFFSET, REGEXP_SHARED_OFFSET};
        let rt = self.tag_of(re);
        let st = self.tag_of(s);
        let ro = self.tag_is(rt, TAG_OBJECT as u32);
        let ss = self.tag_is(st, TAG_STRING as u32);
        let both = self.bin(Operator::I32And, ro, ss, Type::I32);
        self.check(both, leaf);
        let obj = self.un(Operator::I32WrapI64, re, Type::I32);
        let shape = self.load_i32(obj, SHAPE_OFFSET);
        let wsh = self.i32c(self.h.strlit_slot + STRLIT_REG_EXP_SHAPE_OFF);
        let armed = self.load_i32(wsh, 0);
        let same = self.bin(Operator::I32Eq, shape, armed, Type::I32);
        self.check(same, leaf);
        let wfuse = self.i32c(self.h.strlit_slot + STRLIT_REG_EXP_FUSE_ADDR_OFF);
        let fuse_at = self.load_i32(wfuse, 0);
        let fuse = self.load_i32(fuse_at, 0);
        let intact = self.un(Operator::I32Eqz, fuse, Type::I32);
        self.check(intact, leaf);
        let li = self.load_i64(obj, REGEXP_LAST_INDEX_OFFSET);
        let lt = self.tag_of(li);
        let k = self.i32c(TAG_INT32 as u32);
        let num = self.bin(Operator::I32LeU, lt, k, Type::I32);
        self.check(num, leaf);
        let shared = self.load_i32(obj, REGEXP_SHARED_OFFSET);
        let wblk = self.i32c(self.h.strlit_slot + STRLIT_REGEX_LEAF_OFF);
        let blk = self.load_i32(wblk, 0);
        let four = self.i32c(4);
        let h = self.bin(Operator::I32ShrU, shared, four, Type::I32);
        let mask = self.i32c(REGEX_LEAF_ROWS - 1);
        let h = self.bin(Operator::I32And, h, mask, Type::I32);
        let rb = self.i32c(REGEX_LEAF_ROW_BYTES);
        let off = self.bin(Operator::I32Mul, h, rb, Type::I32);
        let rows = self.add_off(blk, REGEX_LEAF_ROWS_OFF);
        let row = self.bin(Operator::I32Add, rows, off, Type::I32);
        let rshared = self.load_i32(row, 0);
        let hit = self.bin(Operator::I32Eq, rshared, shared, Type::I32);
        let z = self.i32c(0);
        let nz = self.bin(Operator::I32Ne, shared, z, Type::I32);
        let hit = self.bin(Operator::I32And, hit, nz, Type::I32);
        self.check(hit, leaf);
        // The subject: linear; its encoding picks the matcher.
        let p = self.un(Operator::I32WrapI64, s, Type::I32);
        let flags = self.load_i32(p, STRING_FLAGS_OFFSET);
        let lb = self.i32c(STRING_LINEAR_BIT);
        let lin = self.bin(Operator::I32And, flags, lb, Type::I32);
        self.check(lin, leaf);
        let l1b = self.i32c(STRING_LATIN1_CHARS_BIT);
        let latin1 = self.bin(Operator::I32And, flags, l1b, Type::I32);
        let l1 = self.load_i32(row, 4);
        let tb = self.load_i32(row, 8);
        let fidx = self.select(Type::I32, l1, tb, latin1);
        self.check(fidx, leaf);
        let len = self.load_i32(p, STRING_LENGTH_OFFSET);
        let ib = self.i32c(STRING_INLINE_CHARS_BIT);
        let inline = self.bin(Operator::I32And, flags, ib, Type::I32);
        let heap = self.load_i32(p, STRING_CHARS_OFFSET);
        let here = self.add_off(p, STRING_CHARS_OFFSET);
        let chars = self.select(Type::I32, here, heap, inline);
        let start = self.i32c(0);
        let pairs = self.add_off(blk, REGEX_LEAF_PAIRS_OFF);
        let bt = self.load_i32(blk, REGEX_LEAF_BT_STACK_OFF);
        let bte = self.load_i32(blk, REGEX_LEAF_BT_ELEMS_OFF);
        let args = self
            .body
            .arg_pool
            .from_iter([chars, len, start, pairs, bt, bte, fidx].into_iter());
        let tys = self.body.type_pool.from_iter([Type::I32].into_iter());
        let status = self.push_val(ValueDef::Operator(
            Operator::CallIndirect {
                sig_index: self.h.regex_matcher_sig,
                table_index: self.h.indirect_table,
            },
            args,
            tys,
        ));
        // kRegexMatcherFailure (0): no match.
        let none = self.un(Operator::I32Eqz, status, Type::I32);
        let done = self.body.add_block();
        self.cond_br(none, Self::to(done), Self::to(leaf));
        self.cur = done;
        let f = self.i64c(TAG_BOOLEAN << 32);
        let n = self.i64c(TAG_NULL << 32);
        let r = self.select(Type::I64, f, n, is_test);
        let t = self.edge(inst, 0, &[r])?;
        self.terminate(Terminator::Br { target: t });
        Ok(())
    }

    /// `guard.script`: the callee is a function of `sid`'s script, which
    /// is compiled (§5.5): its class is a function class, its script slot
    /// is that script (a native's slot never is), and the script has a
    /// table index. A lone guard (one likely callee) first probes a value
    /// cell of its own: a hit is one compare of the
    /// callee against the row, with no load through it. The full test fills
    /// the row as `night_call_classify` does (tenured callees only; a
    /// second callee poisons it). In a chain over several targets the
    /// function test is made once, by the first guard.
    pub(super) fn guard_script(
        &mut self,
        inst: mir::Inst,
        d: &mir::func::InstData,
        a: &[Value],
        sid: crate::ids::ScriptId,
    ) -> R<()> {
        let addr = *self
            .mm
            .script_addrs
            .get(&sid)
            .ok_or("lowering: guard.script without the script's address")?;
        let callee = d.args[0];
        let fail_m = d.succs[1].block;
        let chains = self.f.terminator(fail_m).is_some_and(|t| {
            matches!(self.f.insts[t].op, Opcode::GuardScript(_))
                && self.f.insts[t].args[0] == callee
        });
        let memo = self
            .script_memo
            .get(&self.cur_mblock)
            .filter(|(v, _)| *v == callee)
            .map(|&(_, f)| f);
        let cell = (memo.is_none() && !chains).then(|| {
            // This guard's own row: it learns only a callee that passed
            // the whole test below (a function of the script, which is
            // compiled, as it stays), else holds 0 (empty, or a GC's
            // zeroing) or the poison 1. So a hit is the whole test: the
            // row's low word, a learned callee's pointer, is the callee.
            let cell = self.i32c(CALL_CELL_ADDR_PLACEHOLDER);
            let idx = self.atoms.next_call_cell();
            self.call_cell_patches.push((cell, idx + 1));
            let cached = self.load_i32(cell, 0);
            let ok = self.bin(Operator::I32Eq, a[0], cached, Type::I32);
            let slow = self.body.add_block();
            let t = self.edge(inst, 0, &[a[0]])?;
            self.cond_br(ok, t, Self::to(slow));
            self.cur = slow;
            let boxed = self.box_tagged(TAG_OBJECT, a[0]);
            Ok::<_, String>((cell, boxed))
        });
        let cell = cell.transpose()?;
        let is_function = match memo {
            Some(f) => f,
            None => {
                let shape = self.load_i32(a[0], SHAPE_OFFSET);
                let base = self.load_i32(shape, SHAPE_BASESHAPE_OFFSET);
                let clasp = self.load_i32(base, BASESHAPE_CLASP_OFFSET);
                let slot = self.i32c(self.h.fn_class_slot);
                let fn_class = self.load_i32(slot, 0);
                let ext_class = self.load_i32(slot, 4);
                let is_fn = self.bin(Operator::I32Eq, clasp, fn_class, Type::I32);
                let is_ext = self.bin(Operator::I32Eq, clasp, ext_class, Type::I32);
                self.bin(Operator::I32Or, is_fn, is_ext, Type::I32)
            }
        };
        if chains && self.preds.get(&fail_m).is_some_and(|p| p.len() == 1) {
            self.script_memo.insert(fail_m, (callee, is_function));
        }
        let (fun_b, fail_b) = (self.body.add_block(), self.body.add_block());
        self.cond_br(is_function, Self::to(fun_b), Self::to(fail_b));
        self.cur = fun_b;
        let script = self.load_i32(a[0], FUNC_SCRIPT_SLOT_OFFSET);
        let want = self.i32c(addr);
        let same = self.bin(Operator::I32Eq, script, want, Type::I32);
        let idx_addr = self.i32c(addr);
        let idx = self.load_i32(idx_addr, BASESCRIPT_NIGHTFUNCINDEX_OFFSET);
        let z = self.i32c(0);
        let compiled = self.bin(Operator::I32Ne, idx, z, Type::I32);
        let ok = self.bin(Operator::I32And, same, compiled, Type::I32);
        let e = self.edge(inst, 1, &[])?;
        match cell {
            None => {
                let t = self.edge(inst, 0, &[a[0]])?;
                self.cond_br(ok, t, e);
            }
            Some((cell, boxed)) => {
                // The row's fill (`night_call_classify`'s): an empty row
                // learns this callee (a nursery one's store goes to the
                // trash row), a populated one is poisoned.
                let fill = self.body.add_block();
                self.cond_br(ok, Self::to(fill), e);
                self.cur = fill;
                let trash = self.i32c(CALL_CELL_ADDR_PLACEHOLDER);
                self.call_cell_patches.push((trash, 0));
                let cached = self.load_i64(cell, 0);
                let z64 = self.i64c(0);
                let empty = self.bin(Operator::I64Eq, cached, z64, Type::I32);
                let mask = self.i32c(NOT_CHUNK_MASK);
                let chunk = self.bin(Operator::I32And, a[0], mask, Type::I32);
                let sb = self.load_i32(chunk, CHUNK_STORE_BUFFER_OFFSET);
                let dst = self.select(Type::I32, trash, cell, sb);
                let (learn, poison) = (self.body.add_block(), self.body.add_block());
                self.cond_br(empty, Self::to(learn), Self::to(poison));
                self.cur = learn;
                self.store_i64(dst, 0, boxed);
                let t = self.edge(inst, 0, &[a[0]])?;
                self.terminate(Terminator::Br { target: t });
                self.cur = poison;
                let one = self.i64c(1);
                self.store_i64(cell, 0, one);
                let t = self.edge(inst, 0, &[a[0]])?;
                self.terminate(Terminator::Br { target: t });
            }
        }
        self.cur = fail_b;
        let e = self.edge(inst, 1, &[])?;
        self.terminate(Terminator::Br { target: e });
        Ok(())
    }

    /// `night_call_classify` of boxed `callee`: its funcref-table index (0
    /// when not compiled or not a scripted function) and its `JSScript*`.
    pub(super) fn classify(&mut self, callee: Value) -> (Value, Value) {
        let (f, s, _) = self.classify_native(callee);
        (f, s)
    }

    /// `classify`, and whether the callee is a native function (known
    /// only on a cell miss: the cell holds scripted callees).
    pub(super) fn classify_native(&mut self, callee: Value) -> (Value, Value, Value) {
        // The site's value cell: the steady
        // state is one callee repeating, and a hit is the whole classify.
        // The fill caches tenured functions only, and a major GC zeroes
        // the region; a zero row never false-hits (it is the double +0).
        let cell = self.i32c(CALL_CELL_ADDR_PLACEHOLDER);
        let idx = self.atoms.next_call_cell();
        self.call_cell_patches.push((cell, idx + 1));
        let trash = self.i32c(CALL_CELL_ADDR_PLACEHOLDER);
        self.call_cell_patches.push((trash, 0));
        let done = self.body.add_block();
        let fp = self.body.add_blockparam(done, Type::I32);
        let sp = self.body.add_blockparam(done, Type::I32);
        let np = self.body.add_blockparam(done, Type::I32);
        let cached = self.load_i64(cell, 0);
        let f = self.load_i32(cell, CALL_CELL_FUNCIDX);
        let sc = self.load_i32(cell, CALL_CELL_SCRIPT);
        let hit = self.bin(Operator::I64Eq, callee, cached, Type::I32);
        let miss = self.body.add_block();
        let z = self.i32c(0);
        self.cond_br(
            hit,
            BlockTarget {
                block: done,
                args: vec![f, sc, z],
            },
            Self::to(miss),
        );
        self.cur = miss;
        let args = self
            .body
            .arg_pool
            .from_iter([callee, cell, trash].into_iter());
        let tys = self
            .body
            .type_pool
            .from_iter([Type::I32, Type::I32, Type::I32].into_iter());
        let cls = self.push_val(ValueDef::Operator(
            Operator::Call {
                function_index: self.h.call_classify,
            },
            args,
            tys,
        ));
        let funcidx = self.push_val(ValueDef::PickOutput(cls, 0, Type::I32));
        let script = self.push_val(ValueDef::PickOutput(cls, 1, Type::I32));
        let native = self.push_val(ValueDef::PickOutput(cls, 2, Type::I32));
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: done,
                args: vec![funcidx, script, native],
            },
        });
        self.cur = done;
        (fp, sp, np)
    }
}
