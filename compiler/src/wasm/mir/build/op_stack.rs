//! op stack: the lowering of one run of arms of `Run::op`, split out of `build.rs` unchanged.

use super::*;

impl<'s, 'a> Run<'s, 'a> {
    pub(super) fn op_stack(&mut self, op: JSOp, pc: Pc, p: &mut BytecodeParser<'a>) -> R<bool> {
        use JSOp::*;
        let int_ty = Ty::I32;
        match op {
            // (`DebugLeaveLexicalEnv` only matters to a debugger; so does
            // `Debugger`, a no-op with none attached, as in baseline.)
            Nop | Lineno | JumpTarget | LoopHead | NopDestructuring | NopIsAssignOp
            | DebugLeaveLexicalEnv | Debugger => {}
            // A try block's code is ordinary code: a throw in it exits
            // (`exit.throw`) and baseline takes the pc's handler. The catch
            // code, entered only by a throw, is never reached here.
            Try | TryDestructuring => {}
            // A finally block's start: a marker. MIR runs only its normal
            // entry (a throw exits, and baseline's unwind enters it
            // throwing), whose rethrow arm is therefore baseline's too.
            Finally => {}
            ThrowWithStack => {
                let b = self.exit_block(false);
                self.term(Opcode::Jump, vec![], vec![Self::goto(b)]);
            }

            Undefined => {
                let v = self.const_val(ConstVal::Undefined);
                self.push(v, Ty::Val(TagSet::prims(PRIM_UNDEFINED)));
            }
            Null => {
                let v = self.const_val(ConstVal::Null);
                self.push(v, Ty::Val(TagSet::prims(PRIM_NULL)));
            }
            True | False => {
                let v = self.inst(Opcode::ConstBool(op == True), vec![], Some(MType::Bool));
                self.push(v, Ty::Bool);
            }
            Zero | One | Int8 | Uint16 | Uint24 | Int32 => {
                let n: i32 = match op {
                    Zero => 0,
                    One => 1,
                    Int8 => p.next_int8().unwrap().into(),
                    Uint16 => p.next_uint16().unwrap().into(),
                    Uint24 => p.next_uint24().unwrap() as i32,
                    _ => p.next_int32().unwrap(),
                };
                let v = self.const_i32(n);
                self.push(v, int_ty);
            }
            Double => {
                let x = f64::from_bits(p.next_uint64().unwrap());
                let v = self.const_f64(x);
                self.push(v, Ty::F64);
            }
            Uninitialized => {
                let v = self.const_val(ConstVal::Uninitialized);
                self.push(v, Ty::Val(TagSet::MAGIC));
            }
            Void => {
                self.pop();
                let v = self.const_val(ConstVal::Undefined);
                self.push(v, Ty::Val(TagSet::prims(PRIM_UNDEFINED)));
            }

            // --- frame ---
            GetLocal => {
                let n = p.next_uint24().unwrap();
                let ix = self.local_ix(n);
                let x = self.st[ix];
                // A read of the elided arguments object's local: an exit
                // before the op consuming it makes the object for it too.
                if self.args_locals.contains(&ix) {
                    self.args_copies.insert(x.v);
                }
                if let Some(&c) = self.slot_cls.get(&ix) {
                    self.val_cls.insert(x.v, c);
                }
                if self.this_slots.contains(&ix) {
                    self.this_vals.insert(x.v);
                }
                self.st.push(x);
            }
            SetLocal | InitLexical => {
                let n = p.next_uint24().unwrap();
                let ix = self.local_ix(n);
                let x = self.top();
                self.set_frame_slot(ix, x);
            }
            // With an unmapped `arguments` (or none to map), a formal's
            // frame slot is the formal.
            // A mapped `arguments` object aliases the formals: they are
            // its (baseline's reads and writes too).
            GetArg if self.mapped() => {
                let n = u32::from(p.next_uint16().unwrap());
                let v = self.inst(Opcode::ArgsMapped(n), vec![], Some(MType::VAL_TOP));
                self.push(v, Ty::Val(TagSet::ALL));
            }
            GetArg | GetFrameArg => {
                let n = u32::from(p.next_uint16().unwrap());
                let ix = self.arg_ix(n);
                let x = self.st[ix];
                if let Some(&c) = self.slot_cls.get(&ix) {
                    self.val_cls.insert(x.v, c);
                }
                self.st.push(x);
            }
            ToString => {
                // A string is its own ToString.
                let v = self.top();
                if !v.ty.tags().is_nonempty_subset_of(TagSet::STRING) {
                    let v = self.pop();
                    let x = self.boxed(v);
                    let r = self.js(
                        Opcode::JsRt(RtOp::ToString),
                        vec![x],
                        MType::val(TagSet::STRING),
                    );
                    self.push(r, Ty::Val(TagSet::STRING));
                }
            }
            BuiltinObject => {
                let kind = u32::from(p.next_uint8().unwrap());
                let r = self.js(
                    Opcode::JsRt(RtOp::BuiltinObject(kind)),
                    vec![],
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            Symbol => {
                let code = u32::from(p.next_uint8().unwrap());
                let sym = TagSet::prims(crate::opsem::PRIM_SYMBOL);
                let r = self.js(Opcode::JsRt(RtOp::Symbol(code)), vec![], MType::val(sym));
                self.push(r, Ty::Val(sym));
            }
            GetIntrinsic => {
                // A realm constant, from its cell once armed.
                let a = self.atom(p.next_uint32().unwrap())?;
                let r = self.js(Opcode::JsRt(RtOp::Intrinsic(a)), vec![], MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
            }
            RegExp => {
                let idx = p.next_uint32().unwrap();
                let r = self.js(
                    Opcode::JsRt(RtOp::RegExp(idx)),
                    vec![],
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            InitPropGetter | InitHiddenPropGetter | InitPropSetter | InitHiddenPropSetter => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let kind = u32::from(matches!(op, InitPropSetter | InitHiddenPropSetter))
                    | (u32::from(matches!(op, InitHiddenPropGetter | InitHiddenPropSetter)) << 1);
                let f = self.pop();
                let o = self.top();
                let (x, y) = (self.boxed(o), self.boxed(f));
                self.js_void(Opcode::JsRt(RtOp::InitPropGetSet(a, kind)), vec![x, y]);
            }
            SetArg if self.mapped() => {
                let n = u32::from(p.next_uint16().unwrap());
                let x = self.top();
                let v = self.boxed(x);
                self.inst(Opcode::ArgsMappedSet(n), vec![v], None);
            }
            SetArg => {
                let n = u32::from(p.next_uint16().unwrap());
                let ix = self.arg_ix(n);
                let x = self.top();
                self.set_frame_slot(ix, x);
            }
            GetRval => {
                let x = self.st[self.rval_ix()];
                self.st.push(x);
            }
            SetRval => {
                let x = self.pop();
                let ix = self.rval_ix();
                self.set_frame_slot(ix, x);
            }
            GetAliasedVar | GetAliasedDebugVar => {
                let hops = p.next_uint16().unwrap();
                let slot = p.next_uint24().unwrap();
                let e = self.env_at(hops);
                let v = self.inst(
                    Opcode::EnvLoad(crate::ids::EnvSlot::new(slot)),
                    vec![e],
                    Some(MType::VAL_TOP),
                );
                self.push(v, Ty::Val(TagSet::ALL));
                let claim = self.s.ctx.facts.aliased_sites.get(&self.site(pc)).copied();
                self.guard_result(claim.unwrap_or_default(), pc + op.len(), true);
            }
            SetAliasedVar | InitAliasedLexical => {
                let hops = p.next_uint16().unwrap();
                let slot = p.next_uint24().unwrap();
                let x = self.top();
                let v = self.boxed(x);
                let e = self.env_at(hops);
                self.inst(
                    Opcode::EnvStore(crate::ids::EnvSlot::new(slot)),
                    vec![e, v],
                    None,
                );
            }
            Lambda => {
                let index = p.next_uint32().unwrap();
                let e = self.env_at(0);
                let r =
                    self.js_static(Opcode::JsLambda(index), vec![e], MType::val(TagSet::OBJECT));
                let ty = match self.s.script.gcthings.get(index as usize) {
                    Some(&gc) if !gc.is_other() => {
                        match self.s.ctx.source.object(gc) {
                            // The function gcthing names its script.
                            crate::source::SourceObject::Object(o) => match o.script {
                                Some(k) => Ty::Fn(ScriptId::new(k.id())),
                                None => Ty::Val(TagSet::OBJECT),
                            },
                            crate::source::SourceObject::Script(_) => {
                                Ty::Fn(ScriptId::new(gc.id()))
                            }
                            _ => Ty::Val(TagSet::OBJECT),
                        }
                    }
                    _ => Ty::Val(TagSet::OBJECT),
                };
                self.push(r, ty);
            }
            CheckLexical | CheckAliasedLexical => {
                // The top may be the TDZ sentinel only if its type admits
                // magic; then guard it away (baseline throws on failure).
                let x = self.top();
                if let Ty::Val(t) = x.ty {
                    if t.magic {
                        let js = TagSet { magic: false, ..t };
                        if js.is_empty() {
                            // Always uninitialized: baseline throws the
                            // ReferenceError at this pc.
                            let b = self.exit_block(false);
                            self.term(Opcode::Jump, vec![], vec![Self::goto(b)]);
                            return Ok(true);
                        }
                        let v = self.guard(Opcode::GuardTags(js), vec![x.v], MType::val(js));
                        self.st.pop();
                        self.push(v, Ty::Val(js));
                    }
                }
            }

            // --- stack ---
            Pop => {
                self.pop();
            }
            PopN => {
                let n = p.next_uint16().unwrap();
                for _ in 0..n {
                    self.pop();
                }
            }
            Dup if self.string_switch()? => {}
            Dup => {
                let x = self.top();
                self.st.push(x);
            }
            Dup2 => {
                let n = self.st.len();
                let (a, b) = (self.st[n - 2], self.st[n - 1]);
                self.st.push(a);
                self.st.push(b);
            }
            DupAt => {
                let n = p.next_uint24().unwrap() as usize;
                let x = self.st[self.st.len() - 1 - n];
                self.st.push(x);
            }
            Swap => {
                let n = self.st.len();
                self.st.swap(n - 1, n - 2);
            }
            Pick => {
                let n = usize::from(p.next_uint8().unwrap());
                let ix = self.st.len() - 1 - n;
                let x = self.st.remove(ix);
                self.st.push(x);
            }
            Unpick => {
                let n = usize::from(p.next_uint8().unwrap());
                let x = self.pop();
                let ix = self.st.len() - n;
                self.st.insert(ix, x);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
