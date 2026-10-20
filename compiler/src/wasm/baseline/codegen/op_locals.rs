//! op locals: the lowering of one run of arms of `Gen::op`, split out of `codegen.rs` unchanged.

use super::*;

impl<'a> Gen<'a> {
    pub(super) fn op_locals(&mut self, p: &mut BytecodeParser, op: JSOp) -> R<bool> {
        use JSOp::*;
        let d = self.d;
        let h = self.h;
        match op {
            Arguments => self.arguments(),
            GetArg | GetFrameArg => {
                let n = u32::from(p.next_uint16().unwrap());
                let v = if op == GetArg && self.script.has_mapped_args {
                    let obj = self.load_i64(self.vp, self.layout.args_obj());
                    let i = self.i32c(n);
                    self.call(h.get_mapped_arg, &[obj, i], Some(Type::I64))
                        .unwrap()
                } else {
                    self.load_i64(self.sp, self.layout.arg(n))
                };
                self.set_slot(d, v);
            }
            SetArg => {
                let n = u32::from(p.next_uint16().unwrap());
                let v = self.slot(d - 1);
                if self.script.has_mapped_args {
                    let obj = self.load_i64(self.vp, self.layout.args_obj());
                    let i = self.i32c(n);
                    self.call(h.set_mapped_arg, &[obj, i, v], None);
                } else {
                    self.store_i64(self.sp, self.layout.arg(n), v);
                }
            }
            GetLocal => {
                let n = p.next_uint24().unwrap();
                let v = self.load_i64(self.vp, self.layout.local(n));
                self.set_slot(d, v);
            }
            SetLocal | InitLexical => {
                let n = p.next_uint24().unwrap();
                let v = self.slot(d - 1);
                self.store_i64(self.vp, self.layout.local(n), v);
            }
            GetRval => {
                let v = self.load_i64(self.vp, self.layout.rval());
                self.set_slot(d, v);
            }
            SetRval => {
                let v = self.slot(d - 1);
                self.store_i64(self.vp, self.layout.rval(), v);
            }

            // --- environments ---
            GetAliasedVar | GetAliasedDebugVar => {
                let hops = u32::from(p.next_uint16().unwrap());
                let s = p.next_uint24().unwrap();
                let env = self.env();
                let (hv, sv) = (self.i32c(hops), self.i32c(s));
                let v = self
                    .call(h.get_aliased, &[self.cx, env, hv, sv], Some(Type::I64))
                    .unwrap();
                self.set_slot(d, v);
            }
            SetAliasedVar | InitAliasedLexical => {
                let hops = u32::from(p.next_uint16().unwrap());
                let s = p.next_uint24().unwrap();
                let env = self.env();
                let (hv, sv) = (self.i32c(hops), self.i32c(s));
                let v = self.slot(d - 1);
                self.call(h.set_aliased, &[self.cx, env, hv, sv, v], None);
            }
            Lambda => {
                let fi = p.next_uint32().unwrap();
                let env = self.env();
                let script = self.script_ptr();
                let f = self.i32c(fi);
                let no_cell = self.i32c(0);
                let r = self.rt(h.lambda, &[env, script, f, no_cell]);
                self.set_slot(d, r);
            }
            FunWithProto => {
                let fi = p.next_uint32().unwrap();
                let proto = self.slot(d - 1);
                let env = self.env();
                let script = self.script_ptr();
                let f = self.i32c(fi);
                let r = self.rt(h.fun_with_proto, &[env, proto, script, f]);
                self.set_slot(d - 1, r);
            }
            PushLexicalEnv | PushClassBodyEnv | PushVarEnv => {
                skip(p, op);
                let f = match op {
                    PushLexicalEnv => h.push_lexical_env,
                    PushClassBodyEnv => h.push_class_body_env,
                    _ => h.push_var_env,
                };
                let env = self.env();
                let script = self.script_ptr();
                let pcv = self.i32c(self.pc.get());
                let r = self.rt(f, &[env, script, pcv]);
                self.set_env(r);
            }
            EnterWith => {
                skip(p, op);
                let v = self.slot(d - 1);
                let env = self.env();
                let script = self.script_ptr();
                let pcv = self.i32c(self.pc.get());
                let r = self.rt(h.enter_with, &[env, v, script, pcv]);
                self.set_env(r);
            }
            FreshenLexicalEnv | RecreateLexicalEnv => {
                skip(p, op);
                let f = if op == FreshenLexicalEnv {
                    h.freshen_lexical_env
                } else {
                    h.recreate_lexical_env
                };
                let env = self.env();
                let r = self.rt(f, &[env]);
                self.set_env(r);
            }
            PopLexicalEnv | LeaveWith => {
                let env = self.env();
                let ptr = self.unop(Operator::I32WrapI64, env, Type::I32);
                let enclosing = self.load_i64(ptr, FIXED_SLOTS_BASE);
                self.set_env(enclosing);
            }
            SetFunName => {
                let prefix = p.next_uint8().unwrap();
                let name = self.slot(d - 1);
                let fun = self.slot(d - 2);
                let k = self.i32c(u32::from(prefix));
                self.rt(h.set_fun_name, &[fun, name, k]);
            }
            InitGLexical => {
                skip(p, op);
                let v = self.slot(d - 1);
                let script = self.script_ptr();
                let pcv = self.i32c(self.pc.get());
                self.rt(h.init_glexical, &[v, script, pcv]);
            }
            GetName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let for_typeof = self.next_is_typeof(op);
                let env = self.env();
                let (av, tv) = (self.i32c(a), self.i32c(u32::from(for_typeof)));
                let r = self.rt(h.get_name, &[env, av, tv]);
                self.set_slot(d, r);
            }
            BindName | BindUnqualifiedName | DelName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let f = match op {
                    BindName => h.bind_name,
                    BindUnqualifiedName => h.bind_unqualified_name,
                    _ => h.del_name,
                };
                let env = self.env();
                let av = self.i32c(a);
                let r = self.rt(f, &[env, av]);
                self.set_slot(d, r);
            }
            GetBoundName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let env = self.slot(d - 1);
                let av = self.i32c(a);
                let r = self.rt(h.get_bound_name, &[env, av]);
                self.set_slot(d - 1, r);
            }
            BindVar => {
                let env = self.env();
                let r = self.rt(h.bind_var, &[env]);
                self.set_slot(d, r);
            }
            GlobalOrEvalDeclInstantiation => {
                let idx = p.next_uint32().unwrap();
                let script = self.script_ptr();
                let iv = self.i32c(idx);
                self.rt(h.global_decl_instantiation, &[script, iv]);
            }
            GetGName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let for_typeof = self.next_is_typeof(op);
                let (av, tv) = (self.i32c(a), self.i32c(u32::from(for_typeof)));
                let r = self.rt(h.get_gname, &[av, tv]);
                self.set_slot(d, r);
            }
            BindUnqualifiedGName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let av = self.i32c(a);
                let r = self.rt(h.bind_unqualified_gname, &[av]);
                self.set_slot(d, r);
            }
            SetGName | StrictSetGName | SetName | StrictSetName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let strict = matches!(op, StrictSetGName | StrictSetName);
                let env = self.slot(d - 2);
                let v = self.slot(d - 1);
                let (av, sv) = (self.i32c(a), self.i32c(u32::from(strict)));
                self.rt(h.set_name, &[env, av, v, sv]);
                let v = self.slot(d - 1);
                self.set_slot(d - 2, v);
            }

            // --- stack manipulation ---
            Pop => {}
            PopN => {
                p.next_uint16().unwrap();
            }
            Dup => {
                let v = self.slot(d - 1);
                self.set_slot(d, v);
            }
            Dup2 => {
                let a = self.slot(d - 2);
                let b = self.slot(d - 1);
                self.set_slot(d, a);
                self.set_slot(d + 1, b);
            }
            DupAt => {
                let n = p.next_uint24().unwrap();
                let v = self.slot(d - 1 - n);
                self.set_slot(d, v);
            }
            Swap => {
                let a = self.slot(d - 2);
                let b = self.slot(d - 1);
                self.set_slot(d - 2, b);
                self.set_slot(d - 1, a);
            }
            Pick => {
                // Move slot d-1-n to the top, shifting the ones above down.
                let n = u32::from(p.next_uint8().unwrap());
                let v = self.slot(d - 1 - n);
                for k in (d - 1 - n)..(d - 1) {
                    let w = self.slot(k + 1);
                    self.set_slot(k, w);
                }
                self.set_slot(d - 1, v);
            }
            Unpick => {
                // Move the top down to slot d-1-n, shifting the ones there up.
                let n = u32::from(p.next_uint8().unwrap());
                let v = self.slot(d - 1);
                for k in ((d - 1 - n)..(d - 1)).rev() {
                    let w = self.slot(k);
                    self.set_slot(k + 1, w);
                }
                self.set_slot(d - 1 - n, v);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
