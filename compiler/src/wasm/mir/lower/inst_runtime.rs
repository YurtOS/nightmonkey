//! inst runtime: the lowering of one run of arms of `Lower::inst`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    pub(super) fn inst_runtime(
        &mut self,
        inst: mir::Inst,
        d: &mir::func::InstData,
        a: &[Value],
    ) -> R<bool> {
        match d.op {
            Opcode::JsRt(r) => {
                use crate::mir::ops::RtOp;
                let h = self.h;
                let (f, args) = match r {
                    RtOp::Instanceof => {
                        let cell = self.i32c(IOF_CELL_ADDR_PLACEHOLDER);
                        let idx = self.atoms.next_iof_cell();
                        self.iof_cell_patches.push((cell, idx + 1));
                        self.instanceof_arms(inst, a[0], a[1], cell)?;
                        (h.instanceof_, vec![a[0], a[1], cell])
                    }
                    RtOp::In => (h.in_, vec![a[0], a[1]]),
                    RtOp::HasOwn => {
                        self.has_own_arm(inst, a[0], a[1])?;
                        (h.has_own, vec![a[0], a[1]])
                    }
                    RtOp::DelProp(name, strict) => {
                        let (at, sv) = (self.atom(name), self.i32c(u32::from(strict)));
                        (h.del_prop, vec![a[0], at, sv])
                    }
                    RtOp::DelElem(strict) => {
                        let sv = self.i32c(u32::from(strict));
                        (h.del_elem, vec![a[0], a[1], sv])
                    }
                    RtOp::NewObject(n) => {
                        let cell = self.alloc_inline(inst, None)?;
                        let nv = self.i32c(n);
                        (h.new_object, vec![cell, nv])
                    }
                    RtOp::NewArray(len) => {
                        let cell = self.alloc_inline(inst, Some(len))?;
                        let lv = self.i32c(len);
                        (h.new_array, vec![lv, cell])
                    }
                    RtOp::InitProp(name, attrs) => {
                        let site = if attrs == crate::wasm::mir::abi::INIT_ATTR_ENUMERATE {
                            self.init_prop_inline(inst, a[0], a[1])?
                        } else {
                            u32::MAX
                        };
                        let (at, av, sv) = (self.atom(name), self.i32c(attrs), self.i32c(site));
                        (h.init_prop, vec![a[0], at, a[1], av, sv])
                    }
                    RtOp::InitElem(attrs, duty) => {
                        let key = self.f.insts[inst].args[1];
                        let index = match self.f.values[key].def {
                            mir::func::ValueDef::Result(i, _) => match self.f.insts[i].op {
                                Opcode::ConstVal(ConstVal::Int32(n)) => u32::try_from(n).ok(),
                                _ => None,
                            },
                            _ => None,
                        };
                        match index {
                            Some(i)
                                if attrs == crate::wasm::mir::abi::INIT_ATTR_ENUMERATE
                                    && i < crate::constants::INLINE_INIT_ELEM_CAP =>
                            {
                                self.init_elem_inline(inst, a[0], i, a[2], duty)?
                            }
                            _ => {}
                        }
                        let av = self.i32c(attrs);
                        (h.init_elem, vec![a[0], a[1], a[2], av])
                    }
                    RtOp::ToPropertyKey => (h.to_property_key, vec![a[0]]),
                    RtOp::RegExp(idx) => {
                        let script = self.script_ptr();
                        let iv = self.i32c(idx);
                        (h.regexp, vec![script, iv])
                    }
                    RtOp::ToString => {
                        // A string is its own; else the helper.
                        let tag = self.tag_of(a[0]);
                        let s = self.tag_is(tag, TAG_STRING as u32);
                        let (hit, miss) = (self.body.add_block(), self.body.add_block());
                        self.cond_br(s, Self::to(hit), Self::to(miss));
                        self.cur = hit;
                        let t = self.edge(inst, 0, &[a[0]])?;
                        self.terminate(Terminator::Br { target: t });
                        self.cur = miss;
                        (h.tostring, vec![a[0]])
                    }
                    RtOp::Symbol(code) => {
                        // A well-known symbol: permanent, from a leaf.
                        let cv = self.i32c(code);
                        let r = self.call1(h.symbol, &[self.cx, cv], Type::I64);
                        let t = self.edge(inst, 0, &[r])?;
                        self.terminate(Terminator::Br { target: t });
                        return Ok(true);
                    }
                    RtOp::Intrinsic(_) | RtOp::BuiltinObject(_) => {
                        // A realm constant: the cell once armed; the helper
                        // resolves it and arms it.
                        let (id, row, helper) = match r {
                            RtOp::Intrinsic(name) => {
                                let id = self.atoms.intern_chars(self.mm.atoms[name].chars());
                                (id, self.atoms.intrinsic_cell(id), h.get_intrinsic_cell)
                            }
                            RtOp::BuiltinObject(kind) => (
                                kind,
                                self.atoms.builtin_object_cell(kind),
                                h.builtin_object_cell,
                            ),
                            _ => unreachable!(),
                        };
                        let cell =
                            self.i32c(crate::wasm::mir::abi::INTRINSIC_CELL_ADDR_PLACEHOLDER);
                        self.intrinsic_cell_patches.push((cell, row));
                        let bits = self.load_i64(cell, 0);
                        let z = self.i64c(0);
                        let armed = self.bin(Operator::I64Ne, bits, z, Type::I32);
                        let (hit, miss) = (self.body.add_block(), self.body.add_block());
                        self.cond_br(armed, Self::to(hit), Self::to(miss));
                        self.cur = hit;
                        let t = self.edge(inst, 0, &[bits])?;
                        self.terminate(Terminator::Br { target: t });
                        self.cur = miss;
                        let iv = self.i32c(id);
                        (helper, vec![iv, cell])
                    }
                    RtOp::InitPropGetSet(name, kind) => {
                        let (at, kv) = (self.atom(name), self.i32c(kind));
                        (h.init_prop_getset, vec![a[0], at, a[1], kv])
                    }
                    RtOp::Iter => {
                        self.iter_from_shape(inst, a[0])?;
                        (h.iter_, vec![a[0]])
                    }
                    RtOp::Check(k) => {
                        let f = match k {
                            mir::ops::CHECK_OBJ_COERCIBLE => h.check_obj_coercible,
                            mir::ops::CHECK_CLASS_HERITAGE => h.check_class_heritage,
                            mir::ops::CHECK_THIS_REINIT => h.check_this_reinit,
                            _ => h.check_this,
                        };
                        (f, vec![a[0]])
                    }
                    RtOp::SetFunName(prefix) => {
                        let k = self.i32c(prefix);
                        (h.set_fun_name, vec![a[0], a[1], k])
                    }
                    RtOp::GlobalThis => (h.global_this, vec![]),
                    RtOp::BigInt(idx) => {
                        let script = self.script_ptr();
                        let iv = self.i32c(idx);
                        (h.bigint, vec![script, iv])
                    }
                    RtOp::MutateProto => (h.mutate_proto, vec![a[0], a[1]]),
                    RtOp::CheckIsObj(kind) => {
                        let k = self.i32c(kind);
                        (h.check_is_obj, vec![a[0], k])
                    }
                    RtOp::CloseIter(kind) => {
                        let k = self.i32c(kind);
                        (h.close_iter, vec![a[0], k])
                    }
                    RtOp::OptimizeSpreadCall => (h.optimize_spread_call, vec![a[0]]),
                    RtOp::PushEnv(kind, pc) => {
                        let f = match kind {
                            mir::ops::ENV_LEXICAL => h.push_lexical_env,
                            mir::ops::ENV_CLASS_BODY => h.push_class_body_env,
                            _ => h.push_var_env,
                        };
                        let env = self.frame_env();
                        let script = self.script_ptr();
                        let pcv = self.i32c(pc);
                        (f, vec![env, script, pcv])
                    }
                    RtOp::EnterWith(pc) => {
                        let env = self.frame_env();
                        let script = self.script_ptr();
                        let pcv = self.i32c(pc);
                        (h.enter_with, vec![env, a[0], script, pcv])
                    }
                    RtOp::FreshenEnv(recreate) => {
                        let f = if recreate != 0 {
                            h.recreate_lexical_env
                        } else {
                            h.freshen_lexical_env
                        };
                        (f, vec![self.frame_env()])
                    }
                    RtOp::GetName(name, for_typeof) => {
                        let env = self.frame_env();
                        let (at, tv) = (self.atom(name), self.i32c(for_typeof));
                        (h.get_name, vec![env, at, tv])
                    }
                    RtOp::BindName(name, unqualified) => {
                        let f = if unqualified != 0 {
                            h.bind_unqualified_name
                        } else {
                            h.bind_name
                        };
                        let env = self.frame_env();
                        (f, vec![env, self.atom(name)])
                    }
                    RtOp::DelName(name) => {
                        let env = self.frame_env();
                        (h.del_name, vec![env, self.atom(name)])
                    }
                    RtOp::BindVar => (h.bind_var, vec![self.frame_env()]),
                    RtOp::SuperBase => (h.super_base, vec![a[0]]),
                    RtOp::SuperFun => (h.super_fun, vec![a[0]]),
                    RtOp::GetPropSuper(name) => {
                        (h.get_prop_super, vec![a[0], a[1], self.atom(name)])
                    }
                    RtOp::GetElemSuper => (h.get_elem_super, vec![a[0], a[1], a[2]]),
                    RtOp::SetPropSuper(name, strict) => {
                        let (at, sv) = (self.atom(name), self.i32c(u32::from(strict)));
                        (h.set_prop_super, vec![a[0], a[1], at, a[2], sv])
                    }
                    RtOp::SetElemSuper(strict) => {
                        let sv = self.i32c(u32::from(strict));
                        (h.set_elem_super, vec![a[0], a[1], a[2], a[3], sv])
                    }
                    RtOp::InitHomeObject => (h.init_home_object, vec![a[0], a[1]]),
                    RtOp::FunWithProto(index) => {
                        let env = self.frame_env();
                        let script = self.script_ptr();
                        let iv = self.i32c(index);
                        (h.fun_with_proto, vec![env, a[0], script, iv])
                    }
                    RtOp::CheckReturn => (h.check_return, vec![a[0], a[1]]),
                    RtOp::CreateGenerator => {
                        let callee = self.load_i64(self.sp, FrameLayout::CALLEE);
                        let env = if self.plain_env || self.own_env {
                            self.frame_env()
                        } else {
                            self.i64c(UNDEF)
                        };
                        (h.create_generator, vec![callee, env])
                    }
                    RtOp::GenFinal => (h.gen_final, vec![a[0]]),
                    RtOp::GenCheckResume => {
                        let k = self.un(Operator::I32WrapI64, a[2], Type::I32);
                        let rval = self.add_off(
                            self.vp,
                            self.frame_voff[self.cur_frame as usize] + self.layout.rval(),
                        );
                        (h.gen_check_resume, vec![a[1], a[0], k, rval])
                    }
                    RtOp::AsyncAwait(resolve) => {
                        let f = if resolve != 0 {
                            h.async_resolve
                        } else {
                            h.async_await
                        };
                        (f, vec![a[1], a[0]])
                    }
                    RtOp::AsyncReject => (h.async_reject, vec![a[2], a[0], a[1]]),
                    RtOp::CanSkipAwait => (h.can_skip_await, vec![a[0]]),
                    RtOp::MaybeExtractAwait => {
                        let can = self.un(Operator::I32WrapI64, a[1], Type::I32);
                        (h.maybe_extract_await, vec![a[0], can])
                    }
                    RtOp::Resume => (h.resume, vec![a[0], a[1], a[2]]),
                    RtOp::AddDisposable(hint) => {
                        let env = self.frame_env();
                        let hv = self.i32c(hint);
                        (h.add_disposable, vec![env, a[0], a[1], a[2], hv])
                    }
                    RtOp::TakeDisposeCapability => {
                        (h.take_dispose_capability, vec![self.frame_env()])
                    }
                    RtOp::CreateSuppressedError => (h.create_suppressed_error, vec![a[0], a[1]]),
                    RtOp::GetBoundName(name) => (h.get_bound_name, vec![a[0], self.atom(name)]),
                    RtOp::ObjWithProto => (h.obj_with_proto, vec![a[0]]),
                    RtOp::NewPrivateName(name) => (h.new_private_name, vec![self.atom(name)]),
                    RtOp::DynamicImport => {
                        let script = self.script_ptr();
                        (h.dynamic_import, vec![script, a[0], a[1]])
                    }
                    RtOp::SpreadEval(pc) => {
                        let env = self.frame_env();
                        let script = self.script_ptr();
                        let pcv = self.i32c(pc);
                        (h.spread_eval, vec![a[0], a[1], a[2], env, script, pcv])
                    }
                    RtOp::InitElemGetSet(kind) => {
                        let kv = self.i32c(kind);
                        (h.init_elem_getset, vec![a[0], a[1], a[2], kv])
                    }
                    RtOp::SetName(name, strict) => {
                        let (at, sv) = (self.atom(name), self.i32c(u32::from(strict)));
                        (h.set_name, vec![a[0], at, a[1], sv])
                    }
                    RtOp::SpreadCall(construct) => {
                        let c = self.i32c(construct);
                        (h.spread_call, vec![a[0], a[1], a[2], a[3], c])
                    }
                    RtOp::CheckPrivateField(cond, kind) => {
                        let (cv, kv) = (self.i32c(cond), self.i32c(kind));
                        (h.check_private_field, vec![a[0], a[1], cv, kv])
                    }
                    RtOp::GetNameTypeof(name) => {
                        // `get_gname`'s typeof form: unbound is undefined.
                        let (at, one) = (self.atom(name), self.i32c(1));
                        (h.get_gname, vec![at, one])
                    }
                };
                self.js_call(inst, f, &args, false)?;
            }
            Opcode::JsThrow => {
                // The helper sets the pending exception and always fails.
                // It may GC (capturing the stack): root what its throw exit
                // reads.
                let live = self.live_across(inst);
                self.root(&live)?;
                let top_off = self.top_off(live.len());
                let top = self.add_off(self.vp, top_off);
                self.call(self.h.throw, &[self.cx, top, a[0]], &[]);
                self.after_gc(&live);
                let e = self.edge(inst, 0, &[])?;
                self.terminate(Terminator::Br { target: e });
            }
            Opcode::ArgsObject => {
                // The frame caches it, as baseline does, so both tiers
                // (and repeated reads) see one object.
                let fid = self.cur_frame as usize;
                let l = self.frame_layouts[fid];
                let slot = self.frame_voff[fid] + l.args_obj();
                let (asp, argc) = self.actuals();
                let cached = self.load_i64(self.vp, slot);
                let tag = self.tag_of(cached);
                let undef = self.tag_is(tag, TAG_UNDEFINED as u32);
                let (build, have) = (self.body.add_block(), self.body.add_block());
                self.cond_br(undef, Self::to(build), Self::to(have));
                self.cur = have;
                let t = self.edge(inst, 0, &[cached])?;
                self.terminate(Terminator::Br { target: t });
                self.cur = build;
                let live = self.live_across(inst);
                // With an environment, the runtime is given it, as
                // baseline's prologue does: a mapped object in a function
                // with a call object records it (`MaybeForwardToCallObject`).
                // (An inlined callee's object is unmapped: no env.)
                let (ok, r) = if fid == 0 && (self.plain_env || self.own_env) {
                    let env = self.load_i64(self.vp, l.env());
                    self.gc_call(self.h.arguments_env, &[asp, argc, env], &live)?
                } else {
                    self.gc_call(self.h.arguments_, &[asp, argc], &live)?
                };
                let (store, e) = (self.body.add_block(), self.edge(inst, 1, &[])?);
                self.cond_br(ok, Self::to(store), e);
                self.cur = store;
                self.store_i64(self.vp, slot, r);
                let t = self.edge(inst, 0, &[r])?;
                self.terminate(Terminator::Br { target: t });
            }
            Opcode::RestArray(n) => {
                let live = self.live_across(inst);
                let nv = self.i32c(n);
                let (asp, argc) = self.actuals();
                let (ok, r) = self.gc_call(self.h.rest, &[asp, argc, nv], &live)?;
                let t = self.edge(inst, 0, &[r])?;
                let e = self.edge(inst, 1, &[])?;
                self.cond_br(ok, t, e);
            }
            Opcode::ArgsLength => {
                let (_, argc) = self.actuals();
                self.def(inst, argc);
            }
            Opcode::IterMore => {
                let r = self.iter_more(a[0]);
                self.def(inst, r);
            }
            Opcode::IterEnd => self.iter_end(a[0]),
            Opcode::IterOptimizable => {
                let r = self.call1(self.h.optimize_get_iterator, &[self.cx, a[0]], Type::I32);
                self.def(inst, r);
            }
            Opcode::IterIsDone => {
                let m = self.i64c((TAG_MAGIC << 32) | crate::wasm::translate::MAGIC_NO_ITER_VALUE);
                let r = self.bin(Operator::I64Eq, a[0], m, Type::I32);
                self.def(inst, r);
            }
            Opcode::FrameCallee => {
                let fid = self.cur_frame as usize;
                let v = if fid == 0 {
                    self.load_i64(self.sp, FrameLayout::CALLEE)
                } else {
                    self.load_i64(self.vp, self.frame_off[fid] + FrameLayout::CALLEE)
                };
                self.def(inst, v);
            }
            Opcode::FrameNewTarget => {
                let fid = self.cur_frame as usize;
                let off = self.frame_voff[fid] + self.frame_layouts[fid].new_target();
                let v = self.load_i64(self.vp, off);
                self.def(inst, v);
            }
            Opcode::ActualArgOr(k) => {
                let (asp, argc) = self.actuals();
                let v = self.load_i64(asp, FrameLayout::ARGS + 8 * k);
                let kv = self.i32c(k);
                let have = self.bin(Operator::I32LtU, kv, argc, Type::I32);
                let undef = self.i64c(UNDEF);
                let r = self.select(Type::I64, v, undef, have);
                self.def(inst, r);
            }
            Opcode::JsIsBuiltin(k) => {
                let cell = self.i32c(self.h.builtin_cells_base + 8 * k);
                let bits = self.load_i64(cell, 0);
                let r = self.bin(Operator::I64Eq, a[0], bits, Type::I32);
                self.def(inst, r);
            }
            Opcode::ApplyFwd => {
                let (asp, argc) = self.actuals();
                self.js_call(
                    inst,
                    self.h.apply_fwd,
                    &[a[0], a[1], a[2], asp, argc],
                    false,
                )?;
            }
            Opcode::ActualArg => {
                let eight = self.i32c(8);
                let off = self.bin(Operator::I32Mul, a[0], eight, Type::I32);
                let (asp, _) = self.actuals();
                let addr = self.bin(Operator::I32Add, asp, off, Type::I32);
                let v = self.load_i64(addr, FrameLayout::ARGS);
                self.def(inst, v);
            }
            Opcode::JsTypeof => {
                // A leaf: the type's name is an atom.
                let v = self.call(self.h.typeof_, &[self.cx, a[0]], &[Type::I64]);
                self.def(inst, v);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
