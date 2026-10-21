//! op rare: the lowering of one run of arms of `Gen::op`, split out of `codegen.rs` unchanged.

use super::*;

impl<'a> Gen<'a> {
    pub(super) fn op_rare(&mut self, p: &mut BytecodeParser, op: JSOp) -> R<bool> {
        use JSOp::*;
        let d = self.d;
        let h = self.h;
        match op {
            // --- the rest (docs/BASELINE.md §6) ---
            BigInt => {
                let idx = p.next_uint32().unwrap();
                let script = self.script_ptr();
                let iv = self.i32c(idx);
                let r = self.rt(h.bigint, &[script, iv]);
                self.set_slot(d, r);
            }
            NonSyntacticGlobalThis => {
                let env = self.env();
                let r = self.rt(h.non_syntactic_global_this, &[env]);
                self.set_slot(d, r);
            }
            SetIntrinsic => {
                skip(p, op);
                let script = self.script_ptr();
                let pcv = self.i32c(self.pc.get());
                let v = self.slot(d - 1);
                self.rt(h.set_intrinsic, &[script, pcv, v]);
            }
            EnvCallee => {
                let hops = match op.len() {
                    2 => u32::from(p.next_uint8().unwrap()),
                    _ => u32::from(p.next_uint16().unwrap()),
                };
                let env = self.env();
                let hv = self.i32c(hops);
                let r = self
                    .call(h.env_callee, &[self.cx, env, hv], Some(Type::I64))
                    .unwrap();
                self.set_slot(d, r);
            }
            // Direct eval (`is_direct_eval`): `build_body` declines the script before
            // lowering, so this is not reached.
            Eval | StrictEval | SpreadEval | StrictSpreadEval => {
                return Err(format!("{} ({op:?})", super::super::UNSUPPORTED_OP));
            }
            DynamicImport => {
                let spec = self.slot(d - 2);
                let opts = self.slot(d - 1);
                let script = self.script_ptr();
                let r = self.rt(h.dynamic_import, &[script, spec, opts]);
                self.set_slot(d - 2, r);
            }
            ImportMeta => {
                let script = self.script_ptr();
                let r = self.rt(h.import_meta, &[script]);
                self.set_slot(d, r);
            }
            GetImport => {
                skip(p, op);
                let env = self.env();
                let script = self.script_ptr();
                let pcv = self.i32c(self.pc.get());
                let r = self.rt(h.get_import, &[env, script, pcv]);
                self.set_slot(d, r);
            }
            AddDisposable => {
                let hint = p.next_uint8().unwrap();
                let env = self.env();
                let v = self.slot(d - 3);
                let method = self.slot(d - 2);
                let nc = self.slot(d - 1);
                let hv = self.i32c(u32::from(hint));
                self.rt(h.add_disposable, &[env, v, method, nc, hv]);
            }
            TakeDisposeCapability => {
                let env = self.env();
                let r = self.rt(h.take_dispose_capability, &[env]);
                self.set_slot(d, r);
            }
            CreateSuppressedError => {
                let e = self.slot(d - 2);
                let sup = self.slot(d - 1);
                let r = self.rt(h.create_suppressed_error, &[e, sup]);
                self.set_slot(d - 2, r);
            }
            Resume => {
                let g = self.slot(d - 3);
                let v = self.slot(d - 2);
                let k = self.slot(d - 1);
                let r = self.rt(h.resume, &[g, v, k]);
                self.set_slot(d - 3, r);
            }
            ForceInterpreter => return Err(super::super::FORCE_INTERPRETER.into()),
            _ => return Ok(false),
        }
        Ok(true)
    }
}
