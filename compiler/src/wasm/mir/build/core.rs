//! core: methods of `Run`, split out of `build.rs` unchanged.

use super::*;

impl<'s, 'a> Run<'s, 'a> {
    pub(super) fn new(
        s: &'s Shape<'a>,
        table: &'s BTreeMap<Pc, Vec<Ty>>,
        hoist: &'s BTreeMap<(Pc, usize), Ty>,
    ) -> Run<'s, 'a> {
        let frame = FrameShape {
            formals: s.nargs,
            locals: s.nlocals,
            depths: BTreeMap::new(),
            elided_args: Default::default(),
        };
        let mut f = mir::Func::new(s.sid, frame);
        let cur = f.add_block();
        Run {
            s,
            table,
            hoist,
            hoist_req: BTreeMap::new(),
            out: BTreeMap::new(),
            f,
            mm: mir::Module {
                array_key_min: s.ctx.array_stamp_in.values().map(|&w| w & 0xFFFF).min(),
                ..mir::Module::default()
            },
            blocks: BTreeMap::new(),
            preheaders: BTreeMap::new(),
            entry_types: BTreeMap::new(),
            pending: BTreeMap::new(),
            widen: false,
            cur,
            live: true,
            st: vec![],
            args_placeholder: None,
            args_locals: std::collections::BTreeSet::new(),
            args_copies: std::collections::BTreeSet::new(),
            pc: Pc::new(0),
            pre: vec![],
            exit_blk: None,
            throw_blk: None,
            renames: vec![],
            next_pc: None,
            bug: None,
            likely_targets: vec![],
            gname_vals: BTreeMap::new(),
            math_fns: BTreeMap::new(),
            ta_poly_site: false,
            store_mask: None,
            val_cls: BTreeMap::new(),
            lit_rows: BTreeMap::new(),
            ctor_hint: BTreeMap::new(),
            boxed_as: BTreeMap::new(),
            ctor_layouts: BTreeMap::new(),
            this_out: None,
            slot_cls: BTreeMap::new(),
            this_vals: Default::default(),
            this_slots: Default::default(),
            inline_sites: 0,
            admitted: InlineCtx::ROOT,
        }
    }

    pub(super) fn finish(mut self) -> mir::Func {
        // Drop blocks nothing reaches (a run creates a block for every
        // leader with an entry type, and some are only reached from blocks
        // that turned out dead).
        let mut seen = std::collections::BTreeSet::new();
        let mut work: Vec<mir::Block> = self.f.roots.iter().map(|r| r.block).collect();
        while let Some(b) = work.pop() {
            if seen.insert(b) {
                work.extend(self.f.succs(b));
            }
        }
        self.f.layout.retain(|b| seen.contains(b));
        self.f.loops.retain(|l| seen.contains(&l.header));
        self.f
    }

    pub(super) fn inst(
        &mut self,
        op: Opcode,
        args: Vec<mir::Value>,
        result: Option<MType>,
    ) -> mir::Value {
        let tys: Vec<MType> = result.into_iter().collect();
        let (_, rs) = self.f.add_inst(self.cur, op, args, &tys, vec![]);
        rs.first()
            .copied()
            .unwrap_or_else(|| mir::Value::from_u32(0))
    }

    pub(super) fn term(&mut self, op: Opcode, args: Vec<mir::Value>, mut succs: Vec<Edge>) {
        self.fence_params(&op, &args, &mut succs);
        let (inst, _) = self.f.add_inst(self.cur, op, args, &[], succs);
        self.attach_targets(inst);
        self.live = false;
    }

    /// The module's binding for syntactic global `name` (row `slot`),
    /// declared on first use.
    pub(super) fn binding(
        &mut self,
        name: mir::entity::AtomId,
        slot: u32,
    ) -> mir::entity::BindingId {
        if let Some((b, _)) = self.mm.bindings.iter().find(|(_, d)| d.slot == slot) {
            return b;
        }
        // A predicted function (`gname_fns`): its script's address is what
        // the runtime's arm compares, and what a guard on it would.
        let pred = self.s.ctx.binding_preds.get(&slot).map(|&(k, addr)| {
            self.mm.script_addrs.insert(k, addr);
            k
        });
        self.mm.bindings.push(mir::module::BindingDef {
            name,
            claim: MType::VAL_TOP,
            slot,
            pred,
        })
    }

