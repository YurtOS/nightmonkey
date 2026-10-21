//! op misc: the lowering of one run of arms of `Run::op`, split out of `build.rs` unchanged.

use super::*;

impl<'s, 'a> Run<'s, 'a> {
    pub(super) fn op_misc(&mut self, op: JSOp, pc: Pc, p: &mut BytecodeParser<'a>) -> R<bool> {
        use JSOp::*;
        match op {
            Iter => {
                // For-in (baseline's helpers): the property iterator; the
                // loop's names from it; its close. A throw inside exits,
                // and baseline's unwind closes it.
                let x = self.pop();
                let v = self.boxed(x);
                let r = self.js(
                    Opcode::JsRt(RtOp::Iter),
                    vec![v],
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            MoreIter => {
                let it = self.top();
                let v = self.boxed(it);
                let r = self.inst(Opcode::IterMore, vec![v], Some(MType::VAL_TOP));
                self.push(r, Ty::Val(TagSet::ALL));
            }
            IsNoIter => {
                let x = self.top();
                let v = self.boxed(x);
                let b = self.inst(Opcode::IterIsDone, vec![v], Some(MType::Bool));
                self.push(b, Ty::Bool);
            }
            EndIter => {
                self.pop();
                let it = self.pop();
                let v = self.boxed(it);
                self.inst(Opcode::IterEnd, vec![v], None);
            }
            Callee => {
                let v = self.inst(
                    Opcode::FrameCallee,
                    vec![],
                    Some(MType::val(TagSet::OBJECT)),
                );
                self.push(v, Ty::Val(TagSet::OBJECT));
            }
            CheckObjCoercible | CheckClassHeritage | CheckThis => {
                // The runtime's check of the value, which stays.
                let k = match op {
                    CheckObjCoercible => mir::ops::CHECK_OBJ_COERCIBLE,
                    CheckClassHeritage => mir::ops::CHECK_CLASS_HERITAGE,
                    _ => mir::ops::CHECK_THIS,
                };
                let x = self.top();
                let v = self.boxed(x);
                self.js_void(Opcode::JsRt(RtOp::Check(k)), vec![v]);
            }
            SetFunName => {
                let prefix = u32::from(p.next_uint8().unwrap());
                let name = self.pop();
                let fun = self.top();
                let (f, n) = (self.boxed(fun), self.boxed(name));
                self.js_void(Opcode::JsRt(RtOp::SetFunName(prefix)), vec![f, n]);
            }
            GlobalThis => {
                let r = self.js(
                    Opcode::JsRt(RtOp::GlobalThis),
                    vec![],
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            BigInt => {
                let idx = p.next_uint32().unwrap();
                let r = self.js(Opcode::JsRt(RtOp::BigInt(idx)), vec![], MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
            }
            MutateProto => {
                let proto = self.pop();
                let obj = self.top();
                let (o, pr) = (self.boxed(obj), self.boxed(proto));
                self.js_void(Opcode::JsRt(RtOp::MutateProto), vec![o, pr]);
            }
            CheckPrivateField => {
                let cond = u32::from(p.next_uint8().unwrap());
                let kind = u32::from(p.next_uint8().unwrap());
                let n = self.st.len();
                let (obj, key) = (self.st[n - 2], self.st[n - 1]);
                let (o, k) = (self.boxed(obj), self.boxed(key));
                let r = self.js(
                    Opcode::JsRt(RtOp::CheckPrivateField(cond, kind)),
                    vec![o, k],
                    MType::val(TagSet::BOOLEAN),
                );
                self.push(r, Ty::Val(TagSet::BOOLEAN));
            }
            Hole => {
                let v = self.const_val(ConstVal::Hole);
                self.push(v, Ty::Val(TagSet::MAGIC));
            }
            // Always throws: baseline does, from here.
            ThrowMsg | ThrowSetConst => {
                let b = self.exit_block(false);
                self.term(Opcode::Jump, vec![], vec![Self::goto(b)]);
            }
            NewTarget => {
                // The frame's new.target (undefined unless constructed).
                let v = self.inst(Opcode::FrameNewTarget, vec![], Some(MType::VAL_TOP));
                self.push(v, Ty::Val(TagSet::ALL));
            }
            IsConstructing => {
                let v = self.const_val(ConstVal::IsConstructing);
                self.push(v, Ty::Val(TagSet::MAGIC));
            }
            New | NewContent => {
                // [callee, this, args…, new.target] -> [object]
                self.publish_this();
                let argc = usize::from(p.next_uint16().unwrap());
                let n = self.st.len();
                let operands: Vec<Slot> = self.st.drain(n - argc - 3..).collect();
                let vals: Vec<mir::Value> = operands.into_iter().map(|x| self.boxed(x)).collect();
                let site = self.site(pc);
                let mono = match self.s.ctx.facts.scripted_targets(site) {
                    [s] => Some(*s),
                    _ => None,
                };
                let nslots = crate::wasm::mir::stamp::construct_nslots(self.s.ctx, mono, site);
                let word = self.s.alloc_word(mono, site);
                let callee = match mono {
                    Some(k) => self
                        .admit(&[k], true, &|s, k, ictx| s.callee(k, ictx))
                        .map(|mut t| t.remove(0)),
                    _ => None,
                };
                let r = match callee {
                    Some((k, c)) => self.inline_construct(k, &c, &vals, nslots, word),
                    _ => self.js(
                        Opcode::Construct(nslots, word),
                        vals,
                        MType::val(TagSet::OBJECT),
                    ),
                };
                // The object the site's constructor builds: its layout, as
                // an advisory class (checked at use).
                let built = mono
                    .and_then(|f| self.s.ctx.stamp_ctors_in.get(&f))
                    .or_else(|| self.s.ctx.construct_sites_in.get(&site));
                if let Some(si) = built {
                    self.val_cls.insert(r, si.layout_id);
                }
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            PushLexicalEnv | PushClassBodyEnv | PushVarEnv => {
                // A scope's environment, made the frame's (write-through:
                // an exit's baseline frame, and its unwind, see it).
                let kind = match op {
                    PushLexicalEnv => mir::ops::ENV_LEXICAL,
                    PushClassBodyEnv => mir::ops::ENV_CLASS_BODY,
                    _ => mir::ops::ENV_VAR,
                };
                let e = self.js(
                    Opcode::JsRt(RtOp::PushEnv(kind, pc.get())),
                    vec![],
                    MType::val(TagSet::OBJECT),
                );
                self.inst(Opcode::EnvSet, vec![e], None);
            }
            EnterWith => {
                let x = self.pop();
                let v = self.boxed(x);
                let e = self.js(
                    Opcode::JsRt(RtOp::EnterWith(pc.get())),
                    vec![v],
                    MType::val(TagSet::OBJECT),
                );
                self.inst(Opcode::EnvSet, vec![e], None);
            }
            FreshenLexicalEnv | RecreateLexicalEnv => {
                let recreate = u32::from(op == RecreateLexicalEnv);
                let e = self.js(
                    Opcode::JsRt(RtOp::FreshenEnv(recreate)),
                    vec![],
                    MType::val(TagSet::OBJECT),
                );
                self.inst(Opcode::EnvSet, vec![e], None);
            }
            PopLexicalEnv | LeaveWith => {
                self.inst(Opcode::EnvPop, vec![], None);
            }
            GetName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let t = u32::from(self.s.next_is_typeof(pc, op));
                let r = self.js(Opcode::JsRt(RtOp::GetName(a, t)), vec![], MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
            }
            BindName | BindUnqualifiedName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let k = u32::from(op == BindUnqualifiedName);
                let r = self.js(
                    Opcode::JsRt(RtOp::BindName(a, k)),
                    vec![],
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            DelName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let r = self.js(
                    Opcode::JsRt(RtOp::DelName(a)),
                    vec![],
                    MType::val(TagSet::BOOLEAN),
                );
                self.push(r, Ty::Val(TagSet::BOOLEAN));
            }
            BindVar => {
                let r = self.js(
                    Opcode::JsRt(RtOp::BindVar),
                    vec![],
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            SetName | StrictSetName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let v = self.pop();
                let env = self.pop();
                let (e, y) = (self.boxed(env), self.boxed(v));
                self.js_void(
                    Opcode::JsRt(RtOp::SetName(a, op == StrictSetName)),
                    vec![e, y],
                );
                self.repush(v);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
