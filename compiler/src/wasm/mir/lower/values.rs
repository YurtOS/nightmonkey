//! values: methods of `Lower`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    pub(super) fn ty(&self, v: mir::Value) -> MType {
        self.f.values[v].ty
    }

    pub(super) fn get(&self, v: mir::Value) -> R<Value> {
        self.vmap
            .get(&v)
            .copied()
            .ok_or_else(|| format!("lowering: {v} has no value here"))
    }

    /// `v` in a register at the emission point: a managed value only in
    /// its home slot is loaded (without recording the copy, which is valid
    /// only on this path).
    pub(super) fn value_here(&mut self, v: mir::Value) -> R<Value> {
        if let Some(&w) = self.vmap.get(&v) {
            return Ok(w);
        }
        if self.slotted.contains(&v) {
            return self.load_home(v);
        }
        Err(format!("lowering: {v} has no value here"))
    }

    /// Load managed `v` from its home slot, unboxed to its representation.
    pub(super) fn load_home(&mut self, v: mir::Value) -> R<Value> {
        let off = self.home_off(v)?;
        let raw = self.load_i64(self.vp, off);
        let t = self.ty(v);
        Ok(self.unboxed_managed(&t, raw))
    }

    pub(super) fn home_off(&self, v: mir::Value) -> R<u32> {
        let k = self.home.get(&v).ok_or_else(|| {
            format!("lowering: {v} is live across a may-GC call but has no home slot")
        })?;
        Ok(self.root_base + 8 * k)
    }

    /// The instruction's operands, at its start (which dominates all of its
    /// code): a value only in its home slot is loaded, and the copy kept
    /// until the next may-GC call.
    pub(super) fn args(&mut self, inst: mir::Inst) -> R<Vec<Value>> {
        let vs = self.f.insts[inst].args.clone();
        let mut out = vec![];
        for v in vs {
            // A ghost (`Fact`) has no representation, and a lazy switch
            // case is loaded by the switch itself: a placeholder no
            // lowering reads.
            if machine(&self.ty(v)).is_none() || self.lazy_strs.contains_key(&v) {
                out.push(self.i32c(0));
                continue;
            }
            let w = self.value_here(v)?;
            self.vmap.insert(v, w);
            out.push(w);
        }
        Ok(out)
    }
}