    /// The constant `v` is, if it is an `i32` constant.
    #[cfg(feature = "ff147")]
    pub(super) fn const_i32_of(&self, v: mir::Value) -> Option<i32> {
        match self.f.values[v].def {
            mir::func::ValueDef::Result(i, _) => match self.f.insts[i].op {
                Opcode::ConstI32(n) => Some(n),
                _ => None,
            },
            _ => None,
        }
    }

    pub(super) fn repush(&mut self, x: Slot) {
        let y = self
            .renames
            .iter()
            .rev()
            .find(|(v, _)| *v == x.v)
            .map_or(x, |&(_, y)| y);
        self.st.push(y);
    }

    /// Whether the script's formals are its mapped `arguments` object's.
    pub(super) fn mapped(&self) -> bool {
        self.s.script.has_mapped_args && self.s.nargs > 0
    }

    pub(super) fn goto(block: mir::Block) -> Edge {
        Edge {
            block,
            args: vec![],
        }
    }

    pub(super) fn new_block(&mut self) -> mir::Block {
        self.f.add_block()
    }

    /// Continue in `b` (opened by the caller's terminator).
    pub(super) fn at(&mut self, b: mir::Block) {
        self.cur = b;
        self.live = true;
    }

    /// Whether a value of type `t` can hold a GC thing, so the frame must
    /// keep it visible to the GC while its local holds it.
    fn managed_ty(t: Ty) -> bool {
        matches!(
            t,
            Ty::Val(_)
                | Ty::Fn(_)
                | Ty::Obj(..)
                | Ty::ObjHint(_)
                | Ty::Ctor(..)
                | Ty::Native
                | Ty::Ta(_)
        )
    }

    /// Whether a value of type `t` may be a heap thing (not `undefined`,
    /// `null`, a boolean or a number).
    fn heap_ty(t: Ty) -> bool {
        use crate::opsem::{NUM, PRIM_BOOLEAN, PRIM_NULL, PRIM_UNDEFINED};
        match t {
            Ty::Val(tags) => !tags.subset_of(TagSet::prims(
                PRIM_UNDEFINED | PRIM_NULL | PRIM_BOOLEAN | NUM,
            )),
            t => Self::managed_ty(t),
        }
    }

    /// Baseline writes every assignment to a local's frame slot, so the slot
    /// holds the local's latest value until the next assignment: a dead local
    /// keeps its object (a `WeakMap` key held only by a dead local must
    /// survive), and the object an overwritten local held is released. MIR
    /// keeps values in SSA, so it writes the slot where that matters, at the
    /// assignment: the new value if it may be a heap thing, else `undefined`
    /// if the slot may hold one (its old type is a heap type, or unknown
    /// because the slot is dead here). A scalar over a scalar writes nothing:
    /// by induction the slot holds no heap thing. Nothing else rewrites the
    /// slot, so no path-dependent state is kept.
    ///
    /// A formal of a script that reads its actuals later (`Rest`, `arguments`,
    /// spread forwarding) keeps the caller's value in its slot; the return
    /// value slot is written like any local.
    fn frame_set(&mut self, ix: usize, old: Slot, x: Slot) {
        if ix == 0
            || ix >= self.frame_len()
            || x.ty == Ty::Dead
            || self.write_through(ix)
            || (self.is_formal(ix) && crate::wasm::baseline::layout::reads_actuals(self.s.script))
        {
            return;
        }
        let v = if Self::heap_ty(x.ty) {
            x.v
        } else if Self::heap_ty(old.ty) || old.ty == Ty::Dead {
            self.const_val(ConstVal::Undefined)
        } else {
            return;
        };
        self.f.add_inst(
            self.cur,
            Opcode::FrameStore(u32::try_from(ix).unwrap()),
            vec![v],
            &[],
            vec![],
        );
    }

