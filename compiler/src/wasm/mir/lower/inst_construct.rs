//! inst construct: the lowering of one run of arms of `Lower::inst`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    pub(super) fn inst_construct(
        &mut self,
        inst: mir::Inst,
        d: &mir::func::InstData,
        a: &[Value],
    ) -> R<bool> {
        match d.op {
            Opcode::ExitInline {
                pc,
                nargs,
                nlocals,
                throw,
            } => self.exit_inline(inst, d, a, pc, nargs, nlocals, throw)?,
            Opcode::Construct(nslots, word) => {
                // `new Array()`: the pristine Array
                // constructor, as its own new.target, makes what `[]` does.
                if a.len() == 3 && self.names_atom("Array") {
                    let (callee, nt) = (a[0], a[2]);
                    let is_arr = self.builtin_is(callee, crate::wasm::translate::BC_ARRAY_CTOR);
                    let same = self.bin(Operator::I64Eq, callee, nt, Type::I32);
                    let m = self.bin(Operator::I32And, is_arr, same, Type::I32);
                    let (arr, other) = (self.body.add_block(), self.body.add_block());
                    self.cond_br(m, Self::to(arr), Self::to(other));
                    self.cur = arr;
                    // The helper's reload rebinds the live values on its
                    // path only.
                    let saved = (self.vmap.clone(), self.slotted.clone(), self.dirty.clone());
                    let cell = self.alloc_inline(inst, Some(0))?;
                    let lv = self.i32c(0);
                    let live = self.live_across(inst);
                    let (ok, r) = self.gc_call(self.h.new_array, &[lv, cell], &live)?;
                    let same = self.epoch_same.take().expect("gc_call sampled the epoch");
                    self.clean_or_dirty(inst, ok, same, &[r])?;
                    (self.vmap, self.slotted, self.dirty) = saved;
                    self.cur = other;
                }
                // The frame `[callee, this, args…, new.target]` above the
                // rooting slots, then the runtime's construct (it creates
                // `this` sized and seeded for the site, and runs the
                // constructor). The result lands at the frame's top.
                let live = self.live_across(inst);
                self.root(&live)?;
                let construct_pre = self.epoch();
                let frame = self.top_off(live.len());
                for (k, &v) in a.iter().enumerate() {
                    self.store_i64(self.vp, frame + 8 * u32::try_from(k).unwrap(), v);
                }
                let argc = u32::try_from(a.len() - 3).unwrap();
                let top_off = frame + 8 * u32::try_from(a.len()).unwrap();
                let base = self.add_off(self.vp, frame);
                let top = self.add_off(self.vp, top_off);
                let (callee, new_target) = (a[0], a[a.len() - 1]);
                let join = self.body.add_block();
                let ok_p = self.body.add_blockparam(join, Type::I32);
                let res_p = self.body.add_blockparam(join, Type::I64);
                // Direct: a compiled constructor gets its `this` from
                // `create_this` (sized and stamped for the site) and runs
                // by a direct call, with no trip through the engine's
                // construct.
                const HEADROOM: u32 = 64 * 1024;
                let (funcidx, script) = self.classify(callee);
                let z = self.i32c(0);
                let compiled = self.bin(Operator::I32Ne, funcidx, z, Type::I32);
                let fits = self.stack_fits(top, HEADROOM);
                let maybe = self.bin(Operator::I32And, compiled, fits, Type::I32);
                let enabled = self.i32c(1);
                let maybe = self.bin(Operator::I32And, maybe, enabled, Type::I32);
                let (chk, generic) = (self.body.add_block(), self.body.add_block());
                self.cond_br(maybe, Self::to(chk), Self::to(generic));
                self.cur = chk;
                let fun = self.un(Operator::I32WrapI64, callee, Type::I32);
                let flags = self.load_i32(fun, FUNC_FLAGS_SLOT_OFFSET);
                let cbit = self.i32c(FUNCTION_FLAGS_CONSTRUCTOR);
                let is_ctor = self.bin(Operator::I32And, flags, cbit, Type::I32);
                let direct = self.body.add_block();
                self.cond_br(is_ctor, Self::to(direct), Self::to(generic));
                self.cur = direct;
                let made = self.construct_this(top, callee, new_target, nslots, word);
                let call_b = self.body.add_block();
                let undef = self.i64c(UNDEF);
                self.cond_br(
                    made,
                    Self::to(call_b),
                    BlockTarget {
                        block: join,
                        args: vec![made, undef],
                    },
                );
                self.cur = call_b;
                let thisv = self.load_i64(top, 0);
                self.store_i64(base, FrameLayout::THIS, thisv);
                // `create_this` may GC: the frame's copies are current, the
                // operands from before it are not.
                let new_target = self.load_i64(base, 8 * (argc + 2));
                let off = self.i32c(u32::MAX);
                self.body_off_patches.push(off);
                let body_idx = self.bin(Operator::I32Sub, funcidx, off, Type::I32);
                let av = self.i32c(argc);
                let args = self
                    .body
                    .arg_pool
                    .from_iter([self.cx, base, av, top, script, new_target, body_idx].into_iter());
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
                let ok_d = self.un(Operator::I32Eqz, err, Type::I32);
                // The constructor's result if an object, else its `this`
                // (reread: the GC updates the frame).
                let r = self.load_i64(top, 0);
                let rt = self.tag_of(r);
                let obj = self.tag_is(rt, TAG_OBJECT as u32);
                let th = self.load_i64(base, FrameLayout::THIS);
                let res = self.select(Type::I64, r, th, obj);
                self.terminate(Terminator::Br {
                    target: BlockTarget {
                        block: join,
                        args: vec![ok_d, res],
                    },
                });
                self.cur = generic;
                let (av, nv, wv) = (self.i32c(argc), self.i32c(nslots), self.i32c(word));
                let ok = self.call1(
                    self.h.construct,
                    &[self.cx, top, base, av, nv, wv],
                    Type::I32,
                );
                let r = self.load_i64(top, 0);
                self.terminate(Terminator::Br {
                    target: BlockTarget {
                        block: join,
                        args: vec![ok, r],
                    },
                });
                self.cur = join;
                self.after_gc(&live);
                let post = self.epoch();
                let same = self.bin(Operator::I32Eq, construct_pre, post, Type::I32);
                self.clean_or_dirty(inst, ok_p, same, &[res_p])?;
            }
            Opcode::GuardLayout {
                keys,
                types,
                slots,
                closed,
            } => {
                // The stamp word (`JSObject*+4`): identity is layout key + 1
                // in the low 16 bits; TYPES is the SHALLOW bit (§4.3). The
                // SLOTS bit too: a claim then holds
                // the fields at their slots, until a kill (only the engine
                // clears SLOTS, bumping the epoch), so the field ops under
                // it load and store with no test of their own.
                let w = self.load_i32(a[0], OBJ_CLASS_IDX_OFFSET);
                let m = self.i32c(0xFFFF);
                let id = self.bin(Operator::I32And, w, m, Type::I32);
                let want = if types { CLASS_WORD_SHALLOW } else { 0 }
                    | if slots { CLASS_WORD_SLOTS } else { 0 }
                    | if closed { CLASS_WORD_CLOSED } else { 0 };
                // One masked compare: identity and
                // the bits at once. Over a range, the masked word less the
                // lowest wanted word is at most the span only with every
                // bit there (a missing bit, at 1 << 16 or above, wraps it).
                // The sentinel too, which the expected word lacks: under
                // it the low half is a set of fields, not an identity.
                let mask = self.i32c(CLASS_WORD_SENTINEL | 0xFFFF | want);
                let wm = self.bin(Operator::I32And, w, mask, Type::I32);
                let exp = self.i32c((keys.lo.get() + 1) | want);
                let ok = if keys.lo == keys.hi {
                    self.bin(Operator::I32Eq, wm, exp, Type::I32)
                } else {
                    let rel = self.bin(Operator::I32Sub, wm, exp, Type::I32);
                    let span = self.i32c(keys.hi.get() - keys.lo.get());
                    self.bin(Operator::I32LeU, rel, span, Type::I32)
                };
                self.guard_word = Some((w, id, keys));
                self.guard_obj = Some(a[0]);
                self.guard(inst, ok, &[a[0]])?;
                self.guard_word = None;
                self.guard_obj = None;
            }
            Opcode::GuardCtor { key, n, types } => {
                // Under construction for `key` (§2.3): the sentinel with
                // its early key, SLOTS (and TYPES), the word's field set
                // `n`, and the span of `n` (every field is at its row's
                // slot, so a property past them would raise it): exactly
                // the fields `n`.
                let w = self.load_i32(a[0], OBJ_CLASS_IDX_OFFSET);
                let bits = CLASS_WORD_SLOTS | if types { CLASS_WORD_SHALLOW } else { 0 };
                let fm = (1 << mir::types::FieldSet::WORD_BITS) - 1;
                let m =
                    self.i32c(CLASS_WORD_SENTINEL | (EARLY_KEY_MAX << EARLY_KEY_SHIFT) | bits | fm);
                let want = self.i32c(
                    CLASS_WORD_SENTINEL
                        | ((key.get() + 1) << EARLY_KEY_SHIFT)
                        | bits
                        | n.word_bits(),
                );
                let wm = self.bin(Operator::I32And, w, m, Type::I32);
                let ok = self.bin(Operator::I32Eq, wm, want, Type::I32);
                let shape = self.load_i32(a[0], SHAPE_OFFSET);
                let imm = self.load_i32(shape, SHAPE_IMMUTABLE_FLAGS_OFFSET);
                let sm = self.i32c(SHAPE_SMALL_SLOTSPAN_MASK_BITS << SHAPE_SMALL_SLOTSPAN_SHIFT);
                let span = self.bin(Operator::I32And, imm, sm, Type::I32);
                let nv = self.i32c(n.span() << SHAPE_SMALL_SLOTSPAN_SHIFT);
                let s_ok = self.bin(Operator::I32Eq, span, nv, Type::I32);
                let ok = self.bin(Operator::I32And, ok, s_ok, Type::I32);
                self.guard(inst, ok, &[a[0]])?;
            }
            Opcode::InitField(name) => self.init_field(inst, name, a[0], a[1])?,
            Opcode::PublishLayout => {
                // The stamp of a completed `constructing(n)` object (its
                // type proves the sentinel, the key and every field): its
                // identity, keeping SLOTS and, where claimed, TYPES.
                let c = self
                    .ty(d.args[0])
                    .obj_info()
                    .and_then(|o| o.layout)
                    .ok_or("lowering: publish_layout without a layout claim")?;
                let w = self.load_i32(a[0], OBJ_CLASS_IDX_OFFSET);
                let keep = CLASS_WORD_SLOTS | if c.types { CLASS_WORD_SHALLOW } else { 0 };
                let kb = self.i32c(keep);
                let bits = self.bin(Operator::I32And, w, kb, Type::I32);
                // Exactly the row's fields: CLOSED, where the layout may be.
                let closed = if self.mm.layouts.get(&c.keys.lo).is_some_and(|l| l.closed) {
                    CLASS_WORD_CLOSED
                } else {
                    0
                };
                let idx = self.i32c((c.keys.lo.get() + 1) | closed);
                let nw = self.bin(Operator::I32Or, idx, bits, Type::I32);
                self.store_i32(a[0], OBJ_CLASS_IDX_OFFSET, nw);
                self.def(inst, a[0]);
            }
            Opcode::CheckFuse(fuse) => {
                let addr = self.mm.fuses[fuse].addr;
                if addr == 0 {
                    return Err(format!("lowering: {fuse} has no fuse word"));
                }
                let base = self.i32c(addr);
                let w = self.load_i32(base, 0);
                let one = self.i32c(1);
                let ok = self.bin(Operator::I32Eq, w, one, Type::I32);
                self.guard(inst, ok, &[])?;
            }
            Opcode::MethodLoad { cell, atom, script } => {
                // The receiver's prototype is the one the cell is armed
                // for (an unarmed cell holds 0, an unarmable one 1): the
                // function there. Else, if unarmed, the runtime arms it
                // for this prototype (a leaf) and it is tested again.
                let sa = self
                    .mm
                    .script_addrs
                    .get(&script)
                    .copied()
                    .ok_or("lowering: method.load of a script with no address")?;
                let c = self.i32c(cell);
                let shape = self.load_i32(a[0], SHAPE_OFFSET);
                let base = self.load_i32(shape, SHAPE_BASESHAPE_OFFSET);
                let proto = self.load_i32(base, BASESHAPE_PROTO_OFFSET);
                let p = self.load_i32(c, 0);
                let hit = self.bin(Operator::I32Eq, proto, p, Type::I32);
                // CLOSED (the receiver is of a published layout: its word
                // is a stamp's).
                let w = self.load_i32(a[0], OBJ_CLASS_IDX_OFFSET);
                let cb = self.i32c(CLASS_WORD_CLOSED);
                let closed = self.bin(Operator::I32And, w, cb, Type::I32);
                let f = self.load_i32(c, crate::region_shape::METHOD_CELL_FN_OFF);
                let (miss, open) = (self.body.add_block(), self.body.add_block());
                let fail = self.edge(inst, 1, &[])?;
                self.cond_br(closed, Self::to(open), fail);
                self.cur = open;
                let t = self.edge(inst, 0, &[f])?;
                self.cond_br(hit, t, Self::to(miss));
                self.cur = miss;
                let (arm, done) = (self.body.add_block(), self.body.add_block());
                let ok = self.body.add_blockparam(done, Type::I32);
                let z = self.i32c(0);
                let unarmed = self.bin(Operator::I32Eq, p, z, Type::I32);
                self.cond_br(
                    unarmed,
                    Self::to(arm),
                    BlockTarget {
                        block: done,
                        args: vec![z],
                    },
                );
                self.cur = arm;
                let rb = self.box_tagged(TAG_OBJECT, a[0]);
                let av = self.atom(atom);
                let sv = self.i32c(sa);
                let r = self.call1(self.h.method_arm, &[rb, c, av, sv], Type::I32);
                self.terminate(Terminator::Br {
                    target: BlockTarget {
                        block: done,
                        args: vec![r],
                    },
                });
                self.cur = done;
                let f = self.load_i32(c, crate::region_shape::METHOD_CELL_FN_OFF);
                self.guard(inst, ok, &[f])?;
            }
            Opcode::CheckBinding(b, BindingCheck::Fn) => {
                // The value fuse in its predicted state: the binding holds
                // a compiled function of its predicted script. A cell a
                // major GC zeroed re-arms at the resolve, so a miss resolves
                // once before it exits.
                let slot = self.mm.bindings[b].slot;
                let vals = self.i32c(self.h.global_vals_base + 16 * slot);
                let fw = self.load_i32(vals, 8);
                let want = self.i32c(BINDING_FUSE_PREDICTED);
                let ok = self.bin(Operator::I32Eq, fw, want, Type::I32);
                let miss = self.body.add_block();
                let t = self.edge(inst, 0, &[])?;
                self.cond_br(ok, t, Self::to(miss));
                self.cur = miss;
                let bv = self.i32c(slot);
                self.call(
                    self.h.resolve_global_slot_guarded,
                    &[self.cx, bv],
                    &[Type::I32],
                );
                let fw = self.load_i32(vals, 8);
                let ok = self.bin(Operator::I32Eq, fw, want, Type::I32);
                self.guard(inst, ok, &[])?;
            }
            Opcode::CheckBinding(b, k) => {
                let slot = self.mm.bindings[b].slot;
                let write = k == BindingCheck::Write;
                if !write {
                    // A read of a binding whose value fuse is armed
                    // (`gGlobalVals`) needs no slot: `load_gname` takes the
                    // value there. Straight to `ok`, the test kept for the
                    // loads its fact reaches (they dominate them).
                    let vals = self.i32c(self.h.global_vals_base + 16 * slot);
                    let fw = self.load_i32(vals, 8);
                    let armed = self.fuse_armed(fw);
                    if let Some(&EdgeArg::Out(0)) = d.succs[0].args.first() {
                        let fact = self.f.blocks[d.succs[0].block].params[0];
                        self.binding_armed.insert(fact, armed);
                    }
                    let chk = self.body.add_block();
                    let t = self.edge(inst, 0, &[])?;
                    self.cond_br(armed, t, Self::to(chk));
                    self.cur = chk;
                } else {
                    // An armed value fuse says the binding is still the
                    // global's own data property (a redefinition, delete or
                    // lexical shadow blows it), and a resolved row whether
                    // it is writable (bit 2; a major GC zeroes the row,
                    // which the full test below resolves again).
                    let vals = self.i32c(self.h.global_vals_base + 16 * slot);
                    let fw = self.load_i32(vals, 8);
                    let base = self.i32c(self.h.global_slots_base);
                    let entry = self.load_i32(base, 8 * slot);
                    let two = self.i32c(2);
                    let armed4 = self.bin(Operator::I32Shl, fw, two, Type::I32);
                    let both = self.bin(Operator::I32And, armed4, entry, Type::I32);
                    let four = self.i32c(4);
                    let fast = self.bin(Operator::I32And, both, four, Type::I32);
                    let chk = self.body.add_block();
                    let t = self.edge(inst, 0, &[])?;
                    self.cond_br(fast, t, Self::to(chk));
                    self.cur = chk;
                }
                let ok = self.binding_ok(slot, write);
                self.guard(inst, ok, &[])?;
            }
            Opcode::LoadGName(b) => {
                // The value fuse's copy while armed, else the slot. The
                // check that made the fact tested the fuse: its test, where
                // the fact is that check's (the fuse only disarms through
                // a write, which kills the fact).
                let slot = self.mm.bindings[b].slot;
                let vals = self.i32c(self.h.global_vals_base + 16 * slot);
                if self.ty(d.args[0]) == MType::Fact(FactKind::BindingFn(b)) {
                    // In the predicted state, which a write leaves only
                    // for another function of the script (and kills the
                    // fact meanwhile): the copy is the value.
                    let v = self.load_i64(vals, 0);
                    self.def(inst, v);
                    return Ok(true);
                }
                // (A write to a global disarms its fuse but keeps the
                // fact: only a load in the check's `ok` block, before any
                // write there.)
                let fact = d.args[0];
                let fresh = matches!(self.f.values[fact].def, mir::func::ValueDef::Param(b, _) if b == self.cur_mblock)
                    && !self.f.blocks[self.cur_mblock]
                        .insts
                        .iter()
                        .take_while(|&&i| i != inst)
                        .any(|&i| matches!(self.f.insts[i].op, Opcode::StoreGName(_)));
                let armed = match self.binding_armed.get(&fact).filter(|_| fresh) {
                    Some(&a) => a,
                    None => {
                        let fw = self.load_i32(vals, 8);
                        self.fuse_armed(fw)
                    }
                };
                let (hit, miss, join) = (
                    self.body.add_block(),
                    self.body.add_block(),
                    self.body.add_block(),
                );
                let r = self.body.add_blockparam(join, Type::I64);
                self.cond_br(armed, Self::to(hit), Self::to(miss));
                self.cur = hit;
                let v = self.load_i64(vals, 0);
                self.terminate(Terminator::Br {
                    target: BlockTarget {
                        block: join,
                        args: vec![v],
                    },
                });
                self.cur = miss;
                let addr = self.binding_addr(slot);
                let v = self.load_i64(addr, 0);
                self.terminate(Terminator::Br {
                    target: BlockTarget {
                        block: join,
                        args: vec![v],
                    },
                });
                self.cur = join;
                self.def(inst, r);
            }
            Opcode::StoreGName(b) => {
                let def = &self.mm.bindings[b];
                let (slot, name) = (def.slot, def.name);
                let fused = self.gname_fused.get(&name).copied();
                let gc_free = value_tags(&self.ty(d.args[1])).subset_of(GC_FREE_TAGS);
                self.binding_store(slot, a[1], gc_free, fused);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
