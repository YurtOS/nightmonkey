//! globals: methods of `Lower`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    /// The inline arms of a read of syntactic global binding `bid`, each
    /// taking the op's `ok` edge:
    /// the binding's value-fuse cell while armed; else its cached slot row
    /// while the global's shape is the one the row was resolved against;
    /// else the resolve leaf (no GC) and the slot. Falls through to the
    /// generic helper when the binding is not cacheable (lexicals, TDZ).
    /// `check.binding` (§3): binding row `slot` resolved against the
    /// global object's live shape (writable too, for `write`), re-resolved
    /// by the leaf when it is not. The row then says where the slot is.
    pub(super) fn binding_ok(&mut self, slot: u32, write: bool) -> Value {
        let base = self.i32c(self.h.global_slots_base);
        let entry0 = self.load_i32(base, 8 * slot);
        let shape0 = self.load_i32(base, 8 * slot + 4);
        // Resolved (bit 0), and for a write writable (bit 2).
        let usable = |l: &mut Self, e: Value| {
            let m = l.i32c(if write { 5 } else { 1 });
            let r = l.bin(Operator::I32And, e, m, Type::I32);
            if !write {
                return r;
            }
            l.bin(Operator::I32Eq, r, m, Type::I32)
        };
        let u0 = usable(self, entry0);
        let realm = self.load_i32(self.cx, JSCONTEXT_REALM_OFFSET);
        let global = self.load_i32(realm, REALM_GLOBAL_OFFSET);
        let live = self.load_i32(global, SHAPE_OFFSET);
        let same = self.bin(Operator::I32Eq, shape0, live, Type::I32);
        let hit = self.bin(Operator::I32And, u0, same, Type::I32);
        let join = self.body.add_block();
        let ok = self.body.add_blockparam(join, Type::I32);
        let resolve_b = self.body.add_block();
        self.cond_br(
            hit,
            BlockTarget {
                block: join,
                args: vec![hit],
            },
            Self::to(resolve_b),
        );
        self.cur = resolve_b;
        let b = self.i32c(slot);
        let entry1 = self.call1(self.h.resolve_global_slot_guarded, &[self.cx, b], Type::I32);
        let u1 = usable(self, entry1);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: join,
                args: vec![u1],
            },
        });
        self.cur = join;
        ok
    }

    /// The address of binding row `slot`'s global slot, the row resolved
    /// (bit 1 of the entry selects the dynamic slots; `entry & !7` is the
    /// byte offset from that base, past the fixed-slot header when fixed).
    pub(super) fn binding_addr(&mut self, slot: u32) -> Value {
        self.binding_slot(slot).0
    }

    /// Binding row `slot`'s global slot, the binding checked: its address,
    /// the global object, the row's entry and whether the slot is dynamic
    /// (`binding_addr`). A major GC since the check zeroes the row (its
    /// shape word may dangle): resolved again first, to the same slot (the
    /// check's binding is a data property that cannot move).
    pub(super) fn binding_slot(&mut self, slot: u32) -> (Value, Value, Value, Value) {
        let base = self.i32c(self.h.global_slots_base);
        let entry0 = self.load_i32(base, 8 * slot);
        let one = self.i32c(1);
        let resolved = self.bin(Operator::I32And, entry0, one, Type::I32);
        let (resolve_b, join) = (self.body.add_block(), self.body.add_block());
        let entry = self.body.add_blockparam(join, Type::I32);
        self.cond_br(
            resolved,
            BlockTarget {
                block: join,
                args: vec![entry0],
            },
            Self::to(resolve_b),
        );
        self.cur = resolve_b;
        let bv = self.i32c(slot);
        let entry1 = self.call1(
            self.h.resolve_global_slot_guarded,
            &[self.cx, bv],
            Type::I32,
        );
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: join,
                args: vec![entry1],
            },
        });
        self.cur = join;
        let realm = self.load_i32(self.cx, JSCONTEXT_REALM_OFFSET);
        let global = self.load_i32(realm, REALM_GLOBAL_OFFSET);
        let one = self.i32c(1);
        let sh = self.bin(Operator::I32ShrU, entry, one, Type::I32);
        let dynamic = self.bin(Operator::I32And, sh, one, Type::I32);
        let m = self.i32c(!7);
        let idx8 = self.bin(Operator::I32And, entry, m, Type::I32);
        let z = self.i32c(0);
        let fb = self.i32c(FIXED_SLOTS_BASE);
        let add = self.select(Type::I32, z, fb, dynamic);
        let off = self.bin(Operator::I32Add, idx8, add, Type::I32);
        let slots = self.load_i32(global, NATIVE_SLOTS_OFFSET);
        let slot_base = self.select(Type::I32, slots, global, dynamic);
        let addr = self.bin(Operator::I32Add, slot_base, off, Type::I32);
        (addr, global, entry, dynamic)
    }

    /// `store_gname` (§3): the resolved, writable slot of binding row
    /// `slot` takes `val`, with the barriers and the binding's value fuse
    /// as the syntactic global store keeps them, and a
    /// fused literal's fuse (`gname_set_arms`' tail). `gc_free`: the value
    /// is no GC thing, by its type.
    pub(super) fn binding_store(
        &mut self,
        slot: u32,
        val: Value,
        gc_free: bool,
        fused: Option<crate::wasm::translate::FusedGname>,
    ) {
        let (addr, global, entry, dynamic) = self.binding_slot(slot);
        self.pre_barrier(addr, 0);
        self.store_i64(addr, 0, val);
        if !gc_free {
            // The generational post-barrier: the global object is tenured,
            // so a nursery GC thing is recorded, by the slot's index (its
            // shape's fixed slots first), which only that path computes.
            let cont = self.body.add_block();
            let tag = self.tag_of(val);
            let min = self.i32c(VAL_GCTHING_TAG_MIN);
            let is_gc = self.bin(Operator::I32GeU, tag, min, Type::I32);
            let gc = self.body.add_block();
            self.cond_br(is_gc, Self::to(gc), Self::to(cont));
            self.cur = gc;
            let mask = self.i32c(NOT_CHUNK_MASK);
            let cell = self.un(Operator::I32WrapI64, val, Type::I32);
            let chunk = self.bin(Operator::I32And, cell, mask, Type::I32);
            let sb = self.load_i32(chunk, CHUNK_STORE_BUFFER_OFFSET);
            let record = self.body.add_block();
            self.cond_br(sb, Self::to(record), Self::to(cont));
            self.cur = record;
            let three = self.i32c(3);
            let idx = self.bin(Operator::I32ShrU, entry, three, Type::I32);
            let live = self.load_i32(global, SHAPE_OFFSET);
            let flags = self.load_i32(live, SHAPE_IMMUTABLE_FLAGS_OFFSET);
            let fs = self.i32c(SHAPE_FIXED_SLOTS_SHIFT);
            let nf = self.bin(Operator::I32ShrU, flags, fs, Type::I32);
            let fm = self.i32c(SHAPE_FIXED_SLOTS_MASK_BITS);
            let nfixed = self.bin(Operator::I32And, nf, fm, Type::I32);
            let idx_plus = self.bin(Operator::I32Add, idx, nfixed, Type::I32);
            let abs = self.select(Type::I32, idx_plus, idx, dynamic);
            let owner = self.box_tagged(TAG_OBJECT, global);
            self.call(self.h.post_write_barrier, &[owner, abs, val], &[]);
            self.terminate(Terminator::Br {
                target: Self::to(cont),
            });
            self.cur = cont;
        }
        self.binding_fuses(slot, val, gc_free, fused);
    }

    /// A syntactic global store's bookkeeping, after the slot store: the
    /// binding's value fuse (`gGlobalVals[bid]`) and a fused literal's
    /// fuse.
    pub(super) fn binding_fuses(
        &mut self,
        bid: u32,
        val: Value,
        gc_free: bool,
        fused: Option<crate::wasm::translate::FusedGname>,
    ) {
        let one = self.i32c(1);
        let two = self.i32c(2);
        // The binding's value fuse (`gGlobalVals[bid]`): an armed cell whose
        // value changes mirrors a non-GC value in place, and otherwise is
        // unarmed with the re-arm left to the runtime.
        let vals = self.i32c(self.h.global_vals_base + 16 * bid);
        let fw = self.load_i32(vals, 8);
        let armed = self.fuse_armed(fw);
        let old = self.load_i64(vals, 0);
        let changed = self.bin(Operator::I64Ne, old, val, Type::I32);
        let blow = self.bin(Operator::I32And, armed, changed, Type::I32);
        let (blow_b, cont) = (self.body.add_block(), self.body.add_block());
        self.cond_br(blow, Self::to(blow_b), Self::to(cont));
        self.cur = blow_b;
        let (gc_b, plain_b) = (self.body.add_block(), self.body.add_block());
        if gc_free {
            self.terminate(Terminator::Br {
                target: Self::to(plain_b),
            });
        } else {
            let tag = self.tag_of(val);
            let min = self.i32c(VAL_GCTHING_TAG_MIN);
            let is_gc = self.bin(Operator::I32GeU, tag, min, Type::I32);
            self.cond_br(is_gc, Self::to(gc_b), Self::to(plain_b));
        }
        self.cur = plain_b;
        // Armed with the new value; no function, so not predicted.
        self.store_i64(vals, 0, val);
        self.store_i32(vals, 8, one);
        self.terminate(Terminator::Br {
            target: Self::to(cont),
        });
        self.cur = gc_b;
        let z = self.i32c(0);
        self.store_i32(vals, 8, z);
        let b = self.i32c(bid);
        self.call(self.h.binding_written, &[b], &[]);
        self.terminate(Terminator::Br {
            target: Self::to(cont),
        });
        self.cur = cont;
        // A fused literal's fuse: the literal arms it, anything else blows
        // it.
        if let Some(fg) = fused {
            let fa = self.i32c(fg.fuse_addr);
            let f = self.load_i32(fa, 0);
            let lit = self.i64c(fg.boxed);
            let neq = self.bin(Operator::I64Ne, val, lit, Type::I32);
            let z = self.i32c(0);
            let is_zero = self.bin(Operator::I32Eq, f, z, Type::I32);
            let armed = self.select(Type::I32, one, f, is_zero);
            let nf = self.select(Type::I32, two, armed, neq);
            self.store_i32(fa, 0, nf);
        }
    }

    /// Whether a binding's value-fuse word `fw` is armed: 1, or 3 (armed
    /// with its predicted function).
    pub(super) fn fuse_armed(&mut self, fw: Value) -> Value {
        let one = self.i32c(1);
        self.bin(Operator::I32And, fw, one, Type::I32)
    }

    pub(super) fn gname_fast_arms(&mut self, inst: mir::Inst, bid: u32) -> R<()> {
        let vals = self.i32c(self.h.global_vals_base + 16 * bid);
        let fw = self.load_i32(vals, 8);
        let one = self.i32c(1);
        let armed = self.fuse_armed(fw);
        let (hit_b, slots_b) = (self.body.add_block(), self.body.add_block());
        self.cond_br(armed, Self::to(hit_b), Self::to(slots_b));
        self.cur = hit_b;
        let v = self.load_i64(vals, 0);
        let t = self.edge(inst, 0, &[v])?;
        self.terminate(Terminator::Br { target: t });

        self.cur = slots_b;
        let base = self.i32c(self.h.global_slots_base);
        let entry0 = self.load_i32(base, 8 * bid);
        let shape0 = self.load_i32(base, 8 * bid + 4);
        let resolved0 = self.bin(Operator::I32And, entry0, one, Type::I32);
        let realm = self.load_i32(self.cx, JSCONTEXT_REALM_OFFSET);
        let global = self.load_i32(realm, REALM_GLOBAL_OFFSET);
        let live = self.load_i32(global, SHAPE_OFFSET);
        let same = self.bin(Operator::I32Eq, shape0, live, Type::I32);
        let hit = self.bin(Operator::I32And, resolved0, same, Type::I32);
        let use_b = self.body.add_block();
        let entry = self.body.add_blockparam(use_b, Type::I32);
        let resolve_b = self.body.add_block();
        self.cond_br(
            hit,
            BlockTarget {
                block: use_b,
                args: vec![entry0],
            },
            Self::to(resolve_b),
        );

        self.cur = resolve_b;
        let b = self.i32c(bid);
        let entry1 = self.call1(self.h.resolve_global_slot_guarded, &[self.cx, b], Type::I32);
        let resolved1 = self.bin(Operator::I32And, entry1, one, Type::I32);
        let slow = self.body.add_block();
        self.cond_br(
            resolved1,
            BlockTarget {
                block: use_b,
                args: vec![entry1],
            },
            Self::to(slow),
        );

        // The entry: bit 1 selects the dynamic slots, `entry & !7` is the
        // byte offset from that base (past the fixed-slot header when
        // fixed).
        self.cur = use_b;
        let sh = self.bin(Operator::I32ShrU, entry, one, Type::I32);
        let dynamic = self.bin(Operator::I32And, sh, one, Type::I32);
        let m = self.i32c(!7);
        let idx8 = self.bin(Operator::I32And, entry, m, Type::I32);
        let z = self.i32c(0);
        let fb = self.i32c(FIXED_SLOTS_BASE);
        let add = self.select(Type::I32, z, fb, dynamic);
        let off = self.bin(Operator::I32Add, idx8, add, Type::I32);
        let slots = self.load_i32(global, NATIVE_SLOTS_OFFSET);
        let slot_base = self.select(Type::I32, slots, global, dynamic);
        let addr = self.bin(Operator::I32Add, slot_base, off, Type::I32);
        let v = self.load_i64(addr, 0);
        let t = self.edge(inst, 0, &[v])?;
        self.terminate(Terminator::Br { target: t });
        self.cur = slow;
        Ok(())
    }

    /// The inline arm of binding syntactic global name `bid`:
    /// with the binding's slot row resolved
    /// against the global's live shape (or re-resolved by the leaf), the
    /// binding object is the global object, taking `ok_clean`. Falls
    /// through to the helper otherwise.
    pub(super) fn gname_bind_arms(&mut self, inst: mir::Inst, bid: u32) -> R<()> {
        let base = self.i32c(self.h.global_slots_base);
        let entry0 = self.load_i32(base, 8 * bid);
        let shape0 = self.load_i32(base, 8 * bid + 4);
        let one = self.i32c(1);
        let resolved0 = self.bin(Operator::I32And, entry0, one, Type::I32);
        let realm = self.load_i32(self.cx, JSCONTEXT_REALM_OFFSET);
        let global = self.load_i32(realm, REALM_GLOBAL_OFFSET);
        let live = self.load_i32(global, SHAPE_OFFSET);
        let same = self.bin(Operator::I32Eq, shape0, live, Type::I32);
        let hit = self.bin(Operator::I32And, resolved0, same, Type::I32);
        let (hit_b, resolve_b, slow) = (
            self.body.add_block(),
            self.body.add_block(),
            self.body.add_block(),
        );
        self.cond_br(hit, Self::to(hit_b), Self::to(resolve_b));
        self.cur = resolve_b;
        let b = self.i32c(bid);
        let entry1 = self.call1(self.h.resolve_global_slot_guarded, &[self.cx, b], Type::I32);
        let resolved1 = self.bin(Operator::I32And, entry1, one, Type::I32);
        self.cond_br(resolved1, Self::to(hit_b), Self::to(slow));
        self.cur = hit_b;
        let g = self.box_tagged(TAG_OBJECT, global);
        let t = self.edge(inst, 0, &[g])?;
        self.terminate(Terminator::Br { target: t });
        self.cur = slow;
        Ok(())
    }

    /// The inline arm of a write to syntactic global binding `bid`,
    /// taking `ok_clean`: with the
    /// binding's slot row resolved against the global's live shape (or
    /// re-resolved by the leaf) and writable, store `val` with barriers,
    /// then keep the binding's value fuse, the bind epoch and a fused
    /// literal's fuse as the generic store would. Falls through to the
    /// helper otherwise (accessors, lexicals, read-only, undeclared).
    pub(super) fn gname_set_arms(
        &mut self,
        inst: mir::Inst,
        bid: u32,
        val: Value,
        fused: Option<crate::wasm::translate::FusedGname>,
    ) -> R<()> {
        let base = self.i32c(self.h.global_slots_base);
        let entry0 = self.load_i32(base, 8 * bid);
        let shape0 = self.load_i32(base, 8 * bid + 4);
        let one = self.i32c(1);
        let two = self.i32c(2);
        let writable = |l: &mut Self, e: Value| {
            let r = l.bin(Operator::I32And, e, one, Type::I32);
            let sh = l.bin(Operator::I32ShrU, e, two, Type::I32);
            let w = l.bin(Operator::I32And, sh, one, Type::I32);
            l.bin(Operator::I32And, r, w, Type::I32)
        };
        let rw0 = writable(self, entry0);
        let realm = self.load_i32(self.cx, JSCONTEXT_REALM_OFFSET);
        let global = self.load_i32(realm, REALM_GLOBAL_OFFSET);
        let live = self.load_i32(global, SHAPE_OFFSET);
        let same = self.bin(Operator::I32Eq, shape0, live, Type::I32);
        let hit = self.bin(Operator::I32And, rw0, same, Type::I32);
        let use_b = self.body.add_block();
        let entry = self.body.add_blockparam(use_b, Type::I32);
        let resolve_b = self.body.add_block();
        self.cond_br(
            hit,
            BlockTarget {
                block: use_b,
                args: vec![entry0],
            },
            Self::to(resolve_b),
        );
        self.cur = resolve_b;
        let b = self.i32c(bid);
        let entry1 = self.call1(self.h.resolve_global_slot_guarded, &[self.cx, b], Type::I32);
        let rw1 = writable(self, entry1);
        let slow = self.body.add_block();
        self.cond_br(
            rw1,
            BlockTarget {
                block: use_b,
                args: vec![entry1],
            },
            Self::to(slow),
        );

        self.cur = use_b;
        // Entry: bit 1 selects the dynamic slots, `entry & !7` is the byte
        // offset from that base, `entry >> 3` the slot index within it.
        let sh = self.bin(Operator::I32ShrU, entry, one, Type::I32);
        let dynamic = self.bin(Operator::I32And, sh, one, Type::I32);
        let m = self.i32c(!7);
        let idx8 = self.bin(Operator::I32And, entry, m, Type::I32);
        let z = self.i32c(0);
        let fb = self.i32c(FIXED_SLOTS_BASE);
        let add = self.select(Type::I32, z, fb, dynamic);
        let off = self.bin(Operator::I32Add, idx8, add, Type::I32);
        let slots = self.load_i32(global, NATIVE_SLOTS_OFFSET);
        let slot_base = self.select(Type::I32, slots, global, dynamic);
        let addr = self.bin(Operator::I32Add, slot_base, off, Type::I32);
        self.pre_barrier(addr, 0);
        self.store_i64(addr, 0, val);
        let three = self.i32c(3);
        let idx = self.bin(Operator::I32ShrU, entry, three, Type::I32);
        let flags = self.load_i32(live, SHAPE_IMMUTABLE_FLAGS_OFFSET);
        let fs = self.i32c(SHAPE_FIXED_SLOTS_SHIFT);
        let nf = self.bin(Operator::I32ShrU, flags, fs, Type::I32);
        let fm = self.i32c(SHAPE_FIXED_SLOTS_MASK_BITS);
        let nfixed = self.bin(Operator::I32And, nf, fm, Type::I32);
        let idx_plus = self.bin(Operator::I32Add, idx, nfixed, Type::I32);
        let abs = self.select(Type::I32, idx_plus, idx, dynamic);
        self.post_barrier(self.h.post_write_barrier, global, abs, val);
        self.binding_fuses(bid, val, false, fused);
        let t = self.edge(inst, 0, &[])?;
        self.terminate(Terminator::Br { target: t });
        self.cur = slow;
        Ok(())
    }
}