    pub(super) fn const_val(&mut self, c: ConstVal) -> mir::Value {
        let t = mir::ops::const_val_type(c);
        self.inst(Opcode::ConstVal(c), vec![], Some(t))
    }

    pub(super) fn const_i32(&mut self, n: i32) -> mir::Value {
        let t = MType::i32_range(n.into(), n.into());
        self.inst(Opcode::ConstI32(n), vec![], Some(t))
    }

    pub(super) fn const_f64(&mut self, x: f64) -> mir::Value {
        let t = MType::F64(mir::types::NumInfo::exact(x));
        self.inst(Opcode::ConstF64(x.to_bits()), vec![], Some(t))
    }

    pub(super) fn weaken(&mut self, v: mir::Value, t: MType) -> mir::Value {
        if self.f.ty(v) == t {
            return v;
        }
        self.inst(Opcode::Weaken, vec![v], Some(t))
    }

    /// `x` as a boxed value, of type `Val(x.ty.tags())`.
    pub(super) fn boxed(&mut self, x: Slot) -> mir::Value {
        if let Some(&b) = self.boxed_as.get(&x.v) {
            return b;
        }
        let v = match x.ty {
            Ty::Val(_) | Ty::Fn(_) => return x.v,
            _ => {
                let t = mir::ops::box_type(&self.f.ty(x.v)).expect("raw values box");
                self.inst(Opcode::Box, vec![x.v], Some(t))
            }
        };
        self.weaken(v, MType::val(x.ty.tags()))
    }

    /// `x` converted to type `to`, which it fits.
    pub(super) fn convert(&mut self, x: Slot, to: Ty) -> mir::Value {
        match (x.ty, to) {
            (a, b) if a == b => x.v,
            (Ty::I32, Ty::F64) => self.inst(Opcode::I32ToF64, vec![x.v], Some(MType::F64_TOP)),
            (Ty::Obj(..), Ty::Obj(..) | Ty::ObjHint(_))
            | (Ty::ObjHint(_), Ty::ObjHint(_))
            | (Ty::Ctor(..), Ty::Ctor(..)) => self.weaken(x.v, to.mir()),
            (_, Ty::Val(t)) => {
                let v = self.boxed(x);
                self.weaken(v, MType::val(t))
            }
            (a, b) => unreachable!("convert {a:?} to {b:?}"),
        }
    }

    /// The type `op`'s rule gives its result on operands `args`: ranges
    /// and all, where the builder would otherwise declare the top of the
    /// representation.
    pub(super) fn sig_result(&self, op: &Opcode, args: &[mir::Value]) -> MType {
        let tys: Vec<MType> = args.iter().map(|&v| self.f.ty(v)).collect();
        let sig = mir::ops::signature(op, &tys, &self.mm).expect("well-typed operands");
        sig.results
            .into_iter()
            .chain(sig.outputs)
            .next()
            .expect("a result")
    }

    /// Integer `x op y` (`%` too) on int32s, for an int32 result: unchecked
    /// where the operands' ranges prove the result an int32 other than -0
    /// (a nonzero divisor for `%`: MIR.md §10.6), else checked, exiting on
    /// failure. Either way the result carries its range.
    pub(super) fn int_arith(&mut self, op: ArithOp, x: mir::Value, y: mir::Value) -> mir::Value {
        let range = |v: mir::Value| match self.f.ty(v) {
            MType::I32(r) => r,
            _ => IRange::new(I32_MIN, I32_MAX),
        };
        if let Some(r) = mir::ops::i32_exact(op, range(x), range(y)) {
            return self.inst(Opcode::I32Wrap(op), vec![x, y], Some(MType::I32(r)));
        }
        let out = self.sig_result(&Opcode::I32Ovf(op), &[x, y]);
        self.guard(Opcode::I32Ovf(op), vec![x, y], out)
    }

