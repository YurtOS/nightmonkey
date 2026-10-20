//! inst env: the lowering of one run of arms of `Lower::inst`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    pub(super) fn inst_env(
        &mut self,
        inst: mir::Inst,
        d: &mir::func::InstData,
        a: &[Value],
    ) -> R<bool> {
        let at = |i: usize| self.f.values[d.args[i]].ty;
        match d.op {
            // The activation's environment is fixed (§5.1): the frame's env
            // slot, which the fresh entry (or baseline, before an onramp)
            // set, and which is rooted with the frame.
            Opcode::GlobalObject => {
                let realm = self.load_i32(self.cx, JSCONTEXT_REALM_OFFSET);
                let global = self.load_i32(realm, REALM_GLOBAL_OFFSET);
                let g = self.box_tagged(TAG_OBJECT, global);
                self.def(inst, g);
            }
            Opcode::EnvCurrent => {
                let env = self.frame_env();
                let p = self.un(Operator::I32WrapI64, env, Type::I32);
                self.def(inst, p);
            }
            Opcode::EnvCallee(hops) => {
                let env = self.frame_env();
                let hv = self.i32c(hops);
                let r = self.call1(self.h.env_callee, &[self.cx, env, hv], Type::I64);
                self.def(inst, r);
            }
            Opcode::ObjectLit(index) => {
                let script = self.script_ptr();
                let iv = self.i32c(index);
                let r = self.call1(self.h.object, &[self.cx, script, iv], Type::I64);
                self.def(inst, r);
            }
            Opcode::EnvSet => {
                let off = self.frame_env_off();
                self.store_i64(self.vp, off, a[0]);
            }
            Opcode::EnvPop => {
                let env = self.frame_env();
                let p = self.un(Operator::I32WrapI64, env, Type::I32);
                let up = self.load_i64(p, FIXED_SLOTS_BASE);
                let off = self.frame_env_off();
                self.store_i64(self.vp, off, up);
            }
            // `EnvironmentObject::ENCLOSING_ENV_SLOT`, always fixed.
            Opcode::EnvParent => {
                let env = self.load_i64(a[0], FIXED_SLOTS_BASE);
                let p = self.un(Operator::I32WrapI64, env, Type::I32);
                self.def(inst, p);
            }
            Opcode::EnvLoad(slot) => {
                let (addr, off) = self.env_slot(a[0], slot.get());
                let v = self.load_i64(addr, off);
                self.def(inst, v);
            }
            Opcode::EnvStore(slot) => {
                // `setAliasedBinding` zero hops up, inline: the slot store
                // with its barriers.
                let (addr, off) = self.env_slot(a[0], slot.get());
                self.pre_barrier(addr, off);
                self.store_i64(addr, off, a[1]);
                let s = self.i32c(slot.get());
                self.post_barrier(self.h.post_write_barrier, a[0], s, a[1]);
            }
            Opcode::FrameStore(k) => {
                let fid = self.cur_frame as usize;
                let l = self.frame_layouts[fid];
                let (nargs, nlocals) = (l.nargs, l.nlocals);
                let off = match k {
                    0 => self.frame_off[fid] + FrameLayout::THIS,
                    k if k <= nargs => self.frame_off[fid] + l.arg(k - 1),
                    k if k <= nargs + nlocals => self.frame_voff[fid] + l.local(k - 1 - nargs),
                    _ => self.frame_voff[fid] + l.rval(),
                };
                // The Value it is. A double goes in as a double (NaN made
                // canonical, as any boxed double must be), not re-tagged
                // as an int32: `box_number` is not needed for validity.
                let v = match at(0) {
                    MType::F64(_) => {
                        let bits = self.un(Operator::I64ReinterpretF64, a[0], Type::I64);
                        let nan = self.bin(Operator::F64Ne, a[0], a[0], Type::I32);
                        let canon = self.i64c(CANONICAL_NAN_BITS);
                        self.select(Type::I64, canon, bits, nan)
                    }
                    t => self.boxed(&t, a[0])?,
                };
                // `this` and the formals sit below the actuals (`sp`); the
                // rest of the function's frame, and inline frames, at `vp`.
                let base = if fid == 0 && k <= nargs {
                    self.sp
                } else {
                    self.vp
                };
                self.store_i64(base, off, v);
            }
            Opcode::LitNew(n) => {
                // As `js.rt.newobject`: the inline nursery allocation from
                // the site's cell, else the helper (which fills the cell).
                let cell = self.alloc_inline(inst, None)?;
                let nv = self.i32c(n);
                let live = self.live_across(inst);
                let (ok, r) = self.gc_call(self.h.new_object, &[cell, nv], &live)?;
                let t = self.edge(inst, 0, &[r])?;
                let e = self.edge(inst, 1, &[])?;
                self.cond_br(ok, t, e);
            }
            Opcode::LitInit(name, _) => {
                // As `js.rt.initprop`: the site's add transition replayed
                // inline, else the helper (which fills the row).
                let site = self.init_prop_inline(inst, a[0], a[1])?;
                let at = self.atom(name);
                let av = self.i32c(crate::wasm::mir::abi::INIT_ATTR_ENUMERATE);
                let sv = self.i32c(site);
                let live = self.live_across(inst);
                let (ok, _) = self.gc_call(self.h.init_prop, &[a[0], at, a[1], av, sv], &live)?;
                let t = self.edge(inst, 0, &[])?;
                let e = self.edge(inst, 1, &[])?;
                self.cond_br(ok, t, e);
            }
            Opcode::JsLambda(index) => {
                // Inline from the site's row: no GC, so the ok edge keeps
                // everything in registers. The helper (which fills the
                // row), and the rooting, only on the slow path.
                let env = self.box_tagged(TAG_OBJECT, a[0]);
                let cell = self.i32c(CONSTRUCT_CELL_ADDR_PLACEHOLDER);
                let idx = self.atoms.next_construct_cell();
                self.construct_cell_patches.push((cell, idx + 1));
                let slow = self.body.add_block();
                let r = self.lambda_inline(cell, env, slow);
                let t = self.edge(inst, 0, &[r])?;
                self.terminate(Terminator::Br { target: t });
                self.cur = slow;
                let script = self.script_ptr();
                let i = self.i32c(index);
                let live = self.live_across(inst);
                let (ok, r) = self.gc_call(self.h.lambda, &[env, script, i, cell], &live)?;
                let t = self.edge(inst, 0, &[r])?;
                let e = self.edge(inst, 1, &[])?;
                self.cond_br(ok, t, e);
            }
            Opcode::JsGetName(name) => {
                // A getter may run: clean only if no class word was
                // demoted meanwhile.
                if let Some(&bid) = self.gname_bids.get(&name) {
                    self.gname_fast_arms(inst, bid)?;
                }
                let at = self.atom(name);
                let z = self.i32c(0);
                self.js_call(inst, self.h.get_gname, &[at, z], false)?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
