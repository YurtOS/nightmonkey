//! construct: methods of `Lower`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    /// Whether `headroom` bytes from `top` fit the value stack: the stack is
    /// a region aligned to its size (`VALUE_STACK_LOG2`), so they do iff
    /// their last byte has the frame base's high bits. No load: each
    /// context's stack is its own region, wherever it is.
    pub(super) fn stack_fits(&mut self, top: Value, headroom: u32) -> Value {
        let last = self.add_off(top, headroom - 1);
        let x = self.bin(Operator::I32Xor, last, self.vp, Type::I32);
        let size = self.i32c(1 << crate::region_shape::VALUE_STACK_LOG2);
        self.bin(Operator::I32LtU, x, size, Type::I32)
    }

    /// The construct `this` for a direct construct:
    /// with the site's cell describing the callee
    /// (its shape, under the live IC generation) and its live `.prototype`
    /// still the cached one, a nursery bump of the cached empty `this`;
    /// else `create_this`, which also fills the cell. Leaves `this` at
    /// `top` and returns the ok flag (1 on the bump path).
    pub(super) fn construct_this(
        &mut self,
        top: Value,
        callee: Value,
        new_target: Value,
        nslots: u32,
        word: u32,
    ) -> Value {
        self.construct_this_with(top, callee, new_target, None, nslots, word)
    }

    /// `construct_this`, or with `proto` (`new_this`: the callee is its own
    /// new.target and its `prototype` is read) the cell's arm comparing it
    /// with the cell's prototype, and the `new_this` helper.
    pub(super) fn construct_this_with(
        &mut self,
        top: Value,
        callee: Value,
        new_target: Value,
        proto: Option<Value>,
        nslots: u32,
        word: u32,
    ) -> Value {
        let done = self.body.add_block();
        let ok_p = self.body.add_blockparam(done, Type::I32);
        let (this_v, slow, cell) = self.construct_this_inline(callee, proto, word);
        self.store_i64(top, 0, this_v);
        let one = self.i32c(1);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: done,
                args: vec![one],
            },
        });
        self.cur = slow;
        let made = self.construct_this_call(top, callee, new_target, proto, nslots, word, cell);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: done,
                args: vec![made],
            },
        });
        self.cur = done;
        ok_p
    }

    /// `construct_this_with`'s helper call, in its slow block: the
    /// object from the runtime (which also fills the site's cell), at
    /// `top`; the status.
    pub(super) fn construct_this_call(
        &mut self,
        top: Value,
        callee: Value,
        new_target: Value,
        proto: Option<Value>,
        nslots: u32,
        word: u32,
        cell: Value,
    ) -> Value {
        let (nv, wv) = (self.i32c(nslots), self.i32c(word));
        match proto {
            Some(p) => self.call1(
                self.h.new_this,
                &[self.cx, top, callee, p, nv, cell, wv],
                Type::I32,
            ),
            None => self.call1(
                self.h.create_this,
                &[self.cx, top, callee, new_target, nv, cell, wv],
                Type::I32,
            ),
        }
    }

    /// Bump `total` bytes off the nursery, or branch to `slow` where it has
    /// no room; returns the new cell's start (where its nursery header
    /// goes).
    pub(super) fn nursery_bump(&mut self, total: Value, slow: Block) -> Value {
        let posp_slot = self.i32c(self.h.nursery_pos_slot);
        let posp = self.load_i32(posp_slot, 0);
        let pos = self.load_i32(posp, 0);
        let newpos = self.bin(Operator::I32Add, pos, total, Type::I32);
        let endp_slot = self.i32c(self.h.nursery_end_slot);
        let endp = self.load_i32(endp_slot, 0);
        let end = self.load_i32(endp, 0);
        let fits = self.bin(Operator::I32LeU, newpos, end, Type::I32);
        self.check(fits, slow);
        self.store_i32(posp, 0, newpos);
        pos
    }

    /// Whether row `cell` is armed (its word at `armed_off` nonzero) under
    /// the live property-IC generation (at `gen_off`); else `slow`.
    pub(super) fn check_row(
        &mut self,
        cell: Value,
        armed_off: u32,
        gen_off: u32,
        slow: Block,
    ) -> Value {
        let armed = self.load_i32(cell, armed_off);
        let z = self.i32c(0);
        let filled = self.bin(Operator::I32Ne, armed, z, Type::I32);
        let cgen = self.load_i32(cell, gen_off);
        let gen_addr = self.i32c(self.h.prop_ic_gen_base);
        let live_gen = self.load_i32(gen_addr, 0);
        let g_ok = self.bin(Operator::I32Eq, cgen, live_gen, Type::I32);
        let hit = self.bin(Operator::I32And, filled, g_ok, Type::I32);
        self.check(hit, slow);
        armed
    }

    /// Copy `nbytes` (a multiple of 8, at least 8) from `src` to `dst`.
    pub(super) fn copy_words(&mut self, dst: Value, src: Value, nbytes: Value) {
        let (lp, body, done) = (
            self.body.add_block(),
            self.body.add_block(),
            self.body.add_block(),
        );
        let off = self.body.add_blockparam(lp, Type::I32);
        let z = self.i32c(0);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: lp,
                args: vec![z],
            },
        });
        self.cur = lp;
        let more = self.bin(Operator::I32LtU, off, nbytes, Type::I32);
        self.cond_br(more, Self::to(body), Self::to(done));
        self.cur = body;
        let s = self.bin(Operator::I32Add, src, off, Type::I32);
        let w = self.load_i64(s, 0);
        let d = self.bin(Operator::I32Add, dst, off, Type::I32);
        self.store_i64(d, 0, w);
        let eight = self.i32c(8);
        let next = self.bin(Operator::I32Add, off, eight, Type::I32);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: lp,
                args: vec![next],
            },
        });
        self.cur = done;
    }

    /// `Lambda` inline from the site's row (`NightLambdaCell`): a nursery
    /// copy of the canonical function with environment `env` (boxed).
    /// Returns the boxed closure; `slow` where the row is not armed under
    /// the live generation or the nursery has no room.
    pub(super) fn lambda_inline(&mut self, cell: Value, env: Value, slow: Block) -> Value {
        let tmpl = self.check_row(cell, LAMBDA_CELL_TEMPLATE, LAMBDA_CELL_GEN, slow);
        let total = self.load_i32(cell, 4);
        let pos = self.nursery_bump(total, slow);
        let hdr = self.load_i32(cell, 16);
        self.store_i32(pos, 0, hdr);
        let hb = self.i32c(NURSERY_HEADER_BYTES);
        let obj = self.bin(Operator::I32Add, pos, hb, Type::I32);
        let nbytes = self.bin(Operator::I32Sub, total, hb, Type::I32);
        self.copy_words(obj, tmpl, nbytes);
        self.store_i64(obj, FUNC_ENV_SLOT_OFFSET, env);
        let payload = self.un(Operator::I64ExtendI32U, obj, Type::I64);
        let tag = self.i64c(TAG_OBJECT << 32);
        self.bin(Operator::I64Or, payload, tag, Type::I64)
    }

    /// The prologue's CallObject inline from the script's row
    /// (`NightEnvCell`): enclosing environment the callee's, callee, then
    /// the bindings undefined. Returns it boxed; `slow` where the row is
    /// not armed under the live generation or the nursery has no room.
    pub(super) fn env_inline(&mut self, cell: Value, slow: Block) -> Value {
        let span = self.check_row(cell, ENV_CELL_ARMED, ENV_CELL_GEN, slow);
        let total = self.load_i32(cell, 4);
        let pos = self.nursery_bump(total, slow);
        let hdr = self.load_i32(cell, 16);
        self.store_i32(pos, 0, hdr);
        let hb = self.i32c(NURSERY_HEADER_BYTES);
        let obj = self.bin(Operator::I32Add, pos, hb, Type::I32);
        let shape = self.load_i32(cell, 0);
        self.store_i32(obj, SHAPE_OFFSET, shape);
        let word = self.load_i32(cell, ENV_CELL_CLASS_WORD);
        self.store_i32(obj, OBJ_CLASS_IDX_OFFSET, word);
        let slotsw = self.load_i32(cell, 8);
        self.store_i32(obj, NATIVE_SLOTS_OFFSET, slotsw);
        let elemsw = self.load_i32(cell, 12);
        self.store_i32(obj, OBJ_ELEMENTS_OFFSET, elemsw);
        let callee = self.load_i64(self.sp, FrameLayout::CALLEE);
        let f = self.un(Operator::I32WrapI64, callee, Type::I32);
        let enclosing = self.load_i64(f, FUNC_ENV_SLOT_OFFSET);
        self.store_i64(obj, CALLOBJ_ENCLOSING_OFFSET, enclosing);
        self.store_i64(obj, CALLOBJ_CALLEE_OFFSET, callee);
        // The bindings, slots 2 up to the span: undefined.
        let (lp, body, done) = (
            self.body.add_block(),
            self.body.add_block(),
            self.body.add_block(),
        );
        let k = self.body.add_blockparam(lp, Type::I32);
        let two = self.i32c(2);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: lp,
                args: vec![two],
            },
        });
        self.cur = lp;
        let more = self.bin(Operator::I32LtU, k, span, Type::I32);
        self.cond_br(more, Self::to(body), Self::to(done));
        self.cur = body;
        let three = self.i32c(3);
        let off = self.bin(Operator::I32Shl, k, three, Type::I32);
        let a = self.bin(Operator::I32Add, obj, off, Type::I32);
        let undef = self.i64c(UNDEF);
        self.store_i64(a, FIXED_SLOTS_BASE, undef);
        let one = self.i32c(1);
        let next = self.bin(Operator::I32Add, k, one, Type::I32);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: lp,
                args: vec![next],
            },
        });
        self.cur = done;
        let payload = self.un(Operator::I64ExtendI32U, obj, Type::I64);
        let tag = self.i64c(TAG_OBJECT << 32);
        self.bin(Operator::I64Or, payload, tag, Type::I64)
    }

    /// `construct_this_with`'s inline half: with the site's cell
    /// describing the callee (and its `.prototype`), bump-allocate the
    /// object in the nursery and fill its header. Leaves the current
    /// block at the success point with the boxed object; returns it, the
    /// (empty) slow block every miss branches to, and the cell. Runs no
    /// code and cannot GC: the slow block's helper call can.
    pub(super) fn construct_this_inline(
        &mut self,
        callee: Value,
        proto: Option<Value>,
        word: u32,
    ) -> (Value, Block, Value) {
        let cell = self.i32c(CONSTRUCT_CELL_ADDR_PLACEHOLDER);
        let idx = self.atoms.next_construct_cell();
        self.construct_cell_patches.push((cell, idx + 1));
        let slow = self.body.add_block();
        let cptr = self.un(Operator::I32WrapI64, callee, Type::I32);
        let ashape = self.load_i32(cell, 0);
        let cshape = self.load_i32(cell, CONSTRUCT_CELL_CTORSHAPE);
        let cgen = self.load_i32(cell, CONSTRUCT_CELL_GEN);
        let live_shape = self.load_i32(cptr, SHAPE_OFFSET);
        let gen_addr = self.i32c(self.h.prop_ic_gen_base);
        let live_gen = self.load_i32(gen_addr, 0);
        let z = self.i32c(0);
        let filled = self.bin(Operator::I32Ne, ashape, z, Type::I32);
        let s_ok = self.bin(Operator::I32Eq, cshape, live_shape, Type::I32);
        let g_ok = self.bin(Operator::I32Eq, cgen, live_gen, Type::I32);
        // With the prototype given, the cell's shape is the object's for
        // it, whoever the callee's shape is.
        let hit = if proto.is_some() {
            filled
        } else {
            self.bin(Operator::I32And, filled, s_ok, Type::I32)
        };
        let hit = self.bin(Operator::I32And, hit, g_ok, Type::I32);
        self.check(hit, slow);
        let pval = match proto {
            Some(p) => p,
            None => {
                // A reassigned `.prototype` keeps the callee's shape.
                let enc = self.load_i32(cell, CONSTRUCT_CELL_PROTOSLOTENC);
                let one = self.i32c(1);
                let dynamic = self.bin(Operator::I32And, enc, one, Type::I32);
                let not1 = self.i32c(!1);
                let off = self.bin(Operator::I32And, enc, not1, Type::I32);
                let slots = self.load_i32(cptr, NATIVE_SLOTS_OFFSET);
                let sb = self.select(Type::I32, slots, cptr, dynamic);
                let addr = self.bin(Operator::I32Add, sb, off, Type::I32);
                self.load_i64(addr, 0)
            }
        };
        let pt = self.tag_of(pval);
        let p_obj = self.tag_is(pt, TAG_OBJECT as u32);
        let pptr = self.un(Operator::I32WrapI64, pval, Type::I32);
        let cproto = self.load_i32(cell, CONSTRUCT_CELL_PROTOPTR);
        let p_eq = self.bin(Operator::I32Eq, pptr, cproto, Type::I32);
        let p_ok = self.bin(Operator::I32And, p_obj, p_eq, Type::I32);
        self.check(p_ok, slow);
        // Room in the nursery, then bump and fill the header.
        let total = self.load_i32(cell, 4);
        let pos = self.nursery_bump(total, slow);
        let hdr = self.load_i32(cell, 16);
        self.store_i32(pos, 0, hdr);
        let hb = self.i32c(NURSERY_HEADER_BYTES);
        let obj = self.bin(Operator::I32Add, pos, hb, Type::I32);
        self.store_i32(obj, SHAPE_OFFSET, ashape);
        let wv = self.i32c(word);
        self.store_i32(obj, OBJ_CLASS_IDX_OFFSET, wv);
        let slotsw = self.load_i32(cell, 8);
        self.store_i32(obj, NATIVE_SLOTS_OFFSET, slotsw);
        let elemsw = self.load_i32(cell, 12);
        self.store_i32(obj, OBJ_ELEMENTS_OFFSET, elemsw);
        let payload = self.un(Operator::I64ExtendI32U, obj, Type::I64);
        let tag = self.i64c(TAG_OBJECT << 32);
        let this_v = self.bin(Operator::I64Or, payload, tag, Type::I64);
        (this_v, slow, cell)
    }

    /// `new_this.init`'s inline arm: with the site's construct cell's final
    /// part armed under the live IC generation, `proto` its prototype, and
    /// the prototype chain's shapes as recorded (no setter or read-only
    /// property of the fields' names added since), a nursery bump with the
    /// final shape, the class word (RANGES cleared, as the adds leave it),
    /// and the fields `xs` in their fixed slots (no barriers: the object is
    /// in the nursery, and new). Returns the object, the miss block and the
    /// cell.
    pub(super) fn construct_this_final(
        &mut self,
        proto: Value,
        word: u32,
        xs: &[Value],
    ) -> (Value, Block, Value) {
        let cell = self.i32c(CONSTRUCT_CELL_ADDR_PLACEHOLDER);
        let idx = self.atoms.next_construct_cell();
        self.construct_cell_patches.push((cell, idx + 1));
        let slow = self.body.add_block();
        let fshape = self.load_i32(cell, CONSTRUCT_CELL_FINALSHAPE);
        let cgen = self.load_i32(cell, CONSTRUCT_CELL_GEN);
        let gen_addr = self.i32c(self.h.prop_ic_gen_base);
        let live_gen = self.load_i32(gen_addr, 0);
        let z = self.i32c(0);
        let filled = self.bin(Operator::I32Ne, fshape, z, Type::I32);
        let g_ok = self.bin(Operator::I32Eq, cgen, live_gen, Type::I32);
        let hit = self.bin(Operator::I32And, filled, g_ok, Type::I32);
        self.check(hit, slow);
        let pt = self.tag_of(proto);
        let p_obj = self.tag_is(pt, TAG_OBJECT as u32);
        let pptr = self.un(Operator::I32WrapI64, proto, Type::I32);
        let cproto = self.load_i32(cell, CONSTRUCT_CELL_PROTOPTR);
        let p_eq = self.bin(Operator::I32Eq, pptr, cproto, Type::I32);
        let p_ok = self.bin(Operator::I32And, p_obj, p_eq, Type::I32);
        self.check(p_ok, slow);
        let ps = self.load_i32(pptr, SHAPE_OFFSET);
        let cps = self.load_i32(cell, CONSTRUCT_CELL_PROTOSHAPE);
        let ps_ok = self.bin(Operator::I32Eq, ps, cps, Type::I32);
        self.check(ps_ok, slow);
        let p2 = self.load_i32(cell, CONSTRUCT_CELL_PROTO2PTR);
        let (has2, done2) = (self.body.add_block(), self.body.add_block());
        self.cond_br(p2, Self::to(has2), Self::to(done2));
        self.cur = has2;
        let p2s = self.load_i32(p2, SHAPE_OFFSET);
        let cp2s = self.load_i32(cell, CONSTRUCT_CELL_PROTO2SHAPE);
        let p2_ok = self.bin(Operator::I32Eq, p2s, cp2s, Type::I32);
        self.check(p2_ok, slow);
        self.terminate(Terminator::Br {
            target: Self::to(done2),
        });
        self.cur = done2;
        let total = self.load_i32(cell, 4);
        let pos = self.nursery_bump(total, slow);
        let hdr = self.load_i32(cell, 16);
        self.store_i32(pos, 0, hdr);
        let hb = self.i32c(NURSERY_HEADER_BYTES);
        let obj = self.bin(Operator::I32Add, pos, hb, Type::I32);
        self.store_i32(obj, SHAPE_OFFSET, fshape);
        // The fields' set with them.
        let set = mir::types::FieldSet::prefix(u32::try_from(xs.len()).unwrap());
        let wv = self.i32c((word & !CLASS_WORD_RANGES) | set.word_bits());
        self.store_i32(obj, OBJ_CLASS_IDX_OFFSET, wv);
        let slotsw = self.load_i32(cell, 8);
        self.store_i32(obj, NATIVE_SLOTS_OFFSET, slotsw);
        let elemsw = self.load_i32(cell, 12);
        self.store_i32(obj, OBJ_ELEMENTS_OFFSET, elemsw);
        for (i, &x) in xs.iter().enumerate() {
            self.store_i64(obj, FIXED_SLOTS_BASE + 8 * u32::try_from(i).unwrap(), x);
        }
        (obj, slow, cell)
    }

    /// An object or array literal (`array_len`) from the site's
    /// inline-alloc cell: with the cell filled
    /// and room in the nursery, bump and write the header the cell holds
    /// (an array's elements inline, empty), taking `ok_clean`. Returns the
    /// cell, for the helper that fills it, with `cur` on the miss.
    pub(super) fn alloc_inline(&mut self, inst: mir::Inst, array_len: Option<u32>) -> R<Value> {
        let cell = self.i32c(ALLOC_CELL_ADDR_PLACEHOLDER);
        let idx = self.atoms.next_alloc_cell();
        self.alloc_cell_patches.push((cell, idx));
        let slow = self.body.add_block();
        let shape = self.load_i32(cell, 0);
        self.check(shape, slow);
        let posp_slot = self.i32c(self.h.nursery_pos_slot);
        let posp = self.load_i32(posp_slot, 0);
        let pos = self.load_i32(posp, 0);
        let total = self.load_i32(cell, 4);
        let newpos = self.bin(Operator::I32Add, pos, total, Type::I32);
        let endp_slot = self.i32c(self.h.nursery_end_slot);
        let endp = self.load_i32(endp_slot, 0);
        let end = self.load_i32(endp, 0);
        let fits = self.bin(Operator::I32LeU, newpos, end, Type::I32);
        self.check(fits, slow);
        self.store_i32(posp, 0, newpos);
        let hdr = self.load_i32(cell, 16);
        self.store_i32(pos, 0, hdr);
        let hb = self.i32c(NURSERY_HEADER_BYTES);
        let obj = self.bin(Operator::I32Add, pos, hb, Type::I32);
        self.store_i32(obj, SHAPE_OFFSET, shape);
        let z = self.i32c(0);
        self.store_i32(obj, OBJ_CLASS_IDX_OFFSET, z);
        let slots = self.load_i32(cell, 8);
        self.store_i32(obj, NATIVE_SLOTS_OFFSET, slots);
        match array_len {
            None => {
                let elems = self.load_i32(cell, 12);
                self.store_i32(obj, OBJ_ELEMENTS_OFFSET, elems);
            }
            Some(_) => {
                let eoff = self.load_i32(cell, 12);
                let elems = self.bin(Operator::I32Add, obj, eoff, Type::I32);
                self.store_i32(obj, OBJ_ELEMENTS_OFFSET, elems);
                let ehdr = self.elem_header_addr(elems, ELEMENTS_HEADER_BYTES);
                let flags = self.load_i32(cell, 20);
                self.store_i32(ehdr, 0, flags);
                self.store_i32(ehdr, 4, z);
                let cap = self.load_i32(cell, 24);
                self.store_i32(ehdr, 8, cap);
                let len = self.load_i32(cell, 28);
                self.store_i32(ehdr, 12, len);
            }
        }
        let r = self.box_tagged(TAG_OBJECT, obj);
        let t = self.edge(inst, 0, &[r])?;
        self.terminate(Terminator::Br { target: t });
        self.cur = slow;
        Ok(cell)
    }

    /// `InitProp` on a literal under construction:
    /// replay the site's add transition from its IC row (the helper fills
    /// it) while the literal has the row's old shape: store the value in
    /// the fresh slot (no pre-barrier: nothing was there), swap in the new
    /// shape, taking `ok_clean`. Literals carry no class word, so no stamp
    /// bit needs keeping. Returns the site's cache index, with `cur` on
    /// the miss.
    pub(super) fn init_prop_inline(&mut self, inst: mir::Inst, objv: Value, val: Value) -> R<u32> {
        let cache = self.atoms.next_prop_cache();
        let slow = self.body.add_block();
        let obj = self.un(Operator::I32WrapI64, objv, Type::I32);
        let shape = self.load_i32(obj, SHAPE_OFFSET);
        let row = self.ic_addr(cache * INLINE_IC_STRIDE + IC_TRANS_ROW_OFF);
        let old = self.load_i32(row, IC_TRANS_OLDSHAPE);
        let same = self.bin(Operator::I32Eq, old, shape, Type::I32);
        self.check(same, slow);
        let off = self.load_i32(row, IC_TRANS_SLOTOFF);
        self.check(off, slow);
        let addr = self.bin(Operator::I32Add, obj, off, Type::I32);
        self.store_i64(addr, 0, val);
        let new_s = self.load_i32(row, IC_TRANS_NEWSHAPE);
        self.store_i32(obj, SHAPE_OFFSET, new_s);
        let abs = self.load_i32(row, IC_TRANS_ABSSLOT);
        self.post_barrier(self.h.post_write_barrier, obj, abs, val);
        let t = self.edge(inst, 0, &[])?;
        self.terminate(Terminator::Br { target: t });
        self.cur = slow;
        Ok(cache)
    }
}
