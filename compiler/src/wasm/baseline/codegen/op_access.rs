//! op access: the lowering of one run of arms of `Gen::op`, split out of `codegen.rs` unchanged.

use super::*;

impl<'a> Gen<'a> {
    pub(super) fn op_access(&mut self, p: &mut BytecodeParser, op: JSOp) -> R<bool> {
        use JSOp::*;
        let d = self.d;
        let h = self.h;
        match op {
            // --- properties and elements ---
            GetProp => {
                // The site's inline cache: the module's shared probe
                // `night_ic_get` (own and holder ways, then the megamorphic
                // table), and on a miss the generic get, which fills the
                // site's ways.
                let a = self.atom(p.next_uint32().unwrap())?;
                let recv = self.slot(d - 1);
                let av = self.i32c(a);
                let cache = self.atoms.next_prop_cache();
                let way = self.i32c(IC_WAY_ADDR_PLACEHOLDER);
                self.prop_ic_patches.push((way, cache * INLINE_IC_STRIDE));
                let r = self
                    .call(h.ic_get_poly, &[recv, av, way], Some(Type::I64))
                    .unwrap();
                let hit = self.tag_eq(r, TAG_MAGIC);
                let hit = self.unop(Operator::I32Eqz, hit, Type::I32);
                self.fast_path(
                    hit,
                    |g, _| g.set_slot(d - 1, r),
                    |g| {
                        let c = g.i32c(cache);
                        let r = g.rt(h.get_prop_ic_miss, &[recv, av, c]);
                        g.set_slot(d - 1, r);
                    },
                );
            }
            SetProp | StrictSetProp => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let recv = self.slot(d - 2);
                let v = self.slot(d - 1);
                let (av, sv) = (self.i32c(a), self.i32c(u32::from(op == StrictSetProp)));
                let vouch = self.vouch_types(a, recv, v);
                let sv = self.binop(Operator::I32Or, sv, vouch, Type::I32);
                self.rt(h.set_property, &[recv, av, v, sv]);
                let v = self.slot(d - 1);
                self.set_slot(d - 2, v);
            }
            GetElem => {
                let a = self.slot(d - 2);
                let b = self.slot(d - 1);
                let obj = self.tag_eq(a, TAG_OBJECT);
                let int = self.tag_eq(b, TAG_INT32);
                let cond = self.binop(Operator::I32And, obj, int, Type::I32);
                self.fast_path(
                    cond,
                    |g, slow| {
                        let v = g.dense_element(a, b, slow);
                        g.set_slot(d - 2, v);
                    },
                    |g| {
                        let r = g.rt(h.get_element, &[a, b]);
                        g.set_slot(d - 2, r);
                    },
                );
            }
            SetElem | StrictSetElem => {
                let recv = self.slot(d - 3);
                let key = self.slot(d - 2);
                let v = self.slot(d - 1);
                let sv = self.i32c(u32::from(op == StrictSetElem));
                self.rt(h.set_element, &[recv, key, v, sv]);
                let v = self.slot(d - 1);
                self.set_slot(d - 3, v);
            }
            DelProp | StrictDelProp => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let v = self.slot(d - 1);
                let (av, sv) = (self.i32c(a), self.i32c(u32::from(op == StrictDelProp)));
                let r = self.rt(h.del_prop, &[v, av, sv]);
                self.set_slot(d - 1, r);
            }
            DelElem | StrictDelElem => {
                let sv = self.i32c(u32::from(op == StrictDelElem));
                self.binary(h.del_elem, &[sv], d);
            }
            In => self.binary(h.in_, &[], d),
            HasOwn => self.binary(h.has_own, &[], d),
            Instanceof => {
                let z = self.i32c(0);
                self.binary(h.instanceof_, &[z], d);
            }
            SuperBase => self.unary(h.super_base, d),
            SuperFun => self.unary(h.super_fun, d),
            GetPropSuper => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let recv = self.slot(d - 2);
                let base = self.slot(d - 1);
                let av = self.i32c(a);
                let r = self.rt(h.get_prop_super, &[recv, base, av]);
                self.set_slot(d - 2, r);
            }
            GetElemSuper => {
                let recv = self.slot(d - 3);
                let key = self.slot(d - 2);
                let base = self.slot(d - 1);
                let r = self.rt(h.get_elem_super, &[recv, key, base]);
                self.set_slot(d - 3, r);
            }
            SetPropSuper | StrictSetPropSuper => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let recv = self.slot(d - 3);
                let base = self.slot(d - 2);
                let v = self.slot(d - 1);
                let av = self.i32c(a);
                let sv = self.i32c(u32::from(op == StrictSetPropSuper));
                let r = self.rt(h.set_prop_super, &[recv, base, av, v, sv]);
                self.set_slot(d - 3, r);
            }
            SetElemSuper | StrictSetElemSuper => {
                let recv = self.slot(d - 4);
                let key = self.slot(d - 3);
                let base = self.slot(d - 2);
                let v = self.slot(d - 1);
                let sv = self.i32c(u32::from(op == StrictSetElemSuper));
                let r = self.rt(h.set_elem_super, &[recv, key, base, v, sv]);
                self.set_slot(d - 4, r);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
