//! stamps: methods of `Lower`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    /// A layout constructor's first stamp of its completed `this`, inline
    /// (`night_runtime_ctor_stamp`'s gates): an object still under construction whose early key is ours
    /// or none, with a slot span covering the row, gets the layout's idx
    /// plus the validity bits that survived construction.
    pub(super) fn ctor_stamp_inline(
        &mut self,
        thisv: Value,
        layout: u32,
        nfields: u32,
        keep: u32,
        exact: bool,
    ) {
        let done = self.body.add_block();
        let tag = self.tag_of(thisv);
        let is_obj = self.tag_is(tag, TAG_OBJECT as u32);
        self.check(is_obj, done);
        let obj = self.un(Operator::I32WrapI64, thisv, Type::I32);
        let w0 = self.load_i32(obj, OBJ_CLASS_IDX_OFFSET);
        let sb = self.i32c(CLASS_WORD_SENTINEL);
        let sent = self.bin(Operator::I32And, w0, sb, Type::I32);
        self.check(sent, done);
        let km = self.i32c(EARLY_KEY_MAX << EARLY_KEY_SHIFT);
        let key = self.bin(Operator::I32And, w0, km, Type::I32);
        let mine = self.i32c((layout + 1) << EARLY_KEY_SHIFT);
        let ours = self.bin(Operator::I32Eq, key, mine, Type::I32);
        let owned = if exact {
            ours
        } else {
            let z = self.i32c(0);
            let none = self.bin(Operator::I32Eq, key, z, Type::I32);
            self.bin(Operator::I32Or, none, ours, Type::I32)
        };
        self.check(owned, done);
        let shape = self.load_i32(obj, SHAPE_OFFSET);
        let imm = self.load_i32(shape, SHAPE_IMMUTABLE_FLAGS_OFFSET);
        let sh = self.i32c(SHAPE_SMALL_SLOTSPAN_SHIFT);
        let span = self.bin(Operator::I32ShrU, imm, sh, Type::I32);
        let sm = self.i32c(SHAPE_SMALL_SLOTSPAN_MASK_BITS);
        let span = self.bin(Operator::I32And, span, sm, Type::I32);
        let n = self.i32c(nfields);
        let covers = self.bin(Operator::I32GeU, span, n, Type::I32);
        self.check(covers, done);
        // A permuted shape's span may cover holes: count its properties.
        let pb = self.i32c(SHAPE_PERMUTED_SLOTS_BIT);
        let perm = self.bin(Operator::I32And, imm, pb, Type::I32);
        let (holes, covered) = (self.body.add_block(), self.body.add_block());
        self.cond_br(perm, Self::to(holes), Self::to(covered));
        self.cur = holes;
        let r = self.call1(self.h.slots_covered, &[obj, n], Type::I32);
        self.cond_br(r, Self::to(covered), Self::to(done));
        self.cur = covered;
        if let Some(census) = self.exit_census.filter(|_| keep & CLASS_WORD_SHALLOW != 0) {
            // `--mir-exit-census`: a stamp publishing without the TYPES
            // its layout claims (lost during construction), by layout.
            let tb = self.i32c(CLASS_WORD_SHALLOW);
            let t = self.bin(Operator::I32And, w0, tb, Type::I32);
            let (count, go) = (self.body.add_block(), self.body.add_block());
            self.cond_br(t, Self::to(go), Self::to(count));
            self.cur = count;
            let (k, i) = (
                self.i32c(crate::options::MIR_STAMP_NO_TYPES_CENSUS_KIND),
                self.i32c(layout),
            );
            self.call1(census, &[k, i], Type::I32);
            self.terminate(Terminator::Br {
                target: Self::to(go),
            });
            self.cur = go;
        }
        // CLOSED in `keep` says the layout may be (the bit itself is an
        // early-key bit in `w0`).
        let kb = self.i32c(keep & !CLASS_WORD_CLOSED);
        let bits = self.bin(Operator::I32And, w0, kb, Type::I32);
        let idx = self.i32c(layout + 1);
        let w = self.bin(Operator::I32Or, idx, bits, Type::I32);
        // Every field there and a span of the row's length: nothing else
        // (CLOSED).
        let w = if keep & CLASS_WORD_CLOSED != 0 {
            self.or_closed_if_span(w, span, nfields)
        } else {
            w
        };
        self.store_i32(obj, OBJ_CLASS_IDX_OFFSET, w);
        self.terminate(Terminator::Br {
            target: Self::to(done),
        });
        self.cur = done;
    }

    /// Stamp word `w` with CLOSED where its object holds its row's `n`
    /// fields and nothing else: `span`, its slot span with every slot
    /// below `n` holding a property, is `n`, and `w` keeps SLOTS (every
    /// add landed where the row puts its name, so those properties are the
    /// row's).
    pub(super) fn or_closed_if_span(&mut self, w: Value, span: Value, n: u32) -> Value {
        let nv = self.i32c(n);
        let eq = self.bin(Operator::I32Eq, span, nv, Type::I32);
        let ssh = self.i32c(CLASS_WORD_SLOTS.trailing_zeros());
        let sl = self.bin(Operator::I32ShrU, w, ssh, Type::I32);
        let ok = self.bin(Operator::I32And, eq, sl, Type::I32);
        let one = self.i32c(1);
        let ok = self.bin(Operator::I32And, ok, one, Type::I32);
        let sh = self.i32c(CLASS_WORD_CLOSED.trailing_zeros());
        let c = self.bin(Operator::I32Shl, ok, sh, Type::I32);
        self.bin(Operator::I32Or, w, c, Type::I32)
    }

    /// An inline store's duty to the receiver's class word `w`: RANGES is
    /// consumed checklessly, but by no MIR claim, so it is dropped here
    /// rather than sent to the engine; TYPES survives a number store, and
    /// a non-number one clears it only on an object still under
    /// construction (the CONSTRUCTING sentinel: no guard can have proven
    /// it, so no claim rests on it); on a published one it goes to
    /// `slow`, where the engine keeps the bit.
    pub(super) fn check_store_bits(
        &mut self,
        inst: mir::Inst,
        obj: Value,
        w: Value,
        val: Value,
        _num: bool,
        slow: Block,
    ) {
        self.store_types(inst, obj, w, val, slow);
    }

    /// A store's duty to the object's word `w` (§4.6): TYPES survives
    /// only a value of the stored field's predicted type for the object's
    /// own class (`field_types`: the classes the site expects, as an IC
    /// validating the object's class before it stores); any other store
    /// drops it (`drop_types`). RANGES, which no MIR claim reads, is
    /// dropped either way.
    pub(super) fn store_types(
        &mut self,
        inst: mir::Inst,
        obj: Value,
        w: Value,
        val: Value,
        slow: Block,
    ) {
        let Some(a) = self.f.insts[inst].attach.filter(|&a| {
            let at = &self.f.attachments[a];
            at.field_types_complete || !at.field_types.is_empty()
        }) else {
            self.drop_types(inst, obj, w, slow);
            return;
        };
        let at = &self.f.attachments[a];
        let mut classes: Vec<(u32, TagSet)> = at
            .field_types
            .iter()
            .map(|&(k, m)| (k, mir::func::decode_tags(m)))
            .collect();
        let complete = at.field_types_complete;
        // A receiver proven of some layouts is of no other class.
        let proven = self
            .ty(self.f.insts[inst].args[0])
            .obj_info()
            .and_then(|o| o.layout)
            .filter(|c| c.state == mir::types::LayoutState::Published)
            .map(|c| (c.keys.lo.get() + 1, c.keys.hi.get() + 1));
        if let Some((lo, hi)) = proven {
            if complete {
                classes.retain(|&(k, _)| k >= lo && k <= hi);
                let n = (hi - lo + 1) as usize;
                let first = classes.first().map(|c| c.1);
                if classes.is_empty() {
                    self.clear_bits(obj, w, CLASS_WORD_RANGES);
                    return;
                }
                if classes.len() == n && classes.iter().all(|c| Some(c.1) == first) {
                    self.keep_types_known(inst, obj, w, val, first.unwrap(), slow);
                    return;
                }
            }
        }
        // By the object's class: its stamp's identity, or while it is
        // constructed its early key (none: no class, no claim to check).
        let m16 = self.i32c(0xFFFF);
        let idx = self.bin(Operator::I32And, w, m16, Type::I32);
        let ksh = self.i32c(EARLY_KEY_SHIFT);
        let kraw = self.bin(Operator::I32ShrU, w, ksh, Type::I32);
        let km = self.i32c(EARLY_KEY_MAX);
        let early = self.bin(Operator::I32And, kraw, km, Type::I32);
        let sb = self.i32c(CLASS_WORD_SENTINEL);
        let sent = self.bin(Operator::I32And, w, sb, Type::I32);
        let id = self.select(Type::I32, early, idx, sent);
        let join = self.body.add_block();
        for (k, t) in classes {
            let kv = self.i32c(k);
            let is = self.bin(Operator::I32Eq, id, kv, Type::I32);
            let (this_b, next) = (self.body.add_block(), self.body.add_block());
            self.cond_br(is, Self::to(this_b), Self::to(next));
            self.cur = this_b;
            self.keep_types_known(inst, obj, w, val, t, slow);
            self.terminate(Terminator::Br {
                target: Self::to(join),
            });
            self.cur = next;
        }
        // Another class: with the list complete its layout types no such
        // field (the store touches no claim), unless it has no class.
        if complete {
            let z = self.i32c(0);
            let none = self.bin(Operator::I32Eq, id, z, Type::I32);
            let (drop_b, keep_b) = (self.body.add_block(), self.body.add_block());
            self.cond_br(none, Self::to(drop_b), Self::to(keep_b));
            self.cur = keep_b;
            self.clear_bits(obj, w, CLASS_WORD_RANGES);
            self.terminate(Terminator::Br {
                target: Self::to(join),
            });
            self.cur = drop_b;
        }
        self.drop_types(inst, obj, w, slow);
        self.terminate(Terminator::Br {
            target: Self::to(join),
        });
        self.cur = join;
    }

    /// The set helper's vouch bit (`night_runtime_set_prop_ic_miss`'s
    /// flag 2) for a store of `val` through `recv`: 2 when `recv` is an
    /// object whose class types the field and `val` is of that type, or,
    /// the site's list complete, a class typing no such field; else 0.
    /// The helper's own stores (replays, the mega-set probe) then keep
    /// TYPES, as the inline arms do: a polymorphic site's other classes
    /// miss its one inline way, and must not be demoted for it.
    pub(super) fn vouch_types(&mut self, inst: mir::Inst, recv: Value, val: Value) -> Value {
        let Some(a) = self.f.insts[inst].attach.filter(|&a| {
            let at = &self.f.attachments[a];
            at.field_types_complete || !at.field_types.is_empty()
        }) else {
            return self.i32c(0);
        };
        let at = &self.f.attachments[a];
        let classes: Vec<(u32, TagSet)> = at
            .field_types
            .iter()
            .map(|&(k, m)| (k, mir::func::decode_tags(m)))
            .collect();
        let complete = at.field_types_complete;
        let vt = match self.ty(self.f.insts[inst].args[1]) {
            MType::Val(s) => s.tags,
            _ => TagSet::ALL,
        };
        // A non-object's word is read off the null page, then ignored.
        let tag = self.tag_of(recv);
        let is_obj = self.tag_is(tag, TAG_OBJECT as u32);
        let o = self.un(Operator::I32WrapI64, recv, Type::I32);
        let z = self.i32c(0);
        let obj = self.select(Type::I32, o, z, is_obj);
        let w = self.load_i32(obj, OBJ_CLASS_IDX_OFFSET);
        // Its class: the stamp's identity, or while it is constructed its
        // early key.
        let m16 = self.i32c(0xFFFF);
        let idx = self.bin(Operator::I32And, w, m16, Type::I32);
        let ksh = self.i32c(EARLY_KEY_SHIFT);
        let kraw = self.bin(Operator::I32ShrU, w, ksh, Type::I32);
        let km = self.i32c(EARLY_KEY_MAX);
        let early = self.bin(Operator::I32And, kraw, km, Type::I32);
        let sb = self.i32c(CLASS_WORD_SENTINEL);
        let sent = self.bin(Operator::I32And, w, sb, Type::I32);
        let id = self.select(Type::I32, early, idx, sent);
        let mut ok = self.i32c(0);
        let mut listed = self.i32c(0);
        for (k, t) in classes {
            let kv = self.i32c(k);
            let is = self.bin(Operator::I32Eq, id, kv, Type::I32);
            listed = self.bin(Operator::I32Or, listed, is, Type::I32);
            let conf = if vt.is_nonempty_subset_of(t) {
                self.i32c(1)
            } else if vt.intersect(t).is_empty() {
                self.i32c(0)
            } else {
                self.has_tags(val, t)
            };
            let hit = self.bin(Operator::I32And, is, conf, Type::I32);
            ok = self.bin(Operator::I32Or, ok, hit, Type::I32);
        }
        if complete {
            let unlisted = self.un(Operator::I32Eqz, listed, Type::I32);
            let keyed = self.bin(Operator::I32Ne, id, z, Type::I32);
            let other = self.bin(Operator::I32And, unlisted, keyed, Type::I32);
            ok = self.bin(Operator::I32Or, ok, other, Type::I32);
        }
        let ok = self.bin(Operator::I32And, ok, is_obj, Type::I32);
        let one = self.i32c(1);
        self.bin(Operator::I32Shl, ok, one, Type::I32)
    }

    /// A store that falsifies TYPES: on an object under construction (no
    /// fact covers its word) the bit is cleared here; on a published one
    /// the store goes to `slow`, the engine's, which clears it and bumps
    /// the epoch (so the op reports dirt, and facts leave with it).
    pub(super) fn drop_types(&mut self, inst: mir::Inst, obj: Value, w: Value, slow: Block) {
        let m = self.i32c(CLASS_WORD_SHALLOW | CLASS_WORD_SENTINEL);
        let bits = self.bin(Operator::I32And, w, m, Type::I32);
        let pub_shallow = self.i32c(CLASS_WORD_SHALLOW);
        let bad = self.bin(Operator::I32Eq, bits, pub_shallow, Type::I32);
        let ok = self.un(Operator::I32Eqz, bad, Type::I32);
        self.check(ok, slow);
        if let Some(census) = self.exit_census {
            // `--mir-exit-census`: which stores drop TYPES from an object
            // under construction (no epoch bump, so no other trace).
            static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let d = &self.f.insts[inst];
            let site = d.attach.and_then(|a| self.f.attachments[a].site);
            crate::diag_line!(
                "night: mir typesdrop {id} sid#{} {:?} {} [{:?}]",
                self.f.script,
                site,
                mir::print::mnemonic(&d.op),
                d.args.get(1).map(|&v| self.ty(v))
            );
            let all = self.i32c(CLASS_WORD_SHALLOW | CLASS_WORD_SENTINEL);
            let hit = self.bin(Operator::I32Eq, bits, all, Type::I32);
            let (count, go) = (self.body.add_block(), self.body.add_block());
            self.cond_br(hit, Self::to(count), Self::to(go));
            self.cur = count;
            let (k, i) = (
                self.i32c(crate::options::MIR_TYPES_DROP_CENSUS_KIND),
                self.i32c(id),
            );
            self.call1(census, &[k, i], Type::I32);
            self.terminate(Terminator::Br {
                target: Self::to(go),
            });
            self.cur = go;
        }
        self.clear_bits(obj, w, CLASS_WORD_RANGES | CLASS_WORD_SHALLOW);
    }

    /// A store of `val` to an object whose class types the field `mask`:
    /// TYPES survives iff the value is of it.
    pub(super) fn keep_types_known(
        &mut self,
        inst: mir::Inst,
        obj: Value,
        w: Value,
        val: Value,
        mask: TagSet,
        slow: Block,
    ) {
        let vt = match self.ty(self.f.insts[inst].args[1]) {
            MType::Val(s) => s.tags,
            _ => TagSet::ALL,
        };
        if vt.is_nonempty_subset_of(mask) {
            self.clear_bits(obj, w, CLASS_WORD_RANGES);
        } else if vt.intersect(mask).is_empty() {
            self.drop_types(inst, obj, w, slow);
        } else {
            let conf = self.has_tags(val, mask);
            let (keep, drop, join) = (
                self.body.add_block(),
                self.body.add_block(),
                self.body.add_block(),
            );
            self.cond_br(conf, Self::to(keep), Self::to(drop));
            self.cur = keep;
            self.clear_bits(obj, w, CLASS_WORD_RANGES);
            self.terminate(Terminator::Br {
                target: Self::to(join),
            });
            self.cur = drop;
            self.drop_types(inst, obj, w, slow);
            self.terminate(Terminator::Br {
                target: Self::to(join),
            });
            self.cur = join;
        }
    }

    /// An element store's duty to the array stamp's claims on `obj`'s
    /// word (`ranges_duty` says it owes one): RANGES cleared, and TYPES
    /// where the word is an array's stamp, whose TYPES claims its
    /// elements. On any other object (BigInteger's digits are elements of
    /// a plain object) TYPES is its fields' claim, which an element store
    /// cannot touch.
    pub(super) fn elem_duty(&mut self, obj: Value) {
        let w = self.load_i32(obj, OBJ_CLASS_IDX_OFFSET);
        let Some(min) = self.mm.array_key_min else {
            self.clear_bits(obj, w, CLASS_WORD_RANGES);
            return;
        };
        // A stamped word's identity (under the sentinel the low half is a
        // set of fields).
        let m16 = self.i32c(CLASS_WORD_SENTINEL | 0xFFFF);
        let idx = self.bin(Operator::I32And, w, m16, Type::I32);
        let lo = self.i32c(min);
        let hi = self.i32c(0xFFFF);
        let arr_lo = self.bin(Operator::I32GeU, idx, lo, Type::I32);
        let arr_hi = self.bin(Operator::I32LeU, idx, hi, Type::I32);
        let arr = self.bin(Operator::I32And, arr_lo, arr_hi, Type::I32);
        let (a, o, join) = (
            self.body.add_block(),
            self.body.add_block(),
            self.body.add_block(),
        );
        self.cond_br(arr, Self::to(a), Self::to(o));
        self.cur = a;
        self.clear_bits(obj, w, CLASS_WORD_RANGES | CLASS_WORD_SHALLOW);
        self.terminate(Terminator::Br {
            target: Self::to(join),
        });
        self.cur = o;
        self.clear_bits(obj, w, CLASS_WORD_RANGES);
        self.terminate(Terminator::Br {
            target: Self::to(join),
        });
        self.cur = join;
    }

    /// Clear `mask`'s bits of `obj`'s class word `w`, if any is set.
    pub(super) fn clear_bits(&mut self, obj: Value, w: Value, mask: u32) {
        let m = self.i32c(mask);
        let bits = self.bin(Operator::I32And, w, m, Type::I32);
        let (clr, done) = (self.body.add_block(), self.body.add_block());
        self.cond_br(bits, Self::to(clr), Self::to(done));
        self.cur = clr;
        let keep = self.i32c(!mask);
        let nw = self.bin(Operator::I32And, w, keep, Type::I32);
        self.store_i32(obj, OBJ_CLASS_IDX_OFFSET, nw);
        // A demotion of a stamped word invalidates facts: bump the epoch
        // (not for a mid-construction sentinel word, which no fact
        // covers), as the runtime's `NightNoteDemotion`.
        let sent = self.i32c(CLASS_WORD_SENTINEL);
        let s = self.bin(Operator::I32And, w, sent, Type::I32);
        let (bump, after) = (self.body.add_block(), self.body.add_block());
        self.cond_br(s, Self::to(after), Self::to(bump));
        self.cur = bump;
        self.bump_epoch();
        self.terminate(Terminator::Br {
            target: Self::to(after),
        });
        self.cur = after;
        self.terminate(Terminator::Br {
            target: Self::to(done),
        });
        self.cur = done;
    }

    /// `restamp`: the
    /// runtime's two-phase gates (`night_runtime_ctor_restamp`), with its
    /// two common outcomes inline: a word already of the layout stays,
    /// and a native object under construction for it (its early key none,
    /// the layout's or a prefix's) with every field's slot is stamped.
    /// Anything else (a prefix-stamped word advancing, a short or capped
    /// slot span, not an object) is the helper's.
    pub(super) fn restamp(&mut self, this: Value, r: [u32; 7]) {
        let [layout, nfields, keep, prefixes @ ..] = r;
        let done = self.body.add_block();
        let (obj_b, slow) = (self.body.add_block(), self.body.add_block());
        let tag = self.tag_of(this);
        let is_obj = self.tag_is(tag, TAG_OBJECT as u32);
        self.cond_br(is_obj, Self::to(obj_b), Self::to(done));
        self.cur = obj_b;
        let obj = self.un(Operator::I32WrapI64, this, Type::I32);
        let w0 = self.load_i32(obj, OBJ_CLASS_IDX_OFFSET);
        // A stamped word's identity (under the sentinel the low half is a
        // set of fields).
        let m16 = self.i32c(CLASS_WORD_SENTINEL | 0xFFFF);
        let idx = self.bin(Operator::I32And, w0, m16, Type::I32);
        let want = self.i32c(layout + 1);
        let already = self.bin(Operator::I32Eq, idx, want, Type::I32);
        let work = self.body.add_block();
        self.cond_br(already, Self::to(done), Self::to(work));
        self.cur = work;
        // The sentinel with an early key it owns.
        let sentb = self.i32c(CLASS_WORD_SENTINEL);
        let sent = self.bin(Operator::I32And, w0, sentb, Type::I32);
        let z = self.i32c(0);
        let s_nz = self.bin(Operator::I32Ne, sent, z, Type::I32);
        let km = self.i32c(EARLY_KEY_MAX << EARLY_KEY_SHIFT);
        let key = self.bin(Operator::I32And, w0, km, Type::I32);
        let mut k_ok = self.bin(Operator::I32Eq, key, z, Type::I32);
        for k in std::iter::once(layout + 1).chain(prefixes.iter().copied().filter(|&p| p != 0)) {
            let kv = self.i32c(k << EARLY_KEY_SHIFT);
            let e = self.bin(Operator::I32Eq, key, kv, Type::I32);
            k_ok = self.bin(Operator::I32Or, k_ok, e, Type::I32);
        }
        let ok = self.bin(Operator::I32And, s_nz, k_ok, Type::I32);
        // A native object with every field's slot.
        let shape = self.load_i32(obj, SHAPE_OFFSET);
        let imm = self.load_i32(shape, SHAPE_IMMUTABLE_FLAGS_OFFSET);
        let nb = self.i32c(SHAPE_IS_NATIVE_BIT);
        let native = self.bin(Operator::I32And, imm, nb, Type::I32);
        let native = self.bin(Operator::I32Ne, native, z, Type::I32);
        let sh = self.i32c(SHAPE_SMALL_SLOTSPAN_SHIFT);
        let span = self.bin(Operator::I32ShrU, imm, sh, Type::I32);
        let sm = self.i32c(SHAPE_SMALL_SLOTSPAN_MASK_BITS);
        let span = self.bin(Operator::I32And, span, sm, Type::I32);
        let n = self.i32c(nfields);
        let full = self.bin(Operator::I32GeU, span, n, Type::I32);
        let ok = self.bin(Operator::I32And, ok, native, Type::I32);
        let ok = self.bin(Operator::I32And, ok, full, Type::I32);
        // A permuted shape's span may cover holes: the helper counts.
        let pb = self.i32c(SHAPE_PERMUTED_SLOTS_BIT);
        let perm = self.bin(Operator::I32And, imm, pb, Type::I32);
        let seq = self.bin(Operator::I32Eq, perm, z, Type::I32);
        let ok = self.bin(Operator::I32And, ok, seq, Type::I32);
        let stamp = self.body.add_block();
        self.cond_br(ok, Self::to(stamp), Self::to(slow));
        self.cur = stamp;
        let kb = self.i32c(keep & !CLASS_WORD_CLOSED);
        let bits = self.bin(Operator::I32And, w0, kb, Type::I32);
        let nw = self.bin(Operator::I32Or, want, bits, Type::I32);
        let nw = if keep & CLASS_WORD_CLOSED != 0 {
            self.or_closed_if_span(nw, span, nfields)
        } else {
            nw
        };
        self.store_i32(obj, OBJ_CLASS_IDX_OFFSET, nw);
        self.terminate(Terminator::Br {
            target: Self::to(done),
        });
        self.cur = slow;
        let mut args = vec![this];
        for x in r {
            args.push(self.i32c(x));
        }
        self.call(self.h.ctor_restamp, &args, &[]);
        self.terminate(Terminator::Br {
            target: Self::to(done),
        });
        self.cur = done;
    }
}
