//! boxing: methods of `Lower`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    pub(super) fn tag_of(&mut self, v: Value) -> Value {
        let sh = self.i64c(32);
        let hi = self.bin(Operator::I64ShrU, v, sh, Type::I64);
        self.un(Operator::I32WrapI64, hi, Type::I32)
    }

    pub(super) fn tag_is(&mut self, tag: Value, t: u32) -> Value {
        let k = self.i32c(t);
        self.bin(Operator::I32Eq, tag, k, Type::I32)
    }

    pub(super) fn box_tagged(&mut self, tag: u64, payload: Value) -> Value {
        let p = self.un(Operator::I64ExtendI32U, payload, Type::I64);
        let t = self.i64c(tag << 32);
        self.bin(Operator::I64Or, t, p, Type::I64)
    }

    /// Box an f64 as `NumberValue` does: an int32 when it is one exactly
    /// (not -0), else a double, with NaN canonicalized.
    pub(super) fn box_number(&mut self, x: Value) -> Value {
        let i = self.un(Operator::I32TruncSatF64S, x, Type::I32);
        let back = self.un(Operator::F64ConvertI32S, i, Type::F64);
        let exact = self.bin(Operator::F64Eq, back, x, Type::I32);
        let bits = self.un(Operator::I64ReinterpretF64, x, Type::I64);
        let negz = self.i64c(1 << 63);
        let not_negz = self.bin(Operator::I64Ne, bits, negz, Type::I32);
        let is_int = self.bin(Operator::I32And, exact, not_negz, Type::I32);
        let nan = self.bin(Operator::F64Ne, x, x, Type::I32);
        let canon = self.i64c(CANONICAL_NAN_BITS);
        let dbl = self.select(Type::I64, canon, bits, nan);
        let int = self.box_tagged(TAG_INT32, i);
        self.select(Type::I64, int, dbl, is_int)
    }

    /// A value of type `t` (lowered as `v`) as a boxed Value.
    pub(super) fn boxed(&mut self, t: &MType, v: Value) -> R<Value> {
        Ok(match t {
            MType::Val(_) => v,
            MType::I32(_) => self.box_tagged(TAG_INT32, v),
            MType::Bool => self.box_tagged(TAG_BOOLEAN, v),
            MType::Obj(_) => self.box_tagged(TAG_OBJECT, v),
            MType::Str(_) => self.box_tagged(TAG_STRING, v),
            MType::F64(_) => self.box_number(v),
            MType::Int(_) => {
                let x = self.un(Operator::F64ConvertI64S, v, Type::F64);
                self.box_number(x)
            }
            t => return Err(format!("box: cannot box {}", mir::print::type_str(t))),
        })
    }

    /// The inverse of `boxed` for the managed representations.
    pub(super) fn unboxed_managed(&mut self, t: &MType, v: Value) -> Value {
        match t {
            MType::Val(_) => v,
            _ => self.un(Operator::I32WrapI64, v, Type::I32),
        }
    }

    /// `v`'s value where its type pins one int32 (as an i64: `-c` cannot
    /// overflow).
    pub(super) fn const_i32(&self, v: mir::Value) -> Option<i64> {
        match self.ty(v) {
            MType::I32(r) if r.lo == r.hi => Some(r.lo),
            _ => None,
        }
    }

    /// Whether guard `inst`'s miss is a plain branch to its `fail` edge
    /// (no `--mir-stress` failures and no exit census call to emit).
    pub(super) fn plain_guard(&self, inst: mir::Inst) -> bool {
        let fail_blk = self.f.insts[inst].succs[1].block;
        self.stress == 0 && (self.exit_census.is_none() || self.guard_exits(fail_blk).is_none())
    }

    /// `guard.unbox.f64num`'s edges from `v`'s `tag`, the double first: a
    /// double's bits are the f64 (one compare, no select), an int32
    /// converts, and anything else takes `fail`. `have` (what `v` may be)
    /// drops the arm a value cannot take.
    pub(super) fn unbox_num_edges(
        &mut self,
        inst: mir::Inst,
        v: Value,
        tag: Value,
        have: TagSet,
    ) -> R<()> {
        let may_dbl = !have.intersect(TagSet::prims(PRIM_DOUBLE)).is_empty();
        let may_int = !have.intersect(TagSet::INT32).is_empty();
        let join = self.body.add_block();
        let x = self.body.add_blockparam(join, Type::F64);
        let fail = self.body.add_block();
        let not_dbl = if may_dbl {
            let (d_b, nd_b) = (self.body.add_block(), self.body.add_block());
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
            nd_b
        } else {
            self.cur
        };
        self.cur = not_dbl;
        if may_int {
            let i_b = self.body.add_block();
            let is_int = self.tag_is(tag, TAG_INT32 as u32);
            self.cond_br(is_int, Self::to(i_b), Self::to(fail));
            self.cur = i_b;
            let low = self.un(Operator::I32WrapI64, v, Type::I32);
            let fi = self.un(Operator::F64ConvertI32S, low, Type::F64);
            self.terminate(Terminator::Br {
                target: BlockTarget {
                    block: join,
                    args: vec![fi],
                },
            });
        } else {
            self.terminate(Terminator::Br {
                target: Self::to(fail),
            });
        }
        self.cur = fail;
        let f = self.edge(inst, 1, &[])?;
        self.terminate(Terminator::Br { target: f });
        self.cur = join;
        let t = self.edge(inst, 0, &[x])?;
        self.terminate(Terminator::Br { target: t });
        Ok(())
    }

    /// The unbox guard a `load_elem` may test first: its `ok` edge's block
    /// has no other way in and begins with `guard.unbox` of the loaded
    /// value, in the same frame. Returns the guard, its kind, and the
    /// block param the value arrives in.
    pub(super) fn fusable_unbox(
        &self,
        inst: mir::Inst,
    ) -> Option<(mir::Inst, UnboxKind, mir::Value)> {
        let f = self.f;
        let e = &f.insts[inst].succs[0];
        let k = e.args.iter().position(|a| *a == EdgeArg::Out(0))?;
        let p = *f.blocks[e.block].params.get(k)?;
        if self.preds.get(&e.block).map_or(0, |v| v.len()) != 1
            || f.roots.iter().any(|r| r.block == e.block)
        {
            return None;
        }
        let &g = f.blocks[e.block].insts.first()?;
        let Opcode::GuardUnbox(kind) = f.insts[g].op else {
            return None;
        };
        (f.insts[g].args[0] == p && f.inst_frame[g] == f.inst_frame[inst] && self.plain_guard(g))
            .then_some((g, kind, p))
    }

    /// A number Value (int32 or double) as an f64.
    pub(super) fn to_f64(&mut self, v: Value) -> Value {
        let tag = self.tag_of(v);
        let is_int = self.tag_is(tag, TAG_INT32 as u32);
        let low = self.un(Operator::I32WrapI64, v, Type::I32);
        let fi = self.un(Operator::F64ConvertI32S, low, Type::F64);
        let fd = self.un(Operator::F64ReinterpretI64, v, Type::F64);
        self.select(Type::F64, fi, fd, is_int)
    }

    /// JS ToInt32 of an f64: its integer part modulo 2^32. `x - trunc(x /
    /// 2^32) * 2^32` is exact for every finite `x` (scaling by a power of
    /// two is exact, and the difference fits `x`'s precision), and brings
    /// it within (-2^32, 2^32), where a saturating i64 truncation and a
    /// wrap finish the job. NaN and the infinities come out as NaN, which
    /// truncates to 0, as ToInt32 wants.
    pub(super) fn to_int32(&mut self, x: Value) -> Value {
        let two32 = self.f64c(4294967296f64.to_bits());
        let q = self.bin(Operator::F64Div, x, two32, Type::F64);
        let qt = self.un(Operator::F64Trunc, q, Type::F64);
        let m = self.bin(Operator::F64Mul, qt, two32, Type::F64);
        let r = self.bin(Operator::F64Sub, x, m, Type::F64);
        let i = self.un(Operator::I64TruncSatF64S, r, Type::I64);
        self.un(Operator::I32WrapI64, i, Type::I32)
    }

    /// Whether `v`'s tag is in `tags`.
    pub(super) fn has_tags(&mut self, v: Value, tags: TagSet) -> Value {
        let tag = self.tag_of(v);
        let mut acc: Option<Value> = None;
        let mut or = |l: &mut Self, c: Value| {
            acc = Some(match acc {
                Some(a) => l.bin(Operator::I32Or, a, c, Type::I32),
                None => c,
            });
        };
        let singles = [
            (PRIM_INT32, TAG_INT32 as u32),
            (PRIM_BOOLEAN, TAG_BOOLEAN as u32),
            (PRIM_UNDEFINED, TAG_UNDEFINED as u32),
            (PRIM_NULL, TAG_NULL as u32),
            (PRIM_STRING, TAG_STRING as u32),
            (PRIM_SYMBOL, TAG_SYMBOL as u32),
            (PRIM_BIGINT, TAG_BIGINT_HI),
        ];
        for (prim, t) in singles {
            if tags.prims.intersects(prim) {
                let c = self.tag_is(tag, t);
                or(self, c);
            }
        }
        if tags.prims.intersects(PRIM_DOUBLE) {
            let k = self.i32c(TAG_CLEAR);
            let c = self.bin(Operator::I32LtU, tag, k, Type::I32);
            or(self, c);
        }
        if tags.object {
            let c = self.tag_is(tag, TAG_OBJECT as u32);
            or(self, c);
        }
        if tags.magic {
            let c = self.tag_is(tag, TAG_MAGIC as u32);
            or(self, c);
        }
        match acc {
            Some(a) => a,
            None => self.i32c(0),
        }
    }
}
