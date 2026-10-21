//! op super: the lowering of one run of arms of `Run::op`, split out of `build.rs` unchanged.

use super::*;

impl<'s, 'a> Run<'s, 'a> {
    pub(super) fn op_super(&mut self, op: JSOp, pc: Pc, p: &mut BytecodeParser<'a>) -> R<bool> {
        use JSOp::*;
        match op {
            Eval | StrictEval => {
                let argc = usize::from(p.next_uint16().unwrap());
                self.publish_this();
                let n = self.st.len();
                let operands: Vec<Slot> = self.st.drain(n - argc - 2..).collect();
                let vals: Vec<mir::Value> = operands.into_iter().map(|x| self.boxed(x)).collect();
                let r = self.js(Opcode::CallEval(pc.get()), vals, MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
            }
            Coalesce => {
                // `a ?? b`: on to `b` when the top is null or undefined,
                // else keep it and jump past.
                let off = p.next_int32().unwrap();
                let a = self.top();
                let nullish = TagSet::prims(PRIM_NULL | PRIM_UNDEFINED);
                let tags = a.ty.tags();
                let b = if tags.subset_of(nullish) {
                    self.inst(Opcode::ConstBool(true), vec![], Some(MType::Bool))
                } else if tags.intersect(nullish).is_empty() {
                    self.inst(Opcode::ConstBool(false), vec![], Some(MType::Bool))
                } else {
                    let x = self.boxed(a);
                    self.tag_test(x, nullish)
                };
                let (target, next) = (pc.branch(off), pc + op.len());
                self.branch(b, next, target);
            }
            Object | CallSiteObj => {
                let index = p.next_uint32().unwrap();
                let r = self.inst(
                    Opcode::ObjectLit(index),
                    vec![],
                    Some(MType::val(TagSet::OBJECT)),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            EnvCallee => {
                let hops = match op.len() {
                    2 => u32::from(p.next_uint8().unwrap()),
                    _ => u32::from(p.next_uint16().unwrap()),
                };
                let r = self.inst(
                    Opcode::EnvCallee(hops),
                    vec![],
                    Some(MType::val(TagSet::OBJECT)),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            InitElemGetter | InitHiddenElemGetter | InitElemSetter | InitHiddenElemSetter => {
                let kind = u32::from(matches!(op, InitElemSetter | InitHiddenElemSetter))
                    | (u32::from(matches!(op, InitHiddenElemGetter | InitHiddenElemSetter)) << 1);
                let f = self.pop();
                let k = self.pop();
                let o = self.top();
                let (x, y, z) = (self.boxed(o), self.boxed(k), self.boxed(f));
                self.js_void(Opcode::JsRt(RtOp::InitElemGetSet(kind)), vec![x, y, z]);
            }
            SuperCall => {
                // `super(args)`: the runtime's construct of the parent
                // (`[callee, IS_CONSTRUCTING, args…, new.target]`), which
                // makes `this`; no site sizing, as in baseline.
                self.publish_this();
                let argc = usize::from(p.next_uint16().unwrap());
                let n = self.st.len();
                let operands: Vec<Slot> = self.st.drain(n - argc - 3..).collect();
                let vals: Vec<mir::Value> = operands.into_iter().map(|x| self.boxed(x)).collect();
                let nslots = crate::wasm::mir::abi::NO_NSLOTS;
                let r = self.js(
                    Opcode::Construct(nslots, 0),
                    vals,
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            SuperBase | SuperFun => {
                let x = self.pop();
                let v = self.boxed(x);
                let r = if op == SuperBase {
                    RtOp::SuperBase
                } else {
                    RtOp::SuperFun
                };
                let r = self.js(Opcode::JsRt(r), vec![v], MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
            }
            GetPropSuper => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let base = self.pop();
                let recv = self.pop();
                let (x, y) = (self.boxed(recv), self.boxed(base));
                let r = self.js(
                    Opcode::JsRt(RtOp::GetPropSuper(a)),
                    vec![x, y],
                    MType::VAL_TOP,
                );
                self.push(r, Ty::Val(TagSet::ALL));
            }
            GetElemSuper => {
                let base = self.pop();
                let key = self.pop();
                let recv = self.pop();
                let vals = vec![self.boxed(recv), self.boxed(key), self.boxed(base)];
                let r = self.js(Opcode::JsRt(RtOp::GetElemSuper), vals, MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
            }
            SetPropSuper | StrictSetPropSuper => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let v = self.pop();
                let base = self.pop();
                let recv = self.pop();
                let vals = vec![self.boxed(recv), self.boxed(base), self.boxed(v)];
                let r = self.js(
                    Opcode::JsRt(RtOp::SetPropSuper(a, op == StrictSetPropSuper)),
                    vals,
                    MType::VAL_TOP,
                );
                self.push(r, Ty::Val(TagSet::ALL));
            }
            SetElemSuper | StrictSetElemSuper => {
                let v = self.pop();
                let base = self.pop();
                let key = self.pop();
                let recv = self.pop();
                let vals = vec![
                    self.boxed(recv),
                    self.boxed(key),
                    self.boxed(base),
                    self.boxed(v),
                ];
                let r = self.js(
                    Opcode::JsRt(RtOp::SetElemSuper(op == StrictSetElemSuper)),
                    vals,
                    MType::VAL_TOP,
                );
                self.push(r, Ty::Val(TagSet::ALL));
            }
            InitHomeObject => {
                let home = self.pop();
                let f = self.top();
                let (x, y) = (self.boxed(f), self.boxed(home));
                self.js_void(Opcode::JsRt(RtOp::InitHomeObject), vec![x, y]);
            }
            FunWithProto => {
                let index = p.next_uint32().unwrap();
                let x = self.pop();
                let v = self.boxed(x);
                let r = self.js(
                    Opcode::JsRt(RtOp::FunWithProto(index)),
                    vec![v],
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
