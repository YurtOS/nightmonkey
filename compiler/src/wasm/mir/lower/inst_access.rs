//! inst access: the lowering of one run of arms of `Lower::inst`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    pub(super) fn inst_access(
        &mut self,
        inst: mir::Inst,
        d: &mir::func::InstData,
        a: &[Value],
    ) -> R<bool> {
        match d.op {
            Opcode::JsGetProp(name) | Opcode::GetPropData(name) => {
                // The site's inline cache (a fact-free read): the
                // shared probe `night_ic_get` (own and holder ways, then
                // the megamorphic table) takes `ok_clean` on a hit; a miss
                // runs the generic get and fills the site's ways. For
                // `getprop.data` the same arms take `ok`, and a miss asks
                // the runtime's pure lookup, failing where it would run
                // code.
                let cache = self.atoms.next_prop_cache();
                let way_base = self.ic_addr(cache * INLINE_IC_STRIDE);
                let nm = String::from_utf16_lossy(self.mm.atoms[name].chars());
                if nm == "charCodeAt" || nm == "charAt" {
                    // A string's pristine char method, while String.prototype
                    // is untouched: the cached native.
                    let cell = if nm == "charCodeAt" {
                        self.h.str_ccat_cell
                    } else {
                        self.h.str_cat_cell
                    };
                    let ic = self.body.add_block();
                    let tag = self.tag_of(a[0]);
                    let is_str = self.tag_is(tag, TAG_STRING as u32);
                    let fslot = self.i32c(self.h.str_fuse_addr_slot);
                    let faddr = self.load_i32(fslot, 0);
                    let fword = self.load_i32(faddr, 0);
                    let intact = self.un(Operator::I32Eqz, fword, Type::I32);
                    let c = self.i32c(cell);
                    let bits = self.load_i64(c, 0);
                    let z = self.i64c(0);
                    let armed = self.bin(Operator::I64Ne, bits, z, Type::I32);
                    let ok = self.bin(Operator::I32And, is_str, intact, Type::I32);
                    let ok = self.bin(Operator::I32And, ok, armed, Type::I32);
                    let hit = self.body.add_block();
                    self.cond_br(ok, Self::to(hit), Self::to(ic));
                    self.cur = hit;
                    let t = self.edge(inst, 0, &[bits])?;
                    self.terminate(Terminator::Br { target: t });
                    self.cur = ic;
                }
                if self.mm.atoms[name].chars()
                    == "length".encode_utf16().collect::<Vec<u16>>().as_slice()
                {
                    // Not a slot: a string's or an array's own word.
                    let ic = self.body.add_block();
                    self.length_arms(inst, a[0], ic)?;
                    self.cur = ic;
                }
                if let Opcode::GetPropData(_) = d.op {
                    self.get_ic_pure(inst, name, a[0], way_base, cache)?;
                } else {
                    self.get_ic(inst, name, a[0], way_base, cache)?;
                }
            }
            Opcode::JsSetProp(name, strict) => self.set_ic(inst, name, a[0], a[1], strict)?,
            Opcode::SetPropData(name) => self.set_ic_pure(inst, name, a[0], a[1])?,
            Opcode::JsGetElem | Opcode::GetElemData => {
                // An in-bounds, non-hole dense element of a native object
                // inline (as baseline does), taking `ok_clean` (`ok`);
                // everything else through the helper (for `getelem.data`,
                // its pure read, failing where the read would run code).
                let slow = self.body.add_block();
                let v = self.dense_element(a[0], a[1], slow);
                let t = self.edge(inst, 0, &[v])?;
                self.terminate(Terminator::Br { target: t });
                self.cur = slow;
                // The other pure arms, where the operands' types admit
                // them: a string's char, and a string key's megamorphic
                // probe (`obj[name]`).
                let (rt, kt) = (
                    value_tags(&self.ty(d.args[0])),
                    value_tags(&self.ty(d.args[1])),
                );
                if !rt.intersect(TagSet::prims(PRIM_STRING)).is_empty()
                    && !kt.intersect(TagSet::INT32).is_empty()
                {
                    let other = self.body.add_block();
                    let c = self.string_char(a[0], a[1], other);
                    let t = self.edge(inst, 0, &[c])?;
                    self.terminate(Terminator::Br { target: t });
                    self.cur = other;
                }
                if !rt.intersect(TagSet::OBJECT).is_empty()
                    && !kt.intersect(TagSet::prims(PRIM_STRING)).is_empty()
                {
                    let other = self.body.add_block();
                    let tag = self.tag_of(a[0]);
                    let is_obj = self.tag_is(tag, TAG_OBJECT as u32);
                    let ktag = self.tag_of(a[1]);
                    let is_str = self.tag_is(ktag, TAG_STRING as u32);
                    let both = self.bin(Operator::I32And, is_obj, is_str, Type::I32);
                    self.check(both, other);
                    let obj = self.un(Operator::I32WrapI64, a[0], Type::I32);
                    let r = self.call1(self.h.elem_mega_get, &[obj, a[1]], Type::I64);
                    let rtag = self.tag_of(r);
                    let miss = self.tag_is(rtag, TAG_MAGIC as u32);
                    let hit = self.body.add_block();
                    self.cond_br(miss, Self::to(other), Self::to(hit));
                    self.cur = hit;
                    let t = self.edge(inst, 0, &[r])?;
                    self.terminate(Terminator::Br { target: t });
                    self.cur = other;
                }
                // An arguments object's element (`args_element`),
                // in a function that makes one.
                if self.makes_args
                    && !rt.intersect(TagSet::OBJECT).is_empty()
                    && !kt.intersect(TagSet::INT32).is_empty()
                {
                    let other = self.body.add_block();
                    let v = self.args_element(a[0], a[1], other);
                    let t = self.edge(inst, 0, &[v])?;
                    self.terminate(Terminator::Br { target: t });
                    self.cur = other;
                }
                if self.ta_poly(inst) {
                    // A typed array of any kind: the
                    // probe's value, or the magic tag on a miss.
                    let generic = self.body.add_block();
                    let (obj, idx) = self.obj_int(a[0], a[1], generic);
                    let r = self.call1(self.h.ta_get_poly, &[obj, idx], Type::I64);
                    let tag = self.tag_of(r);
                    let miss = self.tag_is(tag, TAG_MAGIC as u32);
                    let hit = self.body.add_block();
                    self.cond_br(miss, Self::to(generic), Self::to(hit));
                    self.cur = hit;
                    let t = self.edge(inst, 0, &[r])?;
                    self.terminate(Terminator::Br { target: t });
                    self.cur = generic;
                }
                if d.op == Opcode::GetElemData {
                    self.slow_census(inst);
                    let live = self.live_across(inst);
                    let (r, v) = self.gc_call(self.h.get_elem_pure, &[a[0], a[1]], &live)?;
                    self.epoch_same = None;
                    let one = self.i32c(1);
                    let ok = self.bin(Operator::I32Eq, r, one, Type::I32);
                    let t = self.edge(inst, 0, &[v])?;
                    let other = self.body.add_block();
                    self.cond_br(ok, t, Self::to(other));
                    self.cur = other;
                    let none = self.un(Operator::I32Eqz, r, Type::I32);
                    let f = self.edge(inst, 1, &[])?;
                    let e = self.edge(inst, 2, &[])?;
                    self.cond_br(none, f, e);
                } else {
                    self.js_call(inst, self.h.get_element, &[a[0], a[1]], false)?;
                }
            }
            Opcode::JsSetElem(_, duty) | Opcode::SetElemData(duty) => {
                // An in-bounds overwrite of a non-hole dense element inline,
                // taking `ok_clean`: an own writable data property unless
                // the elements are frozen. The store bypasses the engine,
                // so it owes the array stamp's RANGES claim its duty: a
                // clear for a value not proven inside the claim
                // (no MIR fact rests on it; the epoch bump tells callers').
                let num =
                    matches!(self.ty(d.args[2]), MType::Val(s) if s.tags.subset_of(TagSet::NUMBER));
                let slow = self.body.add_block();
                let (obj, elements, idx, addr, _) = self.dense_slot(a[0], a[1], slow);
                let back = self.i32c(ELEMENTS_FLAGS_BACK);
                let header = self.bin(Operator::I32Sub, elements, back, Type::I32);
                let flags = self.load_i32(header, 0);
                let fz = self.i32c(ELEMENTS_FROZEN_FLAG);
                let frozen = self.bin(Operator::I32And, flags, fz, Type::I32);
                let good = self.un(Operator::I32Eqz, frozen, Type::I32);
                self.check(good, slow);
                if duty {
                    self.elem_duty(obj);
                }
                if !num {
                    self.pre_barrier(addr, 0);
                }
                self.store_i64(addr, 0, a[2]);
                if !num {
                    let f = self.h.post_write_barrier_elem;
                    self.post_barrier(f, obj, idx, a[2]);
                }
                let t = self.edge(inst, 0, &[])?;
                self.terminate(Terminator::Br { target: t });
                self.cur = slow;
                {
                    let generic = self.body.add_block();
                    self.elem_append_arm(inst, a[0], a[1], a[2], duty, num, generic)?;
                    self.cur = generic;
                }
                if self.ta_poly(inst) {
                    // A typed array of any kind (the poly-TA store probe).
                    let generic = self.body.add_block();
                    let (obj, idx) = self.obj_int(a[0], a[1], generic);
                    let ok = self.call1(self.h.ta_set_poly, &[obj, idx, a[2]], Type::I32);
                    let hit = self.body.add_block();
                    self.cond_br(ok, Self::to(hit), Self::to(generic));
                    self.cur = hit;
                    let t = self.edge(inst, 0, &[])?;
                    self.terminate(Terminator::Br { target: t });
                    self.cur = generic;
                }
                // Only where the key may be a string: the arm at every
                // element store cost mandreel 5.6% of its code (-3.9%).
                let key_str = !value_tags(&self.ty(d.args[1]))
                    .intersect(TagSet::STRING)
                    .is_empty();
                if key_str {
                    let generic = self.body.add_block();
                    self.elem_add_arm(inst, a[0], a[1], a[2], generic)?;
                    self.cur = generic;
                }
                if let Opcode::JsSetElem(strict, _) = d.op {
                    let sv = self.i32c(u32::from(strict));
                    self.js_call(inst, self.h.set_element, &[a[0], a[1], a[2], sv], false)?;
                } else {
                    // `setelem.data`: the runtime's set, as `set_ic_pure`'s.
                    self.slow_census(inst);
                    let live = self.live_across(inst);
                    let (r, _) = self.gc_call(self.h.set_elem_pure, &[a[0], a[1], a[2]], &live)?;
                    self.epoch_same = None;
                    self.store_codes(inst, r)?;
                }
            }
            Opcode::LengthArray => {
                // The elements header's length word, unsigned.
                let elements = self.load_i32(a[0], OBJ_ELEMENTS_OFFSET);
                let back = self.i32c(ELEMENTS_LENGTH_BACK);
                let la = self.bin(Operator::I32Sub, elements, back, Type::I32);
                let len = self.load_i32(la, 0);
                let r = self.un(Operator::I64ExtendI32U, len, Type::I64);
                self.def(inst, r);
            }
            Opcode::LengthString => {
                let len = self.load_i32(a[0], STRING_LENGTH_OFFSET);
                self.def(inst, len);
            }
            Opcode::GuardRange(r) => {
                // One unsigned compare: `x - lo <=u hi - lo`.
                let x = a[0];
                let lo = self.i32c(r.lo as i32 as u32);
                let d = self.bin(Operator::I32Sub, x, lo, Type::I32);
                let span = self.i32c((r.hi - r.lo) as u32);
                let ok = self.bin(Operator::I32LeU, d, span, Type::I32);
                self.guard(inst, ok, &[x])?;
            }
            Opcode::IntToI32 => {
                let x = a[0];
                let lo = self.i64c(i64::from(i32::MIN) as u64);
                let hi = self.i64c(i64::from(i32::MAX) as u64);
                let ge = self.bin(Operator::I64GeS, x, lo, Type::I32);
                let le = self.bin(Operator::I64LeS, x, hi, Type::I32);
                let ok = self.bin(Operator::I32And, ge, le, Type::I32);
                let i = self.un(Operator::I32WrapI64, x, Type::I32);
                self.guard(inst, ok, &[i])?;
            }
            Opcode::GuardKind(ObjKind::Array) => {
                let shape = self.load_i32(a[0], SHAPE_OFFSET);
                let base = self.load_i32(shape, SHAPE_BASESHAPE_OFFSET);
                let clasp = self.load_i32(base, BASESHAPE_CLASP_OFFSET);
                let aslot = self.i32c(self.h.array_class_slot);
                let arr_class = self.load_i32(aslot, 0);
                let is_arr = self.bin(Operator::I32Eq, clasp, arr_class, Type::I32);
                self.guard(inst, is_arr, &[a[0]])?;
            }
            Opcode::LengthTa => {
                // Its length slot's payload (a detached one reads 0).
                let len = self.load_i32(a[0], TA_LENGTH_PAYLOAD_OFFSET);
                self.def(inst, len);
            }
            Opcode::GuardKind(ObjKind::Plain) => {
                let shape = self.load_i32(a[0], SHAPE_OFFSET);
                let base = self.load_i32(shape, SHAPE_BASESHAPE_OFFSET);
                let clasp = self.load_i32(base, BASESHAPE_CLASP_OFFSET);
                let pslot =
                    self.i32c(self.h.strlit_slot + crate::region_shape::STRLIT_PLAIN_CLASS_OFF);
                let plain_class = self.load_i32(pslot, 0);
                let is_plain = self.bin(Operator::I32Eq, clasp, plain_class, Type::I32);
                self.guard(inst, is_plain, &[a[0]])?;
            }
            Opcode::GuardKind(ObjKind::Native) => {
                let shape = self.load_i32(a[0], SHAPE_OFFSET);
                let flags = self.load_i32(shape, SHAPE_IMMUTABLE_FLAGS_OFFSET);
                let bit = self.i32c(SHAPE_IS_NATIVE_BIT);
                let native = self.bin(Operator::I32And, flags, bit, Type::I32);
                self.guard(inst, native, &[a[0]])?;
            }
            Opcode::GuardKind(ObjKind::TypedArray(k)) => {
                // The class is the kind's typed-array class.
                let shape = self.load_i32(a[0], SHAPE_OFFSET);
                let base = self.load_i32(shape, SHAPE_BASESHAPE_OFFSET);
                let clasp = self.load_i32(base, BASESHAPE_CLASP_OFFSET);
                let cslot = self.i32c(self.h.ta_class_base + 4 * (u32::from(k.code()) - 1));
                let want = self.load_i32(cslot, 0);
                let ok = self.bin(Operator::I32Eq, clasp, want, Type::I32);
                self.guard(inst, ok, &[a[0]])?;
            }
            Opcode::LoadTa | Opcode::StoreTa => {
                // An in-bounds element (a detached array has length 0);
                // else `fail`.
                let k = match self.ty(d.args[0]) {
                    MType::Obj(o) => match o.kind {
                        ObjKind::TypedArray(k) => k,
                        _ => return Err("lowering: typed array op without a kind".into()),
                    },
                    _ => return Err("lowering: typed array op without a kind".into()),
                };
                let fail = self.body.add_block();
                let len = self.load_i32(a[0], TA_LENGTH_PAYLOAD_OFFSET);
                let ok = self.bin(Operator::I32LtU, a[1], len, Type::I32);
                self.check(ok, fail);
                let data = self.load_i32(a[0], TA_DATA_PAYLOAD_OFFSET);
                let sh = k.log2_bytes();
                let addr = if sh == 0 {
                    self.bin(Operator::I32Add, data, a[1], Type::I32)
                } else {
                    let s = self.i32c(sh);
                    let off = self.bin(Operator::I32Shl, a[1], s, Type::I32);
                    self.bin(Operator::I32Add, data, off, Type::I32)
                };
                use crate::opsem::TaKind as K;
                let outs = if d.op == Opcode::LoadTa {
                    let m = self.mem(sh, 0);
                    let v = match k {
                        K::Int8 => self.un(Operator::I32Load8S { memory: m }, addr, Type::I32),
                        K::Uint8 | K::Uint8Clamped => {
                            self.un(Operator::I32Load8U { memory: m }, addr, Type::I32)
                        }
                        K::Int16 => self.un(Operator::I32Load16S { memory: m }, addr, Type::I32),
                        K::Uint16 => self.un(Operator::I32Load16U { memory: m }, addr, Type::I32),
                        K::Int32 => self.un(Operator::I32Load { memory: m }, addr, Type::I32),
                        K::Uint32 => return Err("lowering: load_ta of a Uint32Array".into()),
                        K::Float32 | K::Float64 => {
                            let d = if k == K::Float32 {
                                let f = self.un(Operator::F32Load { memory: m }, addr, Type::F32);
                                self.un(Operator::F64PromoteF32, f, Type::F64)
                            } else {
                                self.un(Operator::F64Load { memory: m }, addr, Type::F64)
                            };
                            // Any NaN as the canonical one: a boxed double
                            // must not look like a tag.
                            let nan = self.bin(Operator::F64Ne, d, d, Type::I32);
                            let c = self.f64c(f64::NAN.to_bits());
                            self.select(Type::F64, c, d, nan)
                        }
                    };
                    vec![v]
                } else {
                    let m = self.mem(sh, 0);
                    let v = a[2];
                    match k {
                        K::Int8 | K::Uint8 => {
                            self.op(Operator::I32Store8 { memory: m }, &[addr, v], None);
                        }
                        K::Int16 | K::Uint16 => {
                            self.op(Operator::I32Store16 { memory: m }, &[addr, v], None);
                        }
                        K::Int32 | K::Uint32 => {
                            self.op(Operator::I32Store { memory: m }, &[addr, v], None);
                        }
                        K::Float32 => {
                            let f = self.un(Operator::F32DemoteF64, v, Type::F32);
                            self.op(Operator::F32Store { memory: m }, &[addr, f], None);
                        }
                        K::Float64 => {
                            self.op(Operator::F64Store { memory: m }, &[addr, v], None);
                        }
                        K::Uint8Clamped => {
                            // An int32 clamped to 0..=255.
                            let z = self.i32c(0);
                            let neg = self.bin(Operator::I32LtS, v, z, Type::I32);
                            let lo = self.select(Type::I32, z, v, neg);
                            let hi = self.i32c(255);
                            let big = self.bin(Operator::I32GtS, lo, hi, Type::I32);
                            let c = self.select(Type::I32, hi, lo, big);
                            self.op(Operator::I32Store8 { memory: m }, &[addr, c], None);
                        }
                    }
                    vec![]
                };
                let t = self.edge(inst, 0, &outs)?;
                self.terminate(Terminator::Br { target: t });
                self.cur = fail;
                let f = self.edge(inst, 1, &[])?;
                self.terminate(Terminator::Br { target: f });
            }
            Opcode::LoadElem if self.fusable_unbox(inst).is_some() => {
                let (g, k, p) = self.fusable_unbox(inst).unwrap();
                self.load_elem_unboxed(inst, g, k, p, a[0], a[1])?;
            }
            Opcode::LoadElem => {
                // An in-bounds, non-hole dense element; else `fail`.
                let fail = self.body.add_block();
                let (_, v) = self.elem_addr(a[0], a[1], fail, true);
                let t = self.edge(inst, 0, &[v.unwrap()])?;
                self.terminate(Terminator::Br { target: t });
                self.cur = fail;
                let f = self.edge(inst, 1, &[])?;
                self.terminate(Terminator::Br { target: f });
            }
            Opcode::StoreElem(duty, append) => {
                // An in-bounds overwrite of a non-hole dense element of
                // unfrozen elements (an own writable data property); else
                // `fail`. RANGES is dropped under the store's duty.
                let num =
                    matches!(self.ty(d.args[2]), MType::Val(s) if s.tags.subset_of(TagSet::NUMBER));
                let fail = self.body.add_block();
                let (addr, _) = self.elem_addr(a[0], a[1], fail, true);
                let elements = self.load_i32(a[0], OBJ_ELEMENTS_OFFSET);
                let back = self.i32c(ELEMENTS_FLAGS_BACK);
                let header = self.bin(Operator::I32Sub, elements, back, Type::I32);
                let flags = self.load_i32(header, 0);
                let fz = self.i32c(ELEMENTS_FROZEN_FLAG);
                let frozen = self.bin(Operator::I32And, flags, fz, Type::I32);
                let thawed = self.un(Operator::I32Eqz, frozen, Type::I32);
                self.check(thawed, fail);
                if duty {
                    self.elem_duty(a[0]);
                }
                if !num {
                    self.pre_barrier(addr, 0);
                }
                self.store_i64(addr, 0, a[2]);
                if !num {
                    let f = self.h.post_write_barrier_elem;
                    self.post_barrier(f, a[0], a[1], a[2]);
                }
                let t = self.edge(inst, 0, &[])?;
                self.terminate(Terminator::Br { target: t });
                self.cur = fail;
                // Past the initialized length or into a hole: the append
                // arm, where the prototypes' proof allows (it writes the
                // lengths, and runs no JS); else `fail`.
                if append {
                    let miss = self.body.add_block();
                    let recv = self.box_tagged(TAG_OBJECT, a[0]);
                    let key = self.box_tagged(TAG_INT32, a[1]);
                    self.elem_append_arm(inst, recv, key, a[2], duty, num, miss)?;
                    self.cur = miss;
                    // What the arm refused (growth, a row not yet cached):
                    // the runtime's append, which runs no JS.
                    if duty {
                        self.elem_duty(a[0]);
                    }
                    let live = self.live_across(inst);
                    let (ok, _) = self.gc_call(self.h.elem_grow, &[recv, a[1], a[2]], &live)?;
                    self.epoch_same = None;
                    let t = self.edge(inst, 0, &[])?;
                    let f = self.edge(inst, 1, &[])?;
                    self.cond_br(ok, t, f);
                    return Ok(true);
                }
                let f = self.edge(inst, 1, &[])?;
                self.terminate(Terminator::Br { target: f });
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
