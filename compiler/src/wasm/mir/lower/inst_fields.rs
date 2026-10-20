//! inst fields: the lowering of one run of arms of `Lower::inst`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    pub(super) fn inst_fields(
        &mut self,
        inst: mir::Inst,
        d: &mir::func::InstData,
        a: &[Value],
    ) -> R<bool> {
        match d.op {
            Opcode::LoadField(name) => self.field_op(inst, name, a[0], None)?,
            Opcode::LoadSlot(name) => {
                let t = self.ty(self.f.insts[inst].args[0]);
                let o = t.obj_info().ok_or("lowering: load_slot of a non-object")?;
                let slot = mir::ops::slot_of(o, name, self.mm)
                    .ok_or("lowering: load_slot without a proven slot")?;
                let v = self.load_i64(a[0], FIXED_SLOTS_BASE + 8 * slot);
                self.def(inst, v);
            }
            Opcode::StoreField(name) => self.field_op(inst, name, a[0], Some(a[1]))?,
            Opcode::StoreSlot(name) => {
                // `field_op`'s store with TYPES kept by type: RANGES dropped
                // (no MIR claim reads it), the barriers unless a number.
                let d = &self.f.insts[inst];
                let t = self.ty(d.args[0]);
                let num =
                    matches!(self.ty(d.args[1]), MType::Val(s) if s.tags.subset_of(TagSet::NUMBER));
                let o = t.obj_info().ok_or("lowering: store_slot of a non-object")?;
                let slot = mir::ops::slot_of(o, name, self.mm)
                    .ok_or("lowering: store_slot without a proven slot")?;
                let off = FIXED_SLOTS_BASE + 8 * slot;
                let w = self.load_i32(a[0], OBJ_CLASS_IDX_OFFSET);
                self.clear_bits(a[0], w, CLASS_WORD_RANGES);
                if !num {
                    self.pre_barrier(a[0], off);
                }
                self.store_i64(a[0], off, a[1]);
                if !num {
                    let sv = self.i32c(slot);
                    self.post_barrier(self.h.post_write_barrier, a[0], sv, a[1]);
                }
            }
            Opcode::JsBoxThis => {
                // Allocates (a primitive's wrapper); runs no user code.
                let live = self.live_across(inst);
                let (ok, r) = self.gc_call(self.h.box_nonstrict_this, &[a[0]], &live)?;
                self.epoch_same = None;
                let t = self.edge(inst, 0, &[r])?;
                let e = self.edge(inst, 1, &[])?;
                self.cond_br(ok, t, e);
            }
            Opcode::JsBindGName(name) => {
                if let Some(&bid) = self.gname_bids.get(&name) {
                    self.gname_bind_arms(inst, bid)?;
                }
                let at = self.atom(name);
                self.js_call(inst, self.h.bind_unqualified_gname, &[at], false)?;
            }
            Opcode::JsSetName(name, strict) => {
                if let Some(&bid) = self.gname_bids.get(&name) {
                    let fused = self.gname_fused.get(&name).copied();
                    self.gname_set_arms(inst, bid, a[1], fused)?;
                }
                let at = self.atom(name);
                let sv = self.i32c(u32::from(strict));
                self.js_call(inst, self.h.set_name, &[a[0], at, a[1], sv], false)?;
            }
            Opcode::ConstStr(_) if self.lazy_strs.contains_key(&self.f.insts[inst].results[0]) => {}
            Opcode::ConstStr(name) => {
                // The atom itself, from the startup-filled atom table (a
                // pinned atom never moves):
                // one load, no allocation.
                let id = self.atoms.intern_chars(self.mm.atoms[name].chars());
                let slot = self.i32c(self.h.atom_table_slot);
                let tbl = self.load_i32(slot, 0);
                let v = self.load_i32(tbl, 4 * id);
                self.def(inst, v);
            }
            op => {
                return Err(format!(
                    "lowering: {} is not lowered yet",
                    mir::print::mnemonic(&op)
                ))
            }
        }
        Ok(true)
    }
}
