//! elements: methods of `Lower`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    /// `load_elem` whose `ok` block begins with `guard.unbox kind` of the
    /// value (`fusable_unbox`): the element's tag is tested for `kind`
    /// first, and a match takes the guard's `ok` edge with the payload
    /// directly (a hole's tag is never `kind`'s). Any other tag goes the
    /// unfused way: a hole to `fail`, the rest to the guard's block, whose
    /// guard then misses as it would have.
    pub(super) fn load_elem_unboxed(
        &mut self,
        inst: mir::Inst,
        g: mir::Inst,
        kind: UnboxKind,
        p: mir::Value,
        obj: Value,
        idx: Value,
    ) -> R<()> {
        let fail = self.body.add_block();
        let (addr, _) = self.elem_addr(obj, idx, fail, false);
        let v = self.load_i64(addr, 0);
        let tag = self.tag_of(v);
        // The guard's edges see the loaded value as its block's param.
        self.vmap.insert(p, v);
        let other = self.body.add_block();
        if kind == UnboxKind::F64Num {
            let have = value_tags(&self.ty(p));
            // Its `fail` is `other` here: the hole test, then the guard.
            let join = self.body.add_block();
            let x = self.body.add_blockparam(join, Type::F64);
            let (d_b, nd_b, i_b) = (
                self.body.add_block(),
                self.body.add_block(),
                self.body.add_block(),
            );
            let k = self.i32c(TAG_INT32 as u32);
            let is_dbl = self.bin(Operator::I32LtU, tag, k, Type::I32);
            self.cond_br(is_dbl, Self::to(d_b), Self::to(nd_b));
            self.cur = d_b;
            let fd = self.un(Operator::F64ReinterpretI64, v, Type::F64);
            self.terminate(Terminator::Br {
                target: BlockTarget {
                    block: join,
                    args: vec![fd],
                },
            });
            self.cur = nd_b;
            let is_int = self.tag_is(tag, TAG_INT32 as u32);
            let may_int = !have.intersect(TagSet::INT32).is_empty();
            if may_int {
                self.cond_br(is_int, Self::to(i_b), Self::to(other));
            } else {
                self.terminate(Terminator::Br {
                    target: Self::to(other),
                });
            }
            self.cur = i_b;
            let low = self.un(Operator::I32WrapI64, v, Type::I32);
            let fi = self.un(Operator::F64ConvertI32S, low, Type::F64);
            self.terminate(Terminator::Br {
                target: BlockTarget {
                    block: join,
                    args: vec![fi],
                },
            });
            self.cur = join;
            let t = self.edge(g, 0, &[x])?;
            self.terminate(Terminator::Br { target: t });
        } else {
            let want = match kind {
                UnboxKind::I32 => TAG_INT32,
                UnboxKind::Bool => TAG_BOOLEAN,
                UnboxKind::Obj => TAG_OBJECT,
                UnboxKind::Str => TAG_STRING,
                UnboxKind::F64Num => unreachable!(),
            };
            let hit = self.body.add_block();
            let is_k = self.tag_is(tag, want as u32);
            self.cond_br(is_k, Self::to(hit), Self::to(other));
            self.cur = hit;
            let payload = self.un(Operator::I32WrapI64, v, Type::I32);
            let t = self.edge(g, 0, &[payload])?;
            self.terminate(Terminator::Br { target: t });
        }
        self.cur = other;
        let hole = self.tag_is(tag, TAG_MAGIC as u32);
        let t = self.edge(inst, 0, &[v])?;
        let f_hole = self.body.add_block();
        self.cond_br(hole, Self::to(f_hole), t);
        self.cur = f_hole;
        self.terminate(Terminator::Br {
            target: Self::to(fail),
        });
        self.cur = fail;
        let f = self.edge(inst, 1, &[])?;
        self.terminate(Terminator::Br { target: f });
        Ok(())
    }

    /// `obj[key] = val` adding `key`, an atom (`props[name] = config[name]`
    /// over a for-in's keys): the transition the runtime's set learned for
    /// (the object's shape, the atom), from the element-add table, replayed
    /// as a named add site replays its own row (`set_ic_trans`, with no
    /// slot predictions for an unknown name). A key that is no atom, or
    /// an index, never has a row. Else `other`.
    pub(super) fn elem_add_arm(
        &mut self,
        inst: mir::Inst,
        recv: Value,
        key: Value,
        val: Value,
        other: Block,
    ) -> R<()> {
        use crate::region_shape::{
            ELEM_ADD_KEY_OFF, ELEM_ADD_ROWS, ELEM_ADD_ROW_BYTES, STRLIT_ELEM_ADD_OFF,
        };
        let kt = self.tag_of(key);
        let ks = self.tag_is(kt, TAG_STRING as u32);
        let rt = self.tag_of(recv);
        let ro = self.tag_is(rt, TAG_OBJECT as u32);
        let both = self.bin(Operator::I32And, ks, ro, Type::I32);
        self.check(both, other);
        let kp = self.un(Operator::I32WrapI64, key, Type::I32);
        let one = self.i32c(1);
        let ekey = self.bin(Operator::I32Or, kp, one, Type::I32);
        let op = self.un(Operator::I32WrapI64, recv, Type::I32);
        let shape = self.load_i32(op, SHAPE_OFFSET);
        // NightRuntime.cpp's CacheHash.
        let three = self.i32c(3);
        let sh = self.bin(Operator::I32ShrU, shape, three, Type::I32);
        let k1 = self.i32c(2654435761);
        let h1 = self.bin(Operator::I32Mul, sh, k1, Type::I32);
        let k2c = self.i32c(0x9e37_79b9);
        let k2 = self.bin(Operator::I32Mul, ekey, k2c, Type::I32);
        let hh = self.bin(Operator::I32Xor, h1, k2, Type::I32);
        let mask = self.i32c(ELEM_ADD_ROWS - 1);
        let idx = self.bin(Operator::I32And, hh, mask, Type::I32);
        let stride = self.i32c(ELEM_ADD_ROW_BYTES);
        let off = self.bin(Operator::I32Mul, idx, stride, Type::I32);
        let ta = self.i32c(self.h.strlit_slot + STRLIT_ELEM_ADD_OFF);
        let tbl = self.load_i32(ta, 0);
        let row = self.bin(Operator::I32Add, tbl, off, Type::I32);
        let rkey = self.load_i32(row, ELEM_ADD_KEY_OFF);
        let same = self.bin(Operator::I32Eq, rkey, ekey, Type::I32);
        self.check(same, other);
        let num = matches!(self.ty(self.f.insts[inst].args[2]), MType::Val(s) if s.tags.subset_of(TagSet::NUMBER));
        self.set_ic_trans(inst, &[], recv, val, num, row, other)
    }

    /// Element `key` of `recv` (both boxed) when `recv` is a native object,
    /// `key` an int32, and the element an initialized, non-hole dense one;
    /// else branches to `fail`. Pure reads: a typed array's dense
    /// initializedLength is 0, and a proxy fails the native check first.
    /// Element `key` (a boxed int32) of `recv` (boxed), an arguments
    /// object whose element is neither overridden nor forwarded to a call
    /// object (as `ArgumentsObject::element`): its data's slot;
    /// anything else branches to `fail`.
    pub(super) fn args_element(&mut self, recv: Value, key: Value, fail: Block) -> Value {
        let tag = self.tag_of(recv);
        let is_obj = self.tag_is(tag, TAG_OBJECT as u32);
        let ktag = self.tag_of(key);
        let is_int = self.tag_is(ktag, TAG_INT32 as u32);
        let both = self.bin(Operator::I32And, is_obj, is_int, Type::I32);
        self.check(both, fail);
        let obj = self.un(Operator::I32WrapI64, recv, Type::I32);
        let shape = self.load_i32(obj, SHAPE_OFFSET);
        let base = self.load_i32(shape, SHAPE_BASESHAPE_OFFSET);
        let clasp = self.load_i32(base, BASESHAPE_CLASP_OFFSET);
        let acbase = self.i32c(self.h.args_class_base);
        let mapped = self.load_i32(acbase, 0);
        let unmapped = self.load_i32(acbase, 4);
        let m = self.bin(Operator::I32Eq, clasp, mapped, Type::I32);
        let u = self.bin(Operator::I32Eq, clasp, unmapped, Type::I32);
        let is_args = self.bin(Operator::I32Or, m, u, Type::I32);
        self.check(is_args, fail);
        // Packed in fixed slot 0: the length above bit 5; bit 2, an
        // element overridden.
        let packed = self.load_i32(obj, FIXED_SLOTS_BASE);
        let four = self.i32c(4);
        let over = self.bin(Operator::I32And, packed, four, Type::I32);
        let kept = self.un(Operator::I32Eqz, over, Type::I32);
        let five = self.i32c(5);
        let len = self.bin(Operator::I32ShrU, packed, five, Type::I32);
        let idx = self.un(Operator::I32WrapI64, key, Type::I32);
        let inb = self.bin(Operator::I32LtU, idx, len, Type::I32);
        let ok = self.bin(Operator::I32And, kept, inb, Type::I32);
        self.check(ok, fail);
        // The data (fixed slot 1's private pointer), its args at the
        // engine's offset.
        let data = self.load_i32(obj, FIXED_SLOTS_BASE + 8);
        let eoff = self.load_i32(acbase, 8);
        let elems = self.bin(Operator::I32Add, data, eoff, Type::I32);
        let eight = self.i32c(8);
        let off = self.bin(Operator::I32Mul, idx, eight, Type::I32);
        let addr = self.bin(Operator::I32Add, elems, off, Type::I32);
        let v = self.load_i64(addr, 0);
        // A forwarded (mapped, aliased) element is magic.
        let vt = self.tag_of(v);
        let magic = self.tag_is(vt, TAG_MAGIC as u32);
        let plain = self.un(Operator::I32Eqz, magic, Type::I32);
        self.check(plain, fail);
        v
    }

    pub(super) fn dense_element(&mut self, recv: Value, key: Value, fail: Block) -> Value {
        self.dense_slot(recv, key, fail).4
    }

    /// `dense_element`'s checks, returning the object, its elements, the
    /// index, the element's address and its value.
    /// Element `idx` of native object `obj`: its address, branching to
    /// `fail` unless in bounds (and, with `load`, the value, unless a hole).
    pub(super) fn elem_addr(
        &mut self,
        obj: Value,
        idx: Value,
        fail: Block,
        load: bool,
    ) -> (Value, Option<Value>) {
        let elements = self.load_i32(obj, OBJ_ELEMENTS_OFFSET);
        let back = self.i32c(ELEMENTS_INITLEN_BACK);
        let header = self.bin(Operator::I32Sub, elements, back, Type::I32);
        let initlen = self.load_i32(header, 0);
        let in_bounds = self.bin(Operator::I32LtU, idx, initlen, Type::I32);
        self.check(in_bounds, fail);
        let eight = self.i32c(8);
        let off = self.bin(Operator::I32Mul, idx, eight, Type::I32);
        let addr = self.bin(Operator::I32Add, elements, off, Type::I32);
        if !load {
            return (addr, None);
        }
        let v = self.load_i64(addr, 0);
        let vtag = self.tag_of(v);
        let hole = self.tag_is(vtag, TAG_MAGIC as u32);
        let not_hole = self.un(Operator::I32Eqz, hole, Type::I32);
        self.check(not_hole, fail);
        (addr, Some(v))
    }

    pub(super) fn dense_slot(
        &mut self,
        recv: Value,
        key: Value,
        fail: Block,
    ) -> (Value, Value, Value, Value, Value) {
        let tag = self.tag_of(recv);
        let is_obj = self.tag_is(tag, TAG_OBJECT as u32);
        let ktag = self.tag_of(key);
        let is_int = self.tag_is(ktag, TAG_INT32 as u32);
        let both = self.bin(Operator::I32And, is_obj, is_int, Type::I32);
        self.check(both, fail);
        let obj = self.un(Operator::I32WrapI64, recv, Type::I32);
        let shape = self.load_i32(obj, SHAPE_OFFSET);
        let flags = self.load_i32(shape, SHAPE_IMMUTABLE_FLAGS_OFFSET);
        let bit = self.i32c(SHAPE_IS_NATIVE_BIT);
        let native = self.bin(Operator::I32And, flags, bit, Type::I32);
        self.check(native, fail);
        let elements = self.load_i32(obj, OBJ_ELEMENTS_OFFSET);
        let back = self.i32c(ELEMENTS_INITLEN_BACK);
        let header = self.bin(Operator::I32Sub, elements, back, Type::I32);
        let initlen = self.load_i32(header, 0);
        let idx = self.un(Operator::I32WrapI64, key, Type::I32);
        let in_bounds = self.bin(Operator::I32LtU, idx, initlen, Type::I32);
        self.check(in_bounds, fail);
        let eight = self.i32c(8);
        let off = self.bin(Operator::I32Mul, idx, eight, Type::I32);
        let addr = self.bin(Operator::I32Add, elements, off, Type::I32);
        let v = self.load_i64(addr, 0);
        let vtag = self.tag_of(v);
        let hole = self.tag_is(vtag, TAG_MAGIC as u32);
        let not_hole = self.un(Operator::I32Eqz, hole, Type::I32);
        self.check(not_hole, fail);
        (obj, elements, idx, addr, v)
    }

    /// A dense append, or a store into a hole, call-free: with
    /// `night_elem_append_check` proving it
    /// legal (the receiver's shape in the append cache, its prototypes
    /// holding no indexed property, room left) and returning the element's
    /// address, store it and, for an append, bump the initialized length,
    /// and an Array's length when the index passes it; taking `ok_clean`.
    /// Else `miss`.
    pub(super) fn elem_append_arm(
        &mut self,
        inst: mir::Inst,
        recv: Value,
        key: Value,
        val: Value,
        duty: bool,
        num: bool,
        miss: Block,
    ) -> R<()> {
        let tag = self.tag_of(recv);
        let is_obj = self.tag_is(tag, TAG_OBJECT as u32);
        let ktag = self.tag_of(key);
        let is_int = self.tag_is(ktag, TAG_INT32 as u32);
        let both = self.bin(Operator::I32And, is_obj, is_int, Type::I32);
        self.check(both, miss);
        let obj = self.un(Operator::I32WrapI64, recv, Type::I32);
        let elements = self.load_i32(obj, OBJ_ELEMENTS_OFFSET);
        let initlen = self.elem_header(elements, ELEMENTS_INITLEN_BACK);
        let idx = self.un(Operator::I32WrapI64, key, Type::I32);
        let packed = self.call1(
            self.h.elem_append_check,
            &[obj, elements, initlen, idx],
            Type::I64,
        );
        let z = self.i64c(0);
        let ok = self.bin(Operator::I64Ne, packed, z, Type::I32);
        self.check(ok, miss);
        let addr = self.un(Operator::I32WrapI64, packed, Type::I32);
        let sh = self.i64c(32);
        let row = self.bin(Operator::I64ShrU, packed, sh, Type::I64);
        let row = self.un(Operator::I32WrapI64, row, Type::I32);
        if duty {
            self.elem_duty(obj);
        }
        // A hole or the space past the initialized length holds no GC
        // thing: no pre-barrier.
        self.store_i64(addr, 0, val);
        let is_app = self.bin(Operator::I32Eq, idx, initlen, Type::I32);
        let (bump, done) = (self.body.add_block(), self.body.add_block());
        self.cond_br(is_app, Self::to(bump), Self::to(done));
        self.cur = bump;
        let one = self.i32c(1);
        let n = self.bin(Operator::I32Add, idx, one, Type::I32);
        self.set_elem_header(elements, ELEMENTS_INITLEN_BACK, n);
        let len = self.elem_header(elements, ELEMENTS_LENGTH_BACK);
        let passes = self.bin(Operator::I32GeU, idx, len, Type::I32);
        // The length word is an Array's (the row's isArray, from its prime).
        let is_arr = self.load_i32(row, 20);
        let both = self.bin(Operator::I32And, passes, is_arr, Type::I32);
        let lb = self.body.add_block();
        self.cond_br(both, Self::to(lb), Self::to(done));
        self.cur = lb;
        self.set_elem_header(elements, ELEMENTS_LENGTH_BACK, n);
        self.terminate(Terminator::Br {
            target: Self::to(done),
        });
        self.cur = done;
        if !num {
            let f = self.h.post_write_barrier_elem;
            self.post_barrier(f, obj, idx, val);
        }
        let t = self.edge(inst, 0, &[])?;
        self.terminate(Terminator::Br { target: t });
        Ok(())
    }

    /// Whether `inst` is at a polymorphic typed-array site.
    pub(super) fn ta_poly(&self, inst: mir::Inst) -> bool {
        self.f.insts[inst]
            .attach
            .is_some_and(|a| self.f.attachments[a].ta_poly)
    }

    /// Boxed `recv` as an object pointer and `key` as an int32, else
    /// `miss`.
    pub(super) fn obj_int(&mut self, recv: Value, key: Value, miss: Block) -> (Value, Value) {
        let tag = self.tag_of(recv);
        let is_obj = self.tag_is(tag, TAG_OBJECT as u32);
        let ktag = self.tag_of(key);
        let is_int = self.tag_is(ktag, TAG_INT32 as u32);
        let both = self.bin(Operator::I32And, is_obj, is_int, Type::I32);
        self.check(both, miss);
        let obj = self.un(Operator::I32WrapI64, recv, Type::I32);
        let idx = self.un(Operator::I32WrapI64, key, Type::I32);
        (obj, idx)
    }

    /// `InitElemArray index` filling an array literal:
    /// with a value that is not the hole, dense elements with no flag but
    /// FIXED, and the initialized length at `index` with room, store (with
    /// the post-barrier a tenured literal owes a nursery value) and bump it
    /// (clearing RANGES under the store's `duty`), taking `ok_clean`;
    /// `cur` is left on the miss.
    pub(super) fn init_elem_inline(
        &mut self,
        inst: mir::Inst,
        arr: Value,
        index: u32,
        val: Value,
        duty: bool,
    ) -> R<()> {
        use crate::wasm::mir::abi::ELEMENTS_FLAG_FIXED;
        let slow = self.body.add_block();
        let vt = self.tag_of(val);
        let magic = self.i32c(TAG_MAGIC as u32);
        let v_ok = self.bin(Operator::I32Ne, vt, magic, Type::I32);
        let at = self.tag_of(arr);
        let a_obj = self.tag_is(at, TAG_OBJECT as u32);
        let ok = self.bin(Operator::I32And, v_ok, a_obj, Type::I32);
        self.check(ok, slow);
        let obj = self.un(Operator::I32WrapI64, arr, Type::I32);
        let elems = self.load_i32(obj, OBJ_ELEMENTS_OFFSET);
        let flags = self.elem_header(elems, ELEMENTS_FLAGS_BACK);
        let initlen = self.elem_header(elems, ELEMENTS_INITLEN_BACK);
        let cap = self.elem_header(elems, ELEMENTS_CAPACITY_BACK);
        let len = self.elem_header(elems, ELEMENTS_LENGTH_BACK);
        let nf = self.i32c(!ELEMENTS_FLAG_FIXED);
        let rest = self.bin(Operator::I32And, flags, nf, Type::I32);
        let f_ok = self.un(Operator::I32Eqz, rest, Type::I32);
        let iv = self.i32c(index);
        let i_ok = self.bin(Operator::I32Eq, initlen, iv, Type::I32);
        let c_ok = self.bin(Operator::I32GtU, cap, iv, Type::I32);
        let l_ok = self.bin(Operator::I32GtU, len, iv, Type::I32);
        let a = self.bin(Operator::I32And, f_ok, i_ok, Type::I32);
        let b = self.bin(Operator::I32And, c_ok, l_ok, Type::I32);
        let all = self.bin(Operator::I32And, a, b, Type::I32);
        self.check(all, slow);
        if duty {
            self.elem_duty(obj);
        }
        self.store_i64(elems, index * 8, val);
        let nl = self.i32c(index + 1);
        self.set_elem_header(elems, ELEMENTS_INITLEN_BACK, nl);
        if !value_tags(&self.ty(self.f.insts[inst].args[2])).subset_of(GC_FREE_TAGS) {
            let f = self.h.post_write_barrier_elem;
            self.post_barrier(f, obj, iv, val);
        }
        let t = self.edge(inst, 0, &[])?;
        self.terminate(Terminator::Br { target: t });
        self.cur = slow;
        Ok(())
    }

    /// `Array.prototype.push(v, ...)` on a dense array, call-free: with
    /// the elements packed, room left for every argument, and the
    /// receiver's shape in the append cache with its protos' shapes
    /// unchanged (no indexed property can appear on them), store them at
    /// the end and bump the initialized length and `length`, taking
    /// `ok_clean` with the new length; else `other`.
    pub(super) fn push_arm(&mut self, inst: mir::Inst, ops: &[Value], other: Block) -> R<()> {
        let (callee, this, args) = (ops[0], ops[1], &ops[2..]);
        let n = u32::try_from(args.len()).unwrap();
        let is_push = self.builtin_is(callee, BC_ARR_PUSH);
        let ttag = self.tag_of(this);
        let this_obj = self.tag_is(ttag, TAG_OBJECT as u32);
        let m = self.bin(Operator::I32And, is_push, this_obj, Type::I32);
        self.check(m, other);
        let obj = self.un(Operator::I32WrapI64, this, Type::I32);
        let elements = self.load_i32(obj, OBJ_ELEMENTS_OFFSET);
        let flags = self.elem_header(elements, ELEMENTS_FLAGS_BACK);
        let initlen = self.elem_header(elements, ELEMENTS_INITLEN_BACK);
        let cap = self.elem_header(elements, ELEMENTS_CAPACITY_BACK);
        let len = self.elem_header(elements, ELEMENTS_LENGTH_BACK);
        let bm = self.i32c(ELEMENTS_PUSH_BAIL_MASK);
        let bail = self.bin(Operator::I32And, flags, bm, Type::I32);
        let flags_ok = self.un(Operator::I32Eqz, bail, Type::I32);
        let len_eq = self.bin(Operator::I32Eq, len, initlen, Type::I32);
        // `cap - initlen >= n` (initlen <= cap always).
        let room = self.bin(Operator::I32Sub, cap, initlen, Type::I32);
        let nv = self.i32c(n);
        let has_cap = self.bin(Operator::I32GeU, room, nv, Type::I32);
        let imax = self.i32c(0x7FFF_FFFF - n);
        let fits = self.bin(Operator::I32LeU, len, imax, Type::I32);
        let a = self.bin(Operator::I32And, flags_ok, len_eq, Type::I32);
        let b = self.bin(Operator::I32And, has_cap, fits, Type::I32);
        let ok = self.bin(Operator::I32And, a, b, Type::I32);
        self.check(ok, other);
        let (row, hit) = self.append_row(obj);
        self.check(hit, other);
        let arr = self.load_i32(row, 20);
        self.check(arr, other);
        for k in 0..2 {
            let p = self.load_i32(row, 4 + 8 * k);
            let s = self.load_i32(row, 8 + 8 * k);
            // A null proto's row word is 0; its shape load reads the
            // (mapped) zero page's word, which the `or` discards.
            let live = self.load_i32(p, SHAPE_OFFSET);
            let empty = self.un(Operator::I32Eqz, p, Type::I32);
            let same = self.bin(Operator::I32Eq, live, s, Type::I32);
            let okp = self.bin(Operator::I32Or, empty, same, Type::I32);
            self.check(okp, other);
        }
        let three = self.i32c(3);
        let off = self.bin(Operator::I32Shl, initlen, three, Type::I32);
        let addr = self.bin(Operator::I32Add, elements, off, Type::I32);
        // A stamped array's element claims (RANGES, TYPES) hold for a value
        // no site proves here: dropped (the array stamp's store duty).
        self.elem_duty(obj);
        for (k, &arg) in args.iter().enumerate() {
            self.store_i64(addr, 8 * u32::try_from(k).unwrap(), arg);
        }
        let newlen = self.bin(Operator::I32Add, initlen, nv, Type::I32);
        self.set_elem_header(elements, ELEMENTS_INITLEN_BACK, newlen);
        self.set_elem_header(elements, ELEMENTS_LENGTH_BACK, newlen);
        let f = self.h.post_write_barrier_elem;
        for (k, &arg) in args.iter().enumerate() {
            let kv = self.i32c(u32::try_from(k).unwrap());
            let idx = self.bin(Operator::I32Add, initlen, kv, Type::I32);
            self.post_barrier(f, obj, idx, arg);
        }
        let r = self.box_tagged(TAG_INT32, newlen);
        let t = self.edge(inst, 0, &[r])?;
        self.terminate(Terminator::Br { target: t });
        Ok(())
    }

    /// The append-cache row for `obj`'s shape, and whether it is that
    /// shape's (the row's first word).
    pub(super) fn append_row(&mut self, obj: Value) -> (Value, Value) {
        let shape = self.load_i32(obj, SHAPE_OFFSET);
        let three = self.i32c(3);
        let sh = self.bin(Operator::I32ShrU, shape, three, Type::I32);
        let k1 = self.i32c(2654435761);
        let h = self.bin(Operator::I32Mul, sh, k1, Type::I32);
        let mask = self.i32c(APPEND_CACHE_SIZE - 1);
        let ridx = self.bin(Operator::I32And, h, mask, Type::I32);
        let stride = self.i32c(APPEND_CACHE_ENTRY_BYTES);
        let roff = self.bin(Operator::I32Mul, ridx, stride, Type::I32);
        let base = self.i32c(self.h.append_cache_base);
        let row = self.bin(Operator::I32Add, base, roff, Type::I32);
        let rshape = self.load_i32(row, 0);
        let hit = self.bin(Operator::I32Eq, shape, rshape, Type::I32);
        (row, hit)
    }

    /// The address of the elements header word `back` bytes below
    /// `elements`.
    pub(super) fn elem_header_addr(&mut self, elements: Value, back: u32) -> Value {
        let k = self.i32c(back);
        self.bin(Operator::I32Sub, elements, k, Type::I32)
    }

    pub(super) fn elem_header(&mut self, elements: Value, back: u32) -> Value {
        let a = self.elem_header_addr(elements, back);
        self.load_i32(a, 0)
    }

    pub(super) fn set_elem_header(&mut self, elements: Value, back: u32, v: Value) {
        let a = self.elem_header_addr(elements, back);
        self.store_i32(a, 0, v);
    }

    /// `Array.prototype.pop()` on a dense array: with the
    /// elements packed and non-empty, no incremental marking (the dropped
    /// element would lose its barrier), and no hole at the end, shrink by
    /// one, taking `ok_clean` with the last element; else `other`.
    pub(super) fn pop_arm(&mut self, inst: mir::Inst, ops: &[Value], other: Block) -> R<()> {
        let (callee, this) = (ops[0], ops[1]);
        let is_pop = self.builtin_is(callee, BC_ARR_POP);
        let ttag = self.tag_of(this);
        let this_obj = self.tag_is(ttag, TAG_OBJECT as u32);
        let m = self.bin(Operator::I32And, is_pop, this_obj, Type::I32);
        self.check(m, other);
        let obj = self.un(Operator::I32WrapI64, this, Type::I32);
        let shape = self.load_i32(obj, SHAPE_OFFSET);
        let base = self.load_i32(shape, SHAPE_BASESHAPE_OFFSET);
        let clasp = self.load_i32(base, BASESHAPE_CLASP_OFFSET);
        let aslot = self.i32c(self.h.array_class_slot);
        let arr_class = self.load_i32(aslot, 0);
        let is_arr = self.bin(Operator::I32Eq, clasp, arr_class, Type::I32);
        self.check(is_arr, other);
        let elements = self.load_i32(obj, OBJ_ELEMENTS_OFFSET);
        let flags = self.elem_header(elements, ELEMENTS_FLAGS_BACK);
        let initlen = self.elem_header(elements, ELEMENTS_INITLEN_BACK);
        let len = self.elem_header(elements, ELEMENTS_LENGTH_BACK);
        let bm = self.i32c(ELEMENTS_POP_BAIL_MASK);
        let bail = self.bin(Operator::I32And, flags, bm, Type::I32);
        let flags_ok = self.un(Operator::I32Eqz, bail, Type::I32);
        let len_eq = self.bin(Operator::I32Eq, len, initlen, Type::I32);
        let z = self.i32c(0);
        let nonempty = self.bin(Operator::I32Ne, len, z, Type::I32);
        let zone = self.load_i32(self.cx, JSCONTEXT_ZONE_OFFSET);
        let needs = self.load_i32(zone, ZONE_NEEDS_BARRIER_OFFSET);
        let no_barrier = self.un(Operator::I32Eqz, needs, Type::I32);
        let a = self.bin(Operator::I32And, flags_ok, len_eq, Type::I32);
        let b = self.bin(Operator::I32And, nonempty, no_barrier, Type::I32);
        let ok = self.bin(Operator::I32And, a, b, Type::I32);
        self.check(ok, other);
        let one = self.i32c(1);
        let newlen = self.bin(Operator::I32Sub, len, one, Type::I32);
        let three = self.i32c(3);
        let off = self.bin(Operator::I32Shl, newlen, three, Type::I32);
        let addr = self.bin(Operator::I32Add, elements, off, Type::I32);
        let elem = self.load_i64(addr, 0);
        let etag = self.tag_of(elem);
        let hole = self.tag_is(etag, TAG_MAGIC as u32);
        let not_hole = self.un(Operator::I32Eqz, hole, Type::I32);
        self.check(not_hole, other);
        self.set_elem_header(elements, ELEMENTS_INITLEN_BACK, newlen);
        self.set_elem_header(elements, ELEMENTS_LENGTH_BACK, newlen);
        let t = self.edge(inst, 0, &[elem])?;
        self.terminate(Terminator::Br { target: t });
        Ok(())
    }
}
