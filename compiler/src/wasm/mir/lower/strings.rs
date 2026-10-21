//! strings: methods of `Lower`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    /// `a + b` of two strings, where both operands' types admit a string:
    /// `rope_concat`'s inline arms, then the quiet concat helper (it runs
    /// no user code and touches no stamp, so it takes `ok_clean`).
    pub(super) fn concat_arm(
        &mut self,
        inst: mir::Inst,
        d: &mir::func::InstData,
        a: &[Value],
    ) -> R<()> {
        let s = TagSet::prims(PRIM_STRING);
        if value_tags(&self.ty(d.args[0])).intersect(s).is_empty()
            || value_tags(&self.ty(d.args[1])).intersect(s).is_empty()
        {
            return Ok(());
        }
        let (x, y) = (a[0], a[1]);
        let tx = self.tag_of(x);
        let sx = self.tag_is(tx, TAG_STRING as u32);
        let ty = self.tag_of(y);
        let sy = self.tag_is(ty, TAG_STRING as u32);
        let both = self.bin(Operator::I32And, sx, sy, Type::I32);
        let (cat, other) = (self.body.add_block(), self.body.add_block());
        self.cond_br(both, Self::to(cat), Self::to(other));
        self.cur = cat;
        let helper = self.body.add_block();
        self.rope_concat(inst, x, y, helper)?;
        self.cur = helper;
        // The helper's reload rebinds the live values on its path only.
        let saved = (
            self.vmap.clone(),
            self.slotted.clone(),
            self.dirty.clone(),
            self.cold,
        );
        let live = self.live_across(inst);
        let (ok, r) = self.gc_call(self.h.concat, &[x, y], &live)?;
        let t = self.edge(inst, 0, &[r])?;
        let e = self.edge(inst, 2, &[])?;
        self.cond_br(ok, t, e);
        (self.vmap, self.slotted, self.dirty, self.cold) = saved;
        self.epoch_same = None;
        self.cur = other;
        Ok(())
    }

    /// `typeof x == T` (`!=` with bit 7 of `k`) inline. The tag decides
    /// every type but an object's, and a primitive type is decided by it
    /// alone. An object's class decides the rest: the function classes are
    /// "function"; a class without a call hook that is neither a proxy nor
    /// emulates undefined is "object". Any other object asks the helper (a
    /// leaf: no GC, no JS).
    pub(super) fn typeof_eq(&mut self, x: Value, k: u8) -> Value {
        const JSTYPE_UNDEFINED: u8 = 0;
        const JSTYPE_OBJECT: u8 = 1;
        const JSTYPE_FUNCTION: u8 = 2;
        let (ty, neg) = (k & 0x7f, k & 0x80 != 0);
        let prims = match ty {
            JSTYPE_UNDEFINED => PRIM_UNDEFINED,
            JSTYPE_OBJECT => PRIM_NULL,
            JSTYPE_FUNCTION => Prims::EMPTY,
            3 => PRIM_STRING,
            4 => PRIM_INT32 | PRIM_DOUBLE,
            5 => PRIM_BOOLEAN,
            6 => PRIM_SYMBOL,
            _ => PRIM_BIGINT,
        };
        let negate = |l: &mut Self, v: Value| {
            if neg {
                l.un(Operator::I32Eqz, v, Type::I32)
            } else {
                v
            }
        };
        let prim = self.has_tags(x, TagSet::prims(prims));
        if !matches!(ty, JSTYPE_UNDEFINED | JSTYPE_OBJECT | JSTYPE_FUNCTION) {
            return negate(self, prim);
        }
        let join = self.body.add_block();
        let r = self.body.add_blockparam(join, Type::I32);
        let (obj_b, prim_b) = (self.body.add_block(), self.body.add_block());
        let tag = self.tag_of(x);
        let is_obj = self.tag_is(tag, TAG_OBJECT as u32);
        self.cond_br(is_obj, Self::to(obj_b), Self::to(prim_b));
        self.cur = prim_b;
        let pv = negate(self, prim);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: join,
                args: vec![pv],
            },
        });
        self.cur = obj_b;
        let obj = self.un(Operator::I32WrapI64, x, Type::I32);
        let shape = self.load_i32(obj, SHAPE_OFFSET);
        let base = self.load_i32(shape, SHAPE_BASESHAPE_OFFSET);
        let clasp = self.load_i32(base, BASESHAPE_CLASP_OFFSET);
        let slot = self.i32c(self.h.fn_class_slot);
        let fn_class = self.load_i32(slot, 0);
        let ext_class = self.load_i32(slot, 4);
        let is_fn = self.bin(Operator::I32Eq, clasp, fn_class, Type::I32);
        let is_ext = self.bin(Operator::I32Eq, clasp, ext_class, Type::I32);
        let is_fn = self.bin(Operator::I32Or, is_fn, is_ext, Type::I32);
        let (fun_b, other_b, helper_b) = (
            self.body.add_block(),
            self.body.add_block(),
            self.body.add_block(),
        );
        self.cond_br(is_fn, Self::to(fun_b), Self::to(other_b));
        self.cur = fun_b;
        let fv = self.i32c(u32::from((ty == JSTYPE_FUNCTION) != neg));
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: join,
                args: vec![fv],
            },
        });
        self.cur = other_b;
        let flags = self.load_i32(clasp, JSCLASS_FLAGS_OFFSET);
        let odd = self.i32c(JSCLASS_IS_PROXY | JSCLASS_EMULATES_UNDEFINED);
        let odd = self.bin(Operator::I32And, flags, odd, Type::I32);
        let plain = self.un(Operator::I32Eqz, odd, Type::I32);
        self.check(plain, helper_b);
        let cops = self.load_i32(clasp, JSCLASS_COPS_OFFSET);
        let (ops_b, obj_t) = (self.body.add_block(), self.body.add_block());
        self.cond_br(cops, Self::to(ops_b), Self::to(obj_t));
        self.cur = ops_b;
        let call = self.load_i32(cops, JSCLASSOPS_CALL_OFFSET);
        let uncallable = self.un(Operator::I32Eqz, call, Type::I32);
        self.cond_br(uncallable, Self::to(obj_t), Self::to(helper_b));
        self.cur = obj_t;
        let ov = self.i32c(u32::from((ty == JSTYPE_OBJECT) != neg));
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: join,
                args: vec![ov],
            },
        });
        self.cur = helper_b;
        let kv = self.i32c(u32::from(k));
        let hv = self.call1(self.h.typeof_eq, &[self.cx, x, kv], Type::I32);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: join,
                args: vec![hv],
            },
        });
        self.cur = join;
        r
    }

    /// The concat of strings `x` and `y` inline, as the JIT's concat stub
    /// does, taking edge 0: either half when the other is empty, else a
    /// nursery rope over the two. A result short enough to be an inline
    /// string (which the engine flattens rather than ropes), one past the
    /// maximum length, a zone that does not allocate strings in the nursery
    /// (the runtime's header word is 0) and a full nursery take `helper`.
    /// No GC, no user code: nothing live needs rooting.
    pub(super) fn rope_concat(
        &mut self,
        inst: mir::Inst,
        x: Value,
        y: Value,
        helper: Block,
    ) -> R<()> {
        let (xp, yp) = (
            self.un(Operator::I32WrapI64, x, Type::I32),
            self.un(Operator::I32WrapI64, y, Type::I32),
        );
        let z = self.i32c(0);
        let lx = self.load_i32(xp, STRING_LENGTH_OFFSET);
        let ly = self.load_i32(yp, STRING_LENGTH_OFFSET);
        for (len, other) in [(lx, y), (ly, x)] {
            let empty = self.bin(Operator::I32Eq, len, z, Type::I32);
            let (take, next) = (self.body.add_block(), self.body.add_block());
            self.cond_br(empty, Self::to(take), Self::to(next));
            self.cur = take;
            let t = self.edge(inst, 0, &[other])?;
            self.terminate(Terminator::Br { target: t });
            self.cur = next;
        }
        let total = self.bin(Operator::I32Add, lx, ly, Type::I32);
        let fx = self.load_i32(xp, STRING_FLAGS_OFFSET);
        let fy = self.load_i32(yp, STRING_FLAGS_OFFSET);
        let both = self.bin(Operator::I32And, fx, fy, Type::I32);
        let l1 = self.i32c(STRING_LATIN1_CHARS_BIT);
        let flags = self.bin(Operator::I32And, both, l1, Type::I32);
        let (fl, ft) = (
            self.i32c(FAT_INLINE_MAX_LATIN1),
            self.i32c(FAT_INLINE_MAX_TWO_BYTE),
        );
        let inline_max = self.select(Type::I32, fl, ft, flags);
        let roped = self.bin(Operator::I32GtU, total, inline_max, Type::I32);
        self.check(roped, helper);
        let max = self.i32c(STRING_MAX_LENGTH);
        let fits = self.bin(Operator::I32LeU, total, max, Type::I32);
        self.check(fits, helper);
        let hslot = self.i32c(self.h.strlit_slot + crate::region_shape::STRLIT_STR_HEADER_OFF);
        let hdr = self.load_i32(hslot, 0);
        let armed = self.bin(Operator::I32Ne, hdr, z, Type::I32);
        self.check(armed, helper);
        let size = self.i32c(NURSERY_HEADER_BYTES + ROPE_BYTES);
        let pos = self.nursery_bump(size, helper);
        self.store_i32(pos, 0, hdr);
        // The catch-all site's count, as the engine's allocation bumps it.
        let cslot = self.i32c(self.h.strlit_slot + crate::region_shape::STRLIT_STR_COUNT_ADDR_OFF);
        let caddr = self.load_i32(cslot, 0);
        let n = self.load_i32(caddr, 0);
        let one = self.i32c(1);
        let n1 = self.bin(Operator::I32Add, n, one, Type::I32);
        self.store_i32(caddr, 0, n1);
        let hb = self.i32c(NURSERY_HEADER_BYTES);
        let s = self.bin(Operator::I32Add, pos, hb, Type::I32);
        self.store_i32(s, STRING_FLAGS_OFFSET, flags);
        self.store_i32(s, STRING_LENGTH_OFFSET, total);
        self.store_i32(s, ROPE_LEFT_OFFSET, xp);
        self.store_i32(s, ROPE_RIGHT_OFFSET, yp);
        let payload = self.un(Operator::I64ExtendI32U, s, Type::I64);
        let tag = self.i64c(TAG_STRING << 32);
        let r = self.bin(Operator::I64Or, payload, tag, Type::I64);
        let t = self.edge(inst, 0, &[r])?;
        self.terminate(Terminator::Br { target: t });
        Ok(())
    }

    /// Whether `v` is a string literal (an atom): a `const.str`, boxed
    /// and weakened.
    pub(super) fn is_str_literal(&self, mut v: mir::Value) -> bool {
        loop {
            let mir::func::ValueDef::Result(i, _) = self.f.values[v].def else {
                return false;
            };
            match self.f.insts[i].op {
                Opcode::ConstStr(_) => return true,
                Opcode::Box | Opcode::Weaken => v = self.f.insts[i].args[0],
                _ => return false,
            }
        }
    }

    /// `x ==/=== lit` for a string literal `lit` (an atom), a
    /// literal-RHS ladder, each deciding arm taking `ok_clean`: a
    /// non-string is unequal (strictly; loosely it coerces: the helper's);
    /// the same pointer is equal; an atom that is not it is unequal (one
    /// flag load, the common miss of a `switch` on an atomized key); a
    /// different length is unequal; two linear strings compare their
    /// characters (a pure leaf). A rope falls through to the generic arms.
    pub(super) fn literal_eq(&mut self, inst: mir::Inst, cc: JsCc, x: Value, lit: Value) -> R<()> {
        use crate::wasm::mir::abi::{STRING_ATOM_BIT, STRING_FLAGS_OFFSET};
        let negate = matches!(cc, JsCc::Ne | JsCc::StrictNe);
        let strict = matches!(cc, JsCc::StrictEq | JsCc::StrictNe);
        let generic = self.body.add_block();
        let (eq_b, ne_b) = (self.body.add_block(), self.body.add_block());
        let tx = self.tag_of(x);
        let is_s = self.tag_is(tx, TAG_STRING as u32);
        let str_b = self.body.add_block();
        self.cond_br(
            is_s,
            Self::to(str_b),
            Self::to(if strict { ne_b } else { generic }),
        );
        self.cur = str_b;
        let same = self.bin(Operator::I64Eq, x, lit, Type::I32);
        let other = self.body.add_block();
        self.cond_br(same, Self::to(eq_b), Self::to(other));
        self.cur = other;
        let (xp, lp) = (
            self.un(Operator::I32WrapI64, x, Type::I32),
            self.un(Operator::I32WrapI64, lit, Type::I32),
        );
        let xf = self.load_i32(xp, STRING_FLAGS_OFFSET);
        let ab = self.i32c(STRING_ATOM_BIT);
        let atom = self.bin(Operator::I32And, xf, ab, Type::I32);
        let len_b = self.body.add_block();
        self.cond_br(atom, Self::to(ne_b), Self::to(len_b));
        self.cur = len_b;
        let xl = self.load_i32(xp, STRING_LENGTH_OFFSET);
        let ll = self.load_i32(lp, STRING_LENGTH_OFFSET);
        let len_eq = self.bin(Operator::I32Eq, xl, ll, Type::I32);
        let lin_b = self.body.add_block();
        self.cond_br(len_eq, Self::to(lin_b), Self::to(ne_b));
        self.cur = lin_b;
        let lb = self.i32c(STRING_LINEAR_BIT);
        let lin = self.bin(Operator::I32And, xf, lb, Type::I32);
        let chars_b = self.body.add_block();
        self.cond_br(lin, Self::to(chars_b), Self::to(generic));
        self.cur = chars_b;
        let r = self.call1(self.h.str_chars_eq, &[xp, lp], Type::I32);
        let r = if negate {
            self.un(Operator::I32Eqz, r, Type::I32)
        } else {
            r
        };
        let t = self.edge(inst, 0, &[r])?;
        self.terminate(Terminator::Br { target: t });
        for (blk, equal) in [(eq_b, true), (ne_b, false)] {
            self.cur = blk;
            let v = self.i32c(u32::from(equal != negate));
            let t = self.edge(inst, 0, &[v])?;
            self.terminate(Terminator::Br { target: t });
        }
        self.cur = generic;
        Ok(())
    }

    /// `switch.str`: `x` against atoms `cases`. A non-string takes the
    /// default. An atom equals a case exactly when it is that atom, so its
    /// tag, flags and pointer are tested once and each case is one compare
    /// of pointers (where a chain of `===`s retests the tag and the flags
    /// and reloads the atom table at every case). Another linear string
    /// compares lengths, then characters (a pure leaf); a rope takes
    /// `fail`.
    pub(super) fn switch_str(&mut self, inst: mir::Inst, a: &[Value], n: u32) -> R<()> {
        use crate::wasm::mir::abi::{STRING_ATOM_BIT, STRING_FLAGS_OFFSET};
        let x = a[0];
        let n = n as usize;
        // The cases' atoms, each loaded where it is compared (or, for a
        // case whose atom has other uses, the value already there).
        let margs = self.f.insts[inst].args.clone();
        let tslot = self.i32c(self.h.atom_table_slot);
        let tbl = self.load_i32(tslot, 0);
        let case = |l: &mut Self, k: usize| -> Value {
            match l.lazy_strs.get(&margs[1 + k]).copied() {
                Some(name) => {
                    let id = l.atoms.intern_chars(l.mm.atoms[name].chars());
                    l.load_i32(tbl, 4 * id)
                }
                None => a[1 + k],
            }
        };
        let default = self.edge(inst, n, &[])?;
        let tag = self.tag_of(x);
        let is_s = self.tag_is(tag, TAG_STRING as u32);
        let str_b = self.body.add_block();
        self.cond_br(is_s, Self::to(str_b), default.clone());
        self.cur = str_b;
        let p = self.un(Operator::I32WrapI64, x, Type::I32);
        let flags = self.load_i32(p, STRING_FLAGS_OFFSET);
        let ab = self.i32c(STRING_ATOM_BIT);
        let atom = self.bin(Operator::I32And, flags, ab, Type::I32);
        let (atom_b, other_b) = (self.body.add_block(), self.body.add_block());
        self.cond_br(atom, Self::to(atom_b), Self::to(other_b));
        self.cur = atom_b;
        for k in 0..n {
            let c = case(self, k);
            let same = self.bin(Operator::I32Eq, p, c, Type::I32);
            let next = self.body.add_block();
            let t = self.edge(inst, k, &[])?;
            self.cond_br(same, t, Self::to(next));
            self.cur = next;
        }
        self.terminate(Terminator::Br {
            target: default.clone(),
        });
        // Not an atom: linear, then by length and characters.
        self.cur = other_b;
        let lb = self.i32c(STRING_LINEAR_BIT);
        let lin = self.bin(Operator::I32And, flags, lb, Type::I32);
        let lin_b = self.body.add_block();
        let fail = self.edge(inst, n + 1, &[])?;
        self.cond_br(lin, Self::to(lin_b), fail);
        self.cur = lin_b;
        let len = self.load_i32(p, STRING_LENGTH_OFFSET);
        for k in 0..n {
            let c = case(self, k);
            let cl = self.load_i32(c, STRING_LENGTH_OFFSET);
            let eq_len = self.bin(Operator::I32Eq, len, cl, Type::I32);
            let (chars_b, next) = (self.body.add_block(), self.body.add_block());
            self.cond_br(eq_len, Self::to(chars_b), Self::to(next));
            self.cur = chars_b;
            let eq = self.call1(self.h.str_chars_eq, &[p, c], Type::I32);
            let t = self.edge(inst, k, &[])?;
            self.cond_br(eq, t, Self::to(next));
            self.cur = next;
        }
        self.terminate(Terminator::Br { target: default });
        Ok(())
    }

    /// The pristine `charCodeAt`/`charAt` on a linear string with an
    /// in-bounds int32 index, and `String.fromCharCode` of a code below 256
    /// (the char arms): the char inline (`charAt` and `fromCharCode`
    /// only below 256, a static unit string), taking `ok_clean`; else
    /// branch to `other` (the call). The callee is compared against the
    /// startup-cached natives' bits: a monkeypatched one misses.
    pub(super) fn char_arms(&mut self, inst: mir::Inst, ops: &[Value], other: Block) -> R<()> {
        let (callee, this, arg) = (ops[0], ops[1], ops[2]);
        let atag = self.tag_of(arg);
        let arg_int = self.tag_is(atag, TAG_INT32 as u32);
        let code = self.un(Operator::I32WrapI64, arg, Type::I32);
        let ccat_c = self.i32c(self.h.str_ccat_cell);
        let ccat = self.load_i64(ccat_c, 0);
        let cat_c = self.i32c(self.h.str_cat_cell);
        let cat = self.load_i64(cat_c, 0);
        let is_ccat = self.bin(Operator::I64Eq, callee, ccat, Type::I32);
        let is_cat = self.bin(Operator::I64Eq, callee, cat, Type::I32);
        let either = self.bin(Operator::I32Or, is_ccat, is_cat, Type::I32);
        let ttag = self.tag_of(this);
        let this_str = self.tag_is(ttag, TAG_STRING as u32);
        let m = self.bin(Operator::I32And, either, this_str, Type::I32);
        let m = self.bin(Operator::I32And, m, arg_int, Type::I32);
        let (chr, fcc) = (self.body.add_block(), self.body.add_block());
        self.cond_br(m, Self::to(chr), Self::to(fcc));
        // String.fromCharCode(code < 256): the static unit string.
        self.cur = fcc;
        let fcc_c = self.i32c(self.h.str_fcc_cell);
        let fcc_bits = self.load_i64(fcc_c, 0);
        let is_fcc = self.bin(Operator::I64Eq, callee, fcc_bits, Type::I32);
        let lim = self.i32c(256);
        let small = self.bin(Operator::I32LtU, code, lim, Type::I32);
        let f = self.bin(Operator::I32And, is_fcc, arg_int, Type::I32);
        let f = self.bin(Operator::I32And, f, small, Type::I32);
        self.check(f, other);
        let r = self.unit_string(code);
        let t = self.edge(inst, 0, &[r])?;
        self.terminate(Terminator::Br { target: t });
        // The char of a linear string.
        self.cur = chr;
        let sp = self.un(Operator::I32WrapI64, this, Type::I32);
        let flags = self.load_i32(sp, STRING_FLAGS_OFFSET);
        let lb = self.i32c(STRING_LINEAR_BIT);
        let lin = self.bin(Operator::I32And, flags, lb, Type::I32);
        self.check(lin, other);
        let len = self.load_i32(sp, STRING_LENGTH_OFFSET);
        let inb = self.bin(Operator::I32LtU, code, len, Type::I32);
        self.check(inb, other);
        let ib = self.i32c(STRING_INLINE_CHARS_BIT);
        let inl = self.bin(Operator::I32And, flags, ib, Type::I32);
        let outofline = self.load_i32(sp, STRING_CHARS_OFFSET);
        let inaddr = self.add_off(sp, STRING_CHARS_OFFSET);
        let chars = self.select(Type::I32, inaddr, outofline, inl);
        let latb = self.i32c(STRING_LATIN1_CHARS_BIT);
        let lat = self.bin(Operator::I32And, flags, latb, Type::I32);
        let (l8, l16, got) = (
            self.body.add_block(),
            self.body.add_block(),
            self.body.add_block(),
        );
        let c = self.body.add_blockparam(got, Type::I32);
        self.cond_br(lat, Self::to(l8), Self::to(l16));
        self.cur = l8;
        let a8 = self.bin(Operator::I32Add, chars, code, Type::I32);
        let m8 = self.mem(0, 0);
        let c8 = self.un(Operator::I32Load8U { memory: m8 }, a8, Type::I32);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: got,
                args: vec![c8],
            },
        });
        self.cur = l16;
        let one = self.i32c(1);
        let off = self.bin(Operator::I32Shl, code, one, Type::I32);
        let a16 = self.bin(Operator::I32Add, chars, off, Type::I32);
        let m16 = self.mem(1, 0);
        let c16 = self.un(Operator::I32Load16U { memory: m16 }, a16, Type::I32);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: got,
                args: vec![c16],
            },
        });
        self.cur = got;
        let (ccb, catb) = (self.body.add_block(), self.body.add_block());
        self.cond_br(is_ccat, Self::to(ccb), Self::to(catb));
        self.cur = ccb;
        let r = self.box_tagged(TAG_INT32, c);
        let t = self.edge(inst, 0, &[r])?;
        self.terminate(Terminator::Br { target: t });
        self.cur = catb;
        let lim = self.i32c(256);
        let small = self.bin(Operator::I32LtU, c, lim, Type::I32);
        self.check(small, other);
        let r = self.unit_string(c);
        let t = self.edge(inst, 0, &[r])?;
        self.terminate(Terminator::Br { target: t });
        Ok(())
    }

    /// The static unit string of code `c` (< 256), boxed.
    pub(super) fn unit_string(&mut self, c: Value) -> Value {
        let slot = self.i32c(self.h.static_strings_slot);
        let tbl = self.load_i32(slot, 0);
        let two = self.i32c(2);
        let off = self.bin(Operator::I32Shl, c, two, Type::I32);
        let e = self.bin(Operator::I32Add, tbl, off, Type::I32);
        let atom = self.load_i32(e, 0);
        self.box_tagged(TAG_STRING, atom)
    }

    /// `x.length` for a string (its length word) or an array (its elements
    /// header's, when an int32), taking `ok_clean`; else branch to `other`.
    pub(super) fn length_arms(&mut self, inst: mir::Inst, x: Value, other: Block) -> R<()> {
        let tag = self.tag_of(x);
        let (s_b, o_chk) = (self.body.add_block(), self.body.add_block());
        let is_str = self.tag_is(tag, TAG_STRING as u32);
        self.cond_br(is_str, Self::to(s_b), Self::to(o_chk));
        self.cur = s_b;
        let sp = self.un(Operator::I32WrapI64, x, Type::I32);
        let slen = self.load_i32(sp, STRING_LENGTH_OFFSET);
        let r = self.box_tagged(TAG_INT32, slen);
        let t = self.edge(inst, 0, &[r])?;
        self.terminate(Terminator::Br { target: t });
        self.cur = o_chk;
        let is_obj = self.tag_is(tag, TAG_OBJECT as u32);
        self.check(is_obj, other);
        let obj = self.un(Operator::I32WrapI64, x, Type::I32);
        let shape = self.load_i32(obj, SHAPE_OFFSET);
        let base = self.load_i32(shape, SHAPE_BASESHAPE_OFFSET);
        let clasp = self.load_i32(base, BASESHAPE_CLASP_OFFSET);
        let aslot = self.i32c(self.h.array_class_slot);
        let arr_class = self.load_i32(aslot, 0);
        let is_arr = self.bin(Operator::I32Eq, clasp, arr_class, Type::I32);
        let (arr_b, args_chk) = (self.body.add_block(), self.body.add_block());
        self.cond_br(is_arr, Self::to(arr_b), Self::to(args_chk));
        // An arguments object: its packed length (fixed slot 0, the count
        // above bit 5), unless the length was overwritten (bit 0).
        self.cur = args_chk;
        let acbase = self.i32c(self.h.args_class_base);
        let mapped = self.load_i32(acbase, 0);
        let unmapped = self.load_i32(acbase, 4);
        let m = self.bin(Operator::I32Eq, clasp, mapped, Type::I32);
        let u = self.bin(Operator::I32Eq, clasp, unmapped, Type::I32);
        let is_args = self.bin(Operator::I32Or, m, u, Type::I32);
        self.check(is_args, other);
        let packed = self.load_i32(obj, FIXED_SLOTS_BASE);
        let one = self.i32c(1);
        let over = self.bin(Operator::I32And, packed, one, Type::I32);
        let kept = self.un(Operator::I32Eqz, over, Type::I32);
        self.check(kept, other);
        let five = self.i32c(5);
        let argc = self.bin(Operator::I32ShrU, packed, five, Type::I32);
        let r = self.box_tagged(TAG_INT32, argc);
        let t = self.edge(inst, 0, &[r])?;
        self.terminate(Terminator::Br { target: t });
        self.cur = arr_b;
        let elements = self.load_i32(obj, OBJ_ELEMENTS_OFFSET);
        let back = self.i32c(ELEMENTS_LENGTH_BACK);
        let la = self.bin(Operator::I32Sub, elements, back, Type::I32);
        let alen = self.load_i32(la, 0);
        let z = self.i32c(0);
        let fits = self.bin(Operator::I32GeS, alen, z, Type::I32);
        self.check(fits, other);
        let r = self.box_tagged(TAG_INT32, alen);
        let t = self.edge(inst, 0, &[r])?;
        self.terminate(Terminator::Br { target: t });
        Ok(())
    }

    /// `x === k` for a `StrictConstantEq` operand `k` (the high byte its
    /// type: 2 boolean, 3 undefined, 4 null, else an int8), as a raw bool.
    pub(super) fn constant_strict_eq(&mut self, x: Value, k: u16) -> Value {
        let (ty, lo) = ((k >> 8) & 0xFF, (k & 0xFF) as u8);
        let tag = self.tag_of(x);
        match ty {
            3 => self.tag_is(tag, TAG_UNDEFINED as u32),
            4 => self.tag_is(tag, TAG_NULL as u32),
            2 => {
                let want = self.i64c((TAG_BOOLEAN << 32) | u64::from(lo & 1));
                self.bin(Operator::I64Eq, x, want, Type::I32)
            }
            _ => {
                // An int32 of that value, or a double equal to it.
                let n = i32::from(lo as i8);
                let is_int = self.tag_is(tag, TAG_INT32 as u32);
                let low = self.un(Operator::I32WrapI64, x, Type::I32);
                let nv = self.i32c(n as u32);
                let low_eq = self.bin(Operator::I32Eq, low, nv, Type::I32);
                let int_eq = self.bin(Operator::I32And, is_int, low_eq, Type::I32);
                let it = self.i32c(TAG_INT32 as u32);
                let is_dbl = self.bin(Operator::I32LtU, tag, it, Type::I32);
                let d = self.un(Operator::F64ReinterpretI64, x, Type::F64);
                let dv = self.f64c(f64::from(n).to_bits());
                let dbl_eq = self.bin(Operator::F64Eq, d, dv, Type::I32);
                let dbl_eq = self.bin(Operator::I32And, is_dbl, dbl_eq, Type::I32);
                self.bin(Operator::I32Or, int_eq, dbl_eq, Type::I32)
            }
        }
    }

    /// `s[i]` of a linear latin1 string (boxed) and an in-bounds int32
    /// index (boxed): the unit string from the static strings table;
    /// anything else branches to `fail`.
    pub(super) fn string_char(&mut self, recv: Value, key: Value, fail: Block) -> Value {
        let tag = self.tag_of(recv);
        let is_str = self.tag_is(tag, TAG_STRING as u32);
        let ktag = self.tag_of(key);
        let is_int = self.tag_is(ktag, TAG_INT32 as u32);
        let both = self.bin(Operator::I32And, is_str, is_int, Type::I32);
        self.check(both, fail);
        let s = self.un(Operator::I32WrapI64, recv, Type::I32);
        let flags = self.load_i32(s, STRING_FLAGS_OFFSET);
        let want = self.i32c(STRING_LINEAR_BIT | STRING_LATIN1_CHARS_BIT);
        let masked = self.bin(Operator::I32And, flags, want, Type::I32);
        let lin = self.bin(Operator::I32Eq, masked, want, Type::I32);
        let len = self.load_i32(s, STRING_LENGTH_OFFSET);
        let idx = self.un(Operator::I32WrapI64, key, Type::I32);
        let inb = self.bin(Operator::I32LtU, idx, len, Type::I32);
        let ok = self.bin(Operator::I32And, lin, inb, Type::I32);
        self.check(ok, fail);
        let ib = self.i32c(STRING_INLINE_CHARS_BIT);
        let inline = self.bin(Operator::I32And, flags, ib, Type::I32);
        let heap = self.load_i32(s, STRING_CHARS_OFFSET);
        let here = self.add_off(s, STRING_CHARS_OFFSET);
        let chars = self.select(Type::I32, here, heap, inline);
        let at = self.bin(Operator::I32Add, chars, idx, Type::I32);
        let m = self.mem(0, 0);
        let c = self.un(Operator::I32Load8U { memory: m }, at, Type::I32);
        let tslot = self.i32c(self.h.static_strings_slot);
        let tbl = self.load_i32(tslot, 0);
        let two = self.i32c(2);
        let off = self.bin(Operator::I32Shl, c, two, Type::I32);
        let ea = self.bin(Operator::I32Add, tbl, off, Type::I32);
        let atom = self.load_i32(ea, 0);
        self.box_tagged(TAG_STRING, atom)
    }
}
