//! props: methods of `Lower`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    /// The address `off` bytes into the property-IC region (a site's ways
    /// or add row): the absolute address, patched in. Addressing the ways
    /// from a base the function loads from its context instead (the
    /// per-context regions prototype) cost, against the absolute form,
    /// react 5%, box2d 3.9%, deltablue 3.3%, earley-boyer 3.1% and
    /// raytrace 2.2% (2026-10-04).
    pub(super) fn ic_addr(&mut self, off: u32) -> Value {
        let c = self.i32c(IC_WAY_ADDR_PLACEHOLDER);
        self.prop_ic_patches.push((c, off));
        c
    }

    /// A property read's inline cache past its special arms (a
    /// fact-free read): the ways, then the shared probe `night_ic_get`
    /// (own and holder ways, the megamorphic table), taking `ok_clean` on
    /// a hit; a miss runs the generic get and fills the site's ways.
    pub(super) fn get_ic(
        &mut self,
        inst: mir::Inst,
        name: mir::entity::AtomId,
        recv: Value,
        way_base: Value,
        cache: u32,
    ) -> R<()> {
        let at = self.atom(name);
        let probe = self.body.add_block();
        self.get_ic_ways(inst, recv, way_base, cache * INLINE_IC_STRIDE, probe)?;
        self.cur = probe;
        let r = self.call(self.h.ic_get_poly, &[recv, at, way_base], &[Type::I64]);
        let tag = self.tag_of(r);
        let miss = self.tag_is(tag, TAG_MAGIC as u32);
        let slow = self.body.add_block();
        let t = self.edge(inst, 0, &[r])?;
        self.cond_br(miss, Self::to(slow), t);
        self.cur = slow;
        if let Some(census) = self.exit_census {
            static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            crate::diag_line!(
                "night: mir getmiss {id} sid#{} {}",
                self.f.script,
                String::from_utf16_lossy(self.mm.atoms[name].chars())
            );
            let (k, i) = (
                self.i32c(crate::options::MIR_GET_MISS_CENSUS_KIND),
                self.i32c(id),
            );
            self.call1(census, &[k, i], Type::I32);
        }
        let c = self.i32c(cache);
        self.js_call(inst, self.h.get_prop_ic_miss, &[recv, at, c], false)
    }

    /// `getprop.data`'s inline cache: `get_ic`'s ways and probe taking
    /// `ok`, then the runtime's pure lookup (a leaf: no rooting), which
    /// fills the ways as the IC miss does; its magic result (the lookup
    /// would run code or throw) takes `fail`.
    pub(super) fn get_ic_pure(
        &mut self,
        inst: mir::Inst,
        name: mir::entity::AtomId,
        recv: Value,
        way_base: Value,
        cache: u32,
    ) -> R<()> {
        let at = self.atom(name);
        let probe = self.body.add_block();
        self.get_ic_ways(inst, recv, way_base, cache * INLINE_IC_STRIDE, probe)?;
        self.cur = probe;
        let r = self.call(self.h.ic_get_poly, &[recv, at, way_base], &[Type::I64]);
        let tag = self.tag_of(r);
        let miss = self.tag_is(tag, TAG_MAGIC as u32);
        let slow = self.body.add_block();
        let t = self.edge(inst, 0, &[r])?;
        self.cond_br(miss, Self::to(slow), t);
        self.cur = slow;
        self.slow_census(inst);
        let c = self.i32c(cache);
        let r = self.call(self.h.get_prop_pure, &[self.cx, recv, at, c], &[Type::I64]);
        let tag = self.tag_of(r);
        let fail = self.tag_is(tag, TAG_MAGIC as u32);
        let t = self.edge(inst, 0, &[r])?;
        let f = self.edge(inst, 1, &[])?;
        self.cond_br(fail, f, t);
        Ok(())
    }

    /// A property store's inline cache: way 0 (an overwrite of the own slot
    /// the way describes) and the add-transition replay, taking `ok_clean`;
    /// a miss runs the generic set (vouched where the value keeps the
    /// object's TYPES) and fills the way.
    pub(super) fn set_ic(
        &mut self,
        inst: mir::Inst,
        name: mir::entity::AtomId,
        recv: Value,
        val: Value,
        strict: bool,
    ) -> R<()> {
        let at = self.atom(name);
        let cache = self.atoms.next_prop_cache();
        let way = self.ic_addr(cache * INLINE_IC_STRIDE);
        let (trans, slow) = (self.body.add_block(), self.body.add_block());
        self.set_ic_ways(inst, at, recv, val, way, trans, slow)?;
        self.cur = trans;
        let row = self.add_off(way, IC_TRANS_ROW_OFF);
        let preds = self.add_preds.get(&name).cloned().unwrap_or_default();
        let num = matches!(self.ty(self.f.insts[inst].args[1]), MType::Val(s) if s.tags.subset_of(TagSet::NUMBER));
        self.set_ic_trans(inst, &preds, recv, val, num, row, slow)?;
        self.cur = slow;
        if let Some(census) = self.exit_census {
            static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let site = self.f.insts[inst]
                .attach
                .and_then(|a| self.f.attachments[a].site);
            crate::diag_line!(
                "night: mir setmiss {id} sid#{} frame {} {:?} {}",
                self.f.script,
                self.cur_frame,
                site,
                std::string::String::from_utf16_lossy(self.mm.atoms[name].chars())
            );
            let (k, i) = (
                self.i32c(crate::options::MIR_SET_MISS_CENSUS_KIND),
                self.i32c(id),
            );
            self.call1(census, &[k, i], Type::I32);
        }
        let vouch = self.vouch_types(inst, recv, val);
        let (c, sv) = (self.i32c(cache), self.i32c(u32::from(strict)));
        let sv = self.bin(Operator::I32Or, sv, vouch, Type::I32);
        self.js_call(
            inst,
            self.h.set_prop_ic_miss,
            &[recv, at, val, c, sv],
            false,
        )
    }

    /// `--mir-exit-census`: count a split op's call of its runtime helper
    /// (kind `MIR_SLOW_CENSUS_KIND`), with its static record.
    pub(super) fn slow_census(&mut self, inst: mir::Inst) {
        let Some(census) = self.exit_census else {
            return;
        };
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name = match self.f.insts[inst].op {
            Opcode::GetPropData(n) | Opcode::SetPropData(n) => {
                format!(" {}", String::from_utf16_lossy(self.mm.atoms[n].chars()))
            }
            _ => String::new(),
        };
        crate::diag_line!(
            "night: mir slowcall {id} sid#{} {}{name}",
            self.f.script,
            mir::print::mnemonic(&self.f.insts[inst].op)
        );
        let (k, i) = (
            self.i32c(crate::options::MIR_SLOW_CENSUS_KIND),
            self.i32c(id),
        );
        self.call1(census, &[k, i], Type::I32);
    }

    /// 1 iff boxed `v` is an object whose ToPrimitive would run user code
    /// (anything but Object.prototype's own conversion: the runtime's pure
    /// check, a leaf).
    pub(super) fn converts_user(&mut self, v: Value) -> Value {
        let t = self.tag_of(v);
        let obj = self.tag_is(t, TAG_OBJECT as u32);
        let (chk, join) = (self.body.add_block(), self.body.add_block());
        let r = self.body.add_blockparam(join, Type::I32);
        let z = self.i32c(0);
        self.cond_br(
            obj,
            Self::to(chk),
            BlockTarget {
                block: join,
                args: vec![z],
            },
        );
        self.cur = chk;
        let ok = self.call1(self.h.to_primitive_pure, &[self.cx, v], Type::I32);
        let bad = self.un(Operator::I32Eqz, ok, Type::I32);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: join,
                args: vec![bad],
            },
        });
        self.cur = join;
        r
    }

    /// `setprop.data`'s inline cache: `set_ic`'s way and add-transition
    /// replay taking `ok_clean` (they demote no claim MIR reads), then the
    /// runtime's set, which fails where it would run code, and reports a
    /// demotion (`ok_dirty`).
    pub(super) fn set_ic_pure(
        &mut self,
        inst: mir::Inst,
        name: mir::entity::AtomId,
        recv: Value,
        val: Value,
    ) -> R<()> {
        let at = self.atom(name);
        let cache = self.atoms.next_prop_cache();
        let way = self.ic_addr(cache * INLINE_IC_STRIDE);
        let (trans, slow) = (self.body.add_block(), self.body.add_block());
        self.set_ic_ways(inst, at, recv, val, way, trans, slow)?;
        self.cur = trans;
        let row = self.add_off(way, IC_TRANS_ROW_OFF);
        let preds = self.add_preds.get(&name).cloned().unwrap_or_default();
        let num = matches!(self.ty(self.f.insts[inst].args[1]), MType::Val(s) if s.tags.subset_of(TagSet::NUMBER));
        self.set_ic_trans(inst, &preds, recv, val, num, row, slow)?;
        self.cur = slow;
        self.slow_census(inst);
        let vouch = self.vouch_types(inst, recv, val);
        let c = self.i32c(cache);
        let live = self.live_across(inst);
        let (r, _) = self.gc_call(self.h.set_prop_pure, &[recv, at, val, c, vouch], &live)?;
        self.epoch_same = None;
        self.store_codes(inst, r)
    }

    /// A data store's helper result `r` (1 clean, 2 dirty, 0 fail, 3 err)
    /// to the edge of that role (`setprop.data`, `setelem.data`).
    pub(super) fn store_codes(&mut self, inst: mir::Inst, r: Value) -> R<()> {
        for (code, idx) in [(1, 0), (2, 1), (0, 2)] {
            let c = self.i32c(code);
            let is = self.bin(Operator::I32Eq, r, c, Type::I32);
            let t = self.edge(inst, idx, &[])?;
            let other = self.body.add_block();
            self.cond_br(is, t, Self::to(other));
            self.cur = other;
        }
        let e = self.edge(inst, 3, &[])?;
        self.terminate(Terminator::Br { target: e });
        Ok(())
    }

    /// `load_field`/`store_field` (§4.3). Under a SLOTS claim (a guard
    /// proved it, or the object is under construction), a field with a
    /// slot prediction is at its slot, read or written directly. Any other
    /// access (a field with no slot prediction, or a claim of TYPES
    /// alone) finds the slot through the site's inline cache: the field,
    /// by name, is still of its claim's type. A store keeps TYPES only with
    /// a value of the field's predicted type (`store_types`); at a known
    /// slot another demotes the word inline (the store choke: the bits
    /// cleared, the epoch bumped) and takes `ok_dirty`. GC barriers unless
    /// the value is a number.
    pub(super) fn field_op(
        &mut self,
        inst: mir::Inst,
        name: mir::entity::AtomId,
        obj: Value,
        val: Option<Value>,
    ) -> R<()> {
        let recv_ty = self.ty(self.f.insts[inst].args[0]);
        let claim = recv_ty
            .obj_info()
            .and_then(|o| o.layout)
            .ok_or("lowering: a field op without a layout claim")?;
        let slot = self
            .mm
            .layouts
            .get(&claim.keys.lo)
            .and_then(|l| l.field(name))
            .map(|(s, _)| u32::try_from(s).unwrap());
        let Some(slot) = slot.filter(|_| claim.slots) else {
            let boxed = self.box_tagged(TAG_OBJECT, obj);
            return match val {
                None => {
                    let cache = self.atoms.next_prop_cache();
                    let way_base = self.ic_addr(cache * INLINE_IC_STRIDE);
                    self.get_ic(inst, name, boxed, way_base, cache)
                }
                Some(v) => self.set_ic(inst, name, boxed, v, self.strict),
            };
        };
        let off = FIXED_SLOTS_BASE + 8 * slot;
        let Some(v) = val else {
            let r = self.load_i64(obj, off);
            let t = self.edge(inst, 0, &[r])?;
            self.terminate(Terminator::Br { target: t });
            return Ok(());
        };
        let num = matches!(self.ty(self.f.insts[inst].args[1]), MType::Val(s) if s.tags.subset_of(TagSet::NUMBER));
        let w = self.load_i32(obj, OBJ_CLASS_IDX_OFFSET);
        let demote = self.body.add_block();
        self.store_types(inst, obj, w, v, demote);
        for dirty in [false, true] {
            if dirty {
                self.cur = demote;
                self.clear_bits(obj, w, CLASS_WORD_RANGES | CLASS_WORD_SHALLOW);
            }
            if !num {
                self.pre_barrier(obj, off);
            }
            self.store_i64(obj, off, v);
            if !num {
                let s = self.i32c(slot);
                self.post_barrier(self.h.post_write_barrier, obj, s, v);
            }
            let t = self.edge(inst, usize::from(dirty), &[])?;
            self.terminate(Terminator::Br { target: t });
        }
        Ok(())
    }

    /// The incremental pre-write barrier on the slot at `obj + off`: while
    /// the zone is marking, mark the value about to be overwritten.
    pub(super) fn pre_barrier(&mut self, addr: Value, off: u32) {
        let zone = self.load_i32(self.cx, JSCONTEXT_ZONE_OFFSET);
        let flag = self.load_i32(zone, ZONE_NEEDS_BARRIER_OFFSET);
        let (marking, cont) = (self.body.add_block(), self.body.add_block());
        self.cond_br(flag, Self::to(marking), Self::to(cont));
        self.cur = marking;
        let old = self.load_i64(addr, off);
        self.call(self.h.pre_write_barrier, &[old], &[]);
        self.terminate(Terminator::Br {
            target: Self::to(cont),
        });
        self.cur = cont;
    }

    /// The generational post-write barrier for storing boxed `v` into slot
    /// or element `slot` of `obj`: a nursery GC thing into a tenured object
    /// is recorded in the store buffer by `helper` (the slot or element
    /// form).
    pub(super) fn post_barrier(&mut self, helper: Func, obj: Value, slot: Value, v: Value) {
        let cont = self.body.add_block();
        let mask = self.i32c(NOT_CHUNK_MASK);
        let chunk = self.bin(Operator::I32And, obj, mask, Type::I32);
        let owner_sb = self.load_i32(chunk, CHUNK_STORE_BUFFER_OFFSET);
        let tenured = self.body.add_block();
        self.cond_br(owner_sb, Self::to(cont), Self::to(tenured));
        self.cur = tenured;
        let tag = self.tag_of(v);
        let min = self.i32c(VAL_GCTHING_TAG_MIN);
        let is_gc = self.bin(Operator::I32GeU, tag, min, Type::I32);
        let gc = self.body.add_block();
        self.cond_br(is_gc, Self::to(gc), Self::to(cont));
        self.cur = gc;
        let cell = self.un(Operator::I32WrapI64, v, Type::I32);
        let chunk = self.bin(Operator::I32And, cell, mask, Type::I32);
        let sb = self.load_i32(chunk, CHUNK_STORE_BUFFER_OFFSET);
        let record = self.body.add_block();
        self.cond_br(sb, Self::to(record), Self::to(cont));
        self.cur = record;
        let owner = self.box_tagged(TAG_OBJECT, obj);
        self.call(helper, &[owner, slot, v], &[]);
        self.terminate(Terminator::Br {
            target: Self::to(cont),
        });
        self.cur = cont;
    }

    pub(super) fn get_ic_ways(
        &mut self,
        inst: mir::Inst,
        recv: Value,
        way0: Value,
        way_off: u32,
        probe: Block,
    ) -> R<()> {
        use crate::region_shape::{INLINE_IC_WAYS, INLINE_IC_WAY_BYTES};
        self.check_recv_object(inst, recv, probe);
        let obj = self.un(Operator::I32WrapI64, recv, Type::I32);
        let shape = self.load_i32(obj, SHAPE_OFFSET);
        let hit_b = self.body.add_block();
        let way = self.body.add_blockparam(hit_b, Type::I32);
        for w in 0..INLINE_IC_WAYS {
            let wb = if w == 0 {
                way0
            } else {
                self.ic_addr(way_off + w * INLINE_IC_WAY_BYTES)
            };
            let wshape = self.load_i32(wb, IC_WAY_RECVSHAPE);
            let m = self.bin(Operator::I32Eq, shape, wshape, Type::I32);
            let next = if w + 1 < INLINE_IC_WAYS {
                self.body.add_block()
            } else {
                probe
            };
            self.cond_br(
                m,
                BlockTarget {
                    block: hit_b,
                    args: vec![wb],
                },
                Self::to(next),
            );
            self.cur = next;
        }
        self.cur = hit_b;
        let moff = self.load_i32(way, IC_WAY_MONO_OFF);
        let (own, tail) = (self.body.add_block(), self.body.add_block());
        self.cond_br(moff, Self::to(own), Self::to(tail));
        self.cur = own;
        let addr = self.bin(Operator::I32Add, obj, moff, Type::I32);
        let v = self.load_i64(addr, 0);
        let t = self.edge(inst, 0, &[v])?;
        self.terminate(Terminator::Br { target: t });
        self.cur = tail;
        let hp = self.load_i32(way, IC_WAY_HOLDERPTR);
        let chs = self.load_i32(way, IC_WAY_HOLDERPTR + 4);
        let enc = self.load_i32(way, IC_WAY_HOLDERPTR + 8);
        let base = self.select(Type::I32, hp, obj, hp);
        let live = self.load_i32(base, SHAPE_OFFSET);
        let same = self.bin(Operator::I32Eq, live, chs, Type::I32);
        self.check(same, probe);
        // A proven absence.
        let absent = self.i32c(crate::region_shape::IC_SLOT_ENC_ABSENT);
        let is_absent = self.bin(Operator::I32Eq, enc, absent, Type::I32);
        let (abs_b, ld_b) = (self.body.add_block(), self.body.add_block());
        self.cond_br(is_absent, Self::to(abs_b), Self::to(ld_b));
        self.cur = abs_b;
        let undef = self.i64c(TAG_UNDEFINED << 32);
        let t = self.edge(inst, 0, &[undef])?;
        self.terminate(Terminator::Br { target: t });
        self.cur = ld_b;
        let one = self.i32c(1);
        let dynamic = self.bin(Operator::I32And, enc, one, Type::I32);
        let not1 = self.i32c(!1);
        let off = self.bin(Operator::I32And, enc, not1, Type::I32);
        let slots = self.load_i32(base, NATIVE_SLOTS_OFFSET);
        let sb = self.select(Type::I32, slots, base, dynamic);
        let addr = self.bin(Operator::I32Add, sb, off, Type::I32);
        let v = self.load_i64(addr, 0);
        let t = self.edge(inst, 0, &[v])?;
        self.terminate(Terminator::Br { target: t });
        Ok(())
    }

    /// A set IC's way 0 (not its
    /// transition and megamorphic arms): with `recv` an object of the
    /// way's shape, store `val` to the slot the way names and take
    /// `ok_clean`; else branch to `slow`. The store bypasses the engine's
    /// choke, so it also requires the object's word to carry no bit it
    /// could falsify: RANGES never, TYPES unless `val` is a number.
    /// A set IC's overwrite arms: way 0 (the site's one shape), or past
    /// one shape (way 0 sentineled) the (shape, atom) row the module's
    /// `night_ic_set_cold` finds in the mega-set table. Both
    /// name the slot for one shared store; a mono miss takes `trans`, no
    /// mega row or a non-object `slow`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn set_ic_ways(
        &mut self,
        inst: mir::Inst,
        atom: Value,
        recv: Value,
        val: Value,
        way: Value,
        trans: Block,
        slow: Block,
    ) -> R<()> {
        use crate::region_shape::{MEGA_SET_ABS_SLOT_OFF, MEGA_SET_SLOT_ENC_OFF};
        let num = matches!(self.ty(self.f.insts[inst].args[1]), MType::Val(s) if s.tags.subset_of(TagSet::NUMBER));
        self.check_recv_object(inst, recv, slow);
        let obj = self.un(Operator::I32WrapI64, recv, Type::I32);
        let shape = self.load_i32(obj, SHAPE_OFFSET);
        let store = self.body.add_block();
        let enc = self.body.add_blockparam(store, Type::I32);
        let abs = self.body.add_blockparam(store, Type::I32);
        let to_store = |s: &mut Self, src: Value, enc_off: u32, abs_off: u32| {
            let e = s.load_i32(src, enc_off);
            let a = if num { e } else { s.load_i32(src, abs_off) };
            s.terminate(Terminator::Br {
                target: BlockTarget {
                    block: store,
                    args: vec![e, a],
                },
            });
        };
        let cached = self.load_i32(way, IC_SET_RECVSHAPE);
        let hit = self.bin(Operator::I32Eq, shape, cached, Type::I32);
        let (w0, poly) = (self.body.add_block(), self.body.add_block());
        self.cond_br(hit, Self::to(w0), Self::to(poly));
        self.cur = w0;
        to_store(self, way, IC_SET_SLOTENC, IC_SET_ABSSLOT);
        self.cur = poly;
        let sentinel = self.i32c(crate::wasm::mir::abi::IC_POLY_SENTINEL);
        let is_poly = self.bin(Operator::I32Eq, cached, sentinel, Type::I32);
        let mega = self.body.add_block();
        self.cond_br(is_poly, Self::to(mega), Self::to(trans));
        self.cur = mega;
        let cold = self.call1(self.h.ic_set_cold, &[shape, way, atom], Type::I64);
        let found = self.un(Operator::I64Eqz, cold, Type::I32);
        let mhit = self.un(Operator::I32Eqz, found, Type::I32);
        if let Some(census) = self.exit_census {
            static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let site = self.f.insts[inst]
                .attach
                .and_then(|a| self.f.attachments[a].site);
            crate::diag_line!("night: mir megaset {id} sid#{} {:?}", self.f.script, site);
            let k = self.i32c(crate::options::MIR_MEGA_SET_CENSUS_KIND);
            let base = self.i32c(id * 2);
            let i = self.bin(Operator::I32Add, base, mhit, Type::I32);
            self.call1(census, &[k, i], Type::I32);
        }
        self.check(mhit, slow);
        let entry = self.un(Operator::I32WrapI64, cold, Type::I32);
        to_store(self, entry, MEGA_SET_SLOT_ENC_OFF, MEGA_SET_ABS_SLOT_OFF);
        // The store: the slot `enc & 1` selects the dynamic slots over the
        // object, `enc & !1` is the byte offset from that base.
        self.cur = store;
        let w = self.load_i32(obj, OBJ_CLASS_IDX_OFFSET);
        self.check_store_bits(inst, obj, w, val, num, slow);
        let one = self.i32c(1);
        let dynamic = self.bin(Operator::I32And, enc, one, Type::I32);
        let not1 = self.i32c(!1);
        let off = self.bin(Operator::I32And, enc, not1, Type::I32);
        let slots = self.load_i32(obj, NATIVE_SLOTS_OFFSET);
        let base = self.op(Operator::Select, &[slots, obj, dynamic], Some(Type::I32));
        let addr = self.bin(Operator::I32Add, base, off, Type::I32);
        if !num {
            self.pre_barrier(addr, 0);
        }
        self.store_i64(addr, 0, val);
        if !num {
            self.post_barrier(self.h.post_write_barrier, obj, abs, val);
        }
        let t = self.edge(inst, 0, &[])?;
        self.terminate(Terminator::Br { target: t });
        Ok(())
    }

    /// A set IC's add-transition row replayed inline (in a
    /// sound subset): `recv` an object of the row's pre-add shape, the
    /// row's prototype hops unchanged (all four, live), and the add unable to falsify the object's class-word bits.
    /// That holds when it has no SLOTS, or lands where one of the name's
    /// predicted (layout key, offset) pairs says; with no RANGES; and no
    /// TYPES unless the value is a number. Then store the fresh slot, swap
    /// the shape word and take `ok_clean`; else branch to `slow` (the
    /// helper keeps the bits itself).
    pub(super) fn set_ic_trans(
        &mut self,
        inst: mir::Inst,
        preds: &[(u32, u32)],
        recv: Value,
        val: Value,
        num: bool,
        row: Value,
        slow: Block,
    ) -> R<()> {
        self.check_recv_object(inst, recv, slow);
        let obj = self.un(Operator::I32WrapI64, recv, Type::I32);
        let shape = self.load_i32(obj, SHAPE_OFFSET);
        let old = self.load_i32(row, IC_TRANS_OLDSHAPE);
        let m_old = self.bin(Operator::I32Eq, old, shape, Type::I32);
        self.check(m_old, slow);
        let slot_off = self.load_i32(row, IC_TRANS_SLOTOFF);
        self.check(slot_off, slow);
        // Every recorded hop, live: a class hierarchy built with
        // prototype objects (`inheritsFrom`) puts three or four protos
        // under its instances, and a constructor's adds that fell to the
        // helper would each clear TYPES there.
        for n in 0..IC_TRANS_PROTO_HOPS {
            let p = self.load_i32(row, IC_TRANS_PROTO0 + IC_TRANS_PROTO_ROW_BYTES * n);
            let empty = self.un(Operator::I32Eqz, p, Type::I32);
            let want = self.load_i32(row, IC_TRANS_PROTO0 + IC_TRANS_PROTO_ROW_BYTES * n + 4);
            // An empty hop's load reads the null page's first word: only
            // its `empty` matters.
            let live = self.load_i32(p, SHAPE_OFFSET);
            let same = self.bin(Operator::I32Eq, live, want, Type::I32);
            let ok = self.bin(Operator::I32Or, empty, same, Type::I32);
            self.check(ok, slow);
        }
        // The class word.
        let w = self.load_i32(obj, OBJ_CLASS_IDX_OFFSET);
        self.check_store_bits(inst, obj, w, val, num, slow);
        let sb = self.i32c(CLASS_WORD_SLOTS);
        let slots = self.bin(Operator::I32And, w, sb, Type::I32);
        let (keyed, go) = (self.body.add_block(), self.body.add_block());
        self.cond_br(slots, Self::to(keyed), Self::to(go));
        self.cur = keyed;
        // The live layout key: the early key under the CONSTRUCTING
        // sentinel, else the stamped identity (they are disjoint).
        let ksh = self.i32c(EARLY_KEY_SHIFT);
        let kraw = self.bin(Operator::I32ShrU, w, ksh, Type::I32);
        let km = self.i32c(EARLY_KEY_MAX);
        let k_sent = self.bin(Operator::I32And, kraw, km, Type::I32);
        let m16 = self.i32c(0xFFFF);
        let k_idx = self.bin(Operator::I32And, w, m16, Type::I32);
        let sb = self.i32c(CLASS_WORD_SENTINEL);
        let sent = self.bin(Operator::I32And, w, sb, Type::I32);
        let k = self.select(Type::I32, k_sent, k_idx, sent);
        // An add past the layout's extent (its bound in the this-cells
        // table; 0 = unknown) leaves every predicted slot where it was:
        // SLOTS holds.
        let three = self.i32c(3);
        let koff = self.bin(Operator::I32Shl, k, three, Type::I32);
        let tbase = self.i32c(self.h.this_cells_base.wrapping_sub(8));
        let taddr = self.bin(Operator::I32Add, tbase, koff, Type::I32);
        let bound = self.load_i32(taddr, 0);
        let z = self.i32c(0);
        let known = self.bin(Operator::I32Ne, bound, z, Type::I32);
        let keyed_k = self.bin(Operator::I32Ne, k, z, Type::I32);
        let past = self.bin(Operator::I32GeU, slot_off, bound, Type::I32);
        let past = self.bin(Operator::I32And, past, known, Type::I32);
        let past = self.bin(Operator::I32And, past, keyed_k, Type::I32);
        let mut hit = past;
        for &(key, off) in preds {
            let kv = self.i32c(key);
            let ke = self.bin(Operator::I32Eq, k, kv, Type::I32);
            let ov = self.i32c(off);
            let oe = self.bin(Operator::I32Eq, slot_off, ov, Type::I32);
            let both = self.bin(Operator::I32And, ke, oe, Type::I32);
            hit = self.bin(Operator::I32Or, hit, both, Type::I32);
        }
        // An add to a CLOSED object (which holds SLOTS, so is keyed) is
        // outside its row: the helper, whose engine add clears the bit.
        let cm = self.i32c(CLASS_WORD_SENTINEL | CLASS_WORD_CLOSED);
        let cbits = self.bin(Operator::I32And, w, cm, Type::I32);
        let cv = self.i32c(CLASS_WORD_CLOSED);
        let open = self.bin(Operator::I32Ne, cbits, cv, Type::I32);
        let hit = self.bin(Operator::I32And, hit, open, Type::I32);
        self.cond_br(hit, Self::to(go), Self::to(slow));
        self.cur = go;
        let addr = self.bin(Operator::I32Add, obj, slot_off, Type::I32);
        self.store_i64(addr, 0, val);
        let new_s = self.load_i32(row, IC_TRANS_NEWSHAPE);
        self.store_i32(obj, SHAPE_OFFSET, new_s);
        if !num {
            let abs = self.load_i32(row, IC_TRANS_ABSSLOT);
            self.post_barrier(self.h.post_write_barrier, obj, abs, val);
        }
        let t = self.edge(inst, 0, &[])?;
        self.terminate(Terminator::Br { target: t });
        Ok(())
    }

    /// `init_field` (§2.3): add field `n` of the receiver's
    /// `constructing(n)` layout, of a value of its type. The site's
    /// add-transition row replayed inline where it adds exactly that
    /// field at its fixed slot (every recorded prototype hop live, as
    /// `set_ic_trans`); else the runtime's plain add, which also fills the
    /// row, continuing iff the object is then `constructing(n + 1)`, and
    /// leaving the rest of the constructor to baseline (`fail`) if not.
    /// TYPES holds by the value's type; RANGES, which no MIR claim reads,
    /// is dropped.
    pub(super) fn init_field(
        &mut self,
        inst: mir::Inst,
        name: mir::entity::AtomId,
        obj: Value,
        val: Value,
    ) -> R<()> {
        let d = &self.f.insts[inst];
        let c = self
            .ty(d.args[0])
            .obj_info()
            .and_then(|o| o.layout)
            .ok_or("lowering: init_field without a layout claim")?;
        let mir::types::LayoutState::Constructing(n) = c.state else {
            return Err("lowering: init_field on a published object".into());
        };
        let i = self
            .mm
            .layouts
            .get(&c.keys.lo)
            .and_then(|l| l.field(name))
            .map(|(i, _)| u32::try_from(i).unwrap())
            .ok_or("lowering: init_field of a field its layout lacks")?;
        let num = matches!(self.ty(d.args[1]), MType::Val(s) if s.tags.subset_of(TagSet::NUMBER));
        let at = self.atom(name);
        let cache = self.atoms.next_prop_cache();
        let way = self.ic_addr(cache * INLINE_IC_STRIDE);
        let slow = self.body.add_block();
        let row = self.add_off(way, IC_TRANS_ROW_OFF);
        let shape = self.load_i32(obj, SHAPE_OFFSET);
        let old = self.load_i32(row, IC_TRANS_OLDSHAPE);
        let m_old = self.bin(Operator::I32Eq, old, shape, Type::I32);
        self.check(m_old, slow);
        let off = FIXED_SLOTS_BASE + 8 * i;
        let slot_off = self.load_i32(row, IC_TRANS_SLOTOFF);
        let want = self.i32c(off);
        let at_slot = self.bin(Operator::I32Eq, slot_off, want, Type::I32);
        self.check(at_slot, slow);
        for h in 0..IC_TRANS_PROTO_HOPS {
            let p = self.load_i32(row, IC_TRANS_PROTO0 + IC_TRANS_PROTO_ROW_BYTES * h);
            let empty = self.un(Operator::I32Eqz, p, Type::I32);
            let wv = self.load_i32(row, IC_TRANS_PROTO0 + IC_TRANS_PROTO_ROW_BYTES * h + 4);
            let live = self.load_i32(p, SHAPE_OFFSET);
            let same = self.bin(Operator::I32Eq, live, wv, Type::I32);
            let ok = self.bin(Operator::I32Or, empty, same, Type::I32);
            self.check(ok, slow);
        }
        // An add past the span (a field ahead of others in the row): the
        // slots it skips, which the new shape covers, hold undefined until
        // their fields come.
        for j in n.span()..i {
            let u = self.i64c(UNDEF);
            self.store_i64(obj, FIXED_SLOTS_BASE + 8 * j, u);
        }
        self.store_i64(obj, off, val);
        let new_s = self.load_i32(row, IC_TRANS_NEWSHAPE);
        self.store_i32(obj, SHAPE_OFFSET, new_s);
        if !num {
            let abs = self.load_i32(row, IC_TRANS_ABSSLOT);
            self.post_barrier(self.h.post_write_barrier, obj, abs, val);
        }
        // The word: the field in its set; RANGES dropped (no MIR claim
        // reads it). The word is the sentinel's, which no fact covers.
        let w = self.load_i32(obj, OBJ_CLASS_IDX_OFFSET);
        let keep = self.i32c(!CLASS_WORD_RANGES);
        let w = self.bin(Operator::I32And, w, keep, Type::I32);
        let w = if i < mir::types::FieldSet::WORD_BITS {
            let b = self.i32c(1 << i);
            self.bin(Operator::I32Or, w, b, Type::I32)
        } else {
            w
        };
        self.store_i32(obj, OBJ_CLASS_IDX_OFFSET, w);
        let t = self.edge(inst, 0, &[obj])?;
        self.terminate(Terminator::Br { target: t });
        self.cur = slow;
        let live = self.live_across(inst);
        let recv = self.box_tagged(TAG_OBJECT, obj);
        let bits = CLASS_WORD_SLOTS | if c.types { CLASS_WORD_SHALLOW } else { 0 };
        let (cv, sv, bv) = (self.i32c(cache), self.i32c(n.with(i).0), self.i32c(bits));
        let (ok, out) = self.gc_call(self.h.init_field, &[recv, at, val, cv, sv, bv], &live)?;
        self.epoch_same = None;
        let o2 = self.un(Operator::I32WrapI64, out, Type::I32);
        let t = self.edge(inst, 0, &[o2])?;
        let f = self.edge(inst, 1, &[])?;
        self.cond_br(ok, t, f);
        Ok(())
    }
}