    pub(super) fn as_i32(&mut self, x: Slot) -> mir::Value {
        match x.ty {
            Ty::I32 => x.v,
            Ty::Val(_) => self.inst(
                Opcode::Unbox(UnboxKind::I32),
                vec![x.v],
                Some(MType::I32_TOP),
            ),
            t => unreachable!("as_i32 {t:?}"),
        }
    }

    pub(super) fn as_f64(&mut self, x: Slot) -> mir::Value {
        match x.ty {
            Ty::F64 => x.v,
            Ty::I32 => self.inst(Opcode::I32ToF64, vec![x.v], Some(MType::F64_TOP)),
            Ty::Val(_) => self.inst(
                Opcode::Unbox(UnboxKind::F64Num),
                vec![x.v],
                Some(MType::F64_TOP),
            ),
            t => unreachable!("as_f64 {t:?}"),
        }
    }

    /// A number as an int32, by ToInt32.
    pub(super) fn to_int32(&mut self, x: Slot) -> mir::Value {
        match x.ty.num() {
            Some(Num::I32) => self.as_i32(x),
            _ => {
                let f = self.as_f64(x);
                self.inst(Opcode::ToInt32, vec![f], Some(MType::I32_TOP))
            }
        }
    }

    pub(super) fn site(&self, pc: Pc) -> crate::ids::Site {
        crate::ids::Site::new(self.s.sid, pc)
    }

    pub(super) fn push(&mut self, v: mir::Value, ty: Ty) {
        self.st.push(Slot { v, ty });
    }

    pub(super) fn pop(&mut self) -> Slot {
        self.st.pop().expect("stack underflow")
    }

    pub(super) fn top(&self) -> Slot {
        *self.st.last().expect("empty stack")
    }

    /// Slots before the operand stack: `this`, formals, locals, rval.
    pub(super) fn frame_len(&self) -> usize {
        (2 + self.s.nargs + self.s.nlocals) as usize
    }

    /// Whether slot `ix` is a formal.
    pub(super) fn is_formal(&self, ix: usize) -> bool {
        ix >= 1 && ix <= self.s.nargs as usize
    }

    /// Whether frame slot `ix` is written through: only the formals of a
    /// mapped `arguments`, which aliases them. Its frame copy then always
    /// holds its value, so exits leave it (§5.1). Everything else stays in
    /// its own representation, and exits write it.
    pub(super) fn write_through(&self, ix: usize) -> bool {
        self.mapped() && ix >= 1 && ix <= self.s.nargs as usize
    }

    /// Assign frame slot `ix`, writing it through to the frame.
    pub(super) fn set_frame_slot(&mut self, ix: usize, x: Slot) {
        if self.this_vals.contains(&x.v) {
            self.this_slots.insert(ix);
        } else {
            self.this_slots.remove(&ix);
        }
        match self.val_cls.get(&x.v) {
            Some(&c) => self.slot_cls.insert(ix, c),
            None => self.slot_cls.remove(&ix),
        };
        if self.args_placeholder == Some(x.v) || self.args_copies.contains(&x.v) {
            self.args_locals.insert(ix);
            self.f.frame.elided_args.insert(u32::try_from(ix).unwrap());
        }
        if x.ty != Ty::Dead && self.write_through(ix) {
            self.inst(
                Opcode::FrameStore(u32::try_from(ix).unwrap()),
                vec![x.v],
                None,
            );
        }
        let old = self.st[ix];
        self.st[ix] = x;
        self.frame_set(ix, old, x);
    }

    pub(super) fn arg_ix(&self, n: u32) -> usize {
        1 + n as usize
    }

    pub(super) fn local_ix(&self, n: u32) -> usize {
        1 + (self.s.nargs + n) as usize
    }

    pub(super) fn rval_ix(&self) -> usize {
        1 + (self.s.nargs + self.s.nlocals) as usize
    }
}
