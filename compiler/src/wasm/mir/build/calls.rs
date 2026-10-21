//! calls: methods of `Run`, split out of `build.rs` unchanged.

use super::*;

mod emit;
mod inline;
mod natives;

impl<'s, 'a> Run<'s, 'a> {
    /// Give a generic `call` the site's likely callees (`likely_targets`),
    /// for its lowering's direct arms.
    pub(super) fn attach_targets(&mut self, inst: mir::Inst) {
        if let Some((classes, complete)) = self.store_mask.clone() {
            if matches!(
                self.f.insts[inst].op,
                Opcode::StoreField(_) | Opcode::JsSetProp(..) | Opcode::SetPropData(_)
            ) {
                let a = self.f.attachments.push(mir::func::Attachment {
                    site: Some(self.site(self.pc)),
                    field_types: classes
                        .iter()
                        .map(|&(k, t)| (k, mir::func::encode_tags(t)))
                        .collect(),
                    field_types_complete: complete,
                    ..Default::default()
                });
                self.f.insts[inst].attach = Some(a);
                return;
            }
        }
        if self.ta_poly_site
            && matches!(
                self.f.insts[inst].op,
                Opcode::JsGetElem
                    | Opcode::GetElemData
                    | Opcode::JsSetElem(..)
                    | Opcode::SetElemData(_)
            )
        {
            let a = self.f.attachments.push(mir::func::Attachment {
                site: Some(self.site(self.pc)),
                ta_poly: true,
                ..Default::default()
            });
            self.f.insts[inst].attach = Some(a);
            return;
        }
        if self.f.insts[inst].op != Opcode::Call || self.likely_targets.is_empty() {
            return;
        }
        let targets = std::mem::take(&mut self.likely_targets);
        let a = self.f.attachments.push(mir::func::Attachment {
            site: Some(self.site(self.pc)),
            ic_cell: None,
            call_cell: None,
            slot: None,
            field_types: vec![],
            field_types_complete: false,
            targets,
            ta_poly: false,
        });
        self.f.insts[inst].attach = Some(a);
    }

    /// `target.apply(this, arguments)` at a proven forward site
    /// (`compute_apply_fwd_pcs`: the arguments object feeds only such calls,
    /// so it was never made), operands `apply, target, this, placeholder`.
    /// With the `.apply` the pristine builtin, the site's known targets
    /// are inlined, their formals read from this frame's actuals; any other
    /// target, or another `.apply`, forwards through the runtime.
    pub(super) fn apply_forward(
        &mut self,
        vals: &[mir::Value],
        this_ctor: Option<((u32, FieldSet, bool), Slot)>,
    ) -> mir::Value {
        let site = self.site(self.pc);
        let facts = &self.s.ctx.facts;
        // Resolved for the site that entered this copy where the analysis
        // has it per entry.
        let per_entry = self
            .s
            .ictx
            .entry
            .and_then(|e| facts.apply_targets_in.get(&(e, site)));
        let sids: Vec<ScriptId> = match per_entry.or_else(|| facts.apply_targets.get(&site)) {
            Some(&k) => vec![k],
            None => facts
                .apply_target_sets
                .get(&site)
                .cloned()
                .unwrap_or_default(),
        };
        let helper = (Opcode::ApplyFwd, vec![vals[0], vals[1], vals[2]]);
        // A target reading its actuals would need this frame's count. A
        // `this` under construction (a wrapper constructor's forward to its
        // `initialize`): targets built for it, as `call_forward`'s.
        let tc = this_ctor.map(|(c, _)| c);
        let build = |s: &Shape<'a>, k, ictx| {
            s.callee_ctx(k, &[], tc, None, ictx)
                .filter(|c| !c.reads_actuals)
        };
        let Some(targets) = self.admit(&sids, false, &build) else {
            return self.js(helper.0, helper.1, MType::VAL_TOP);
        };
        let raw = this_ctor.map(|(_, x)| self.ctor_pass(x, vals[2]));
        // Arms with fences meet at `join`: `Obj` slots go in as `ObjHint`.
        // Without one (both arms keep), they keep their facts.
        if targets.iter().any(|(_, c)| c.fenced) || !self.keepable(1) {
            self.demote_objs();
        }
        let join = self.new_block();
        let result = self.f.add_param(join, MType::VAL_TOP);
        let (fast, slow) = (self.new_block(), self.new_block());
        let is_apply = self.inst(
            Opcode::JsIsBuiltin(crate::wasm::translate::BC_FUN_APPLY),
            vec![vals[0]],
            Some(MType::Bool),
        );
        self.term(
            Opcode::Br,
            vec![is_apply],
            vec![Self::goto(fast), Self::goto(slow)],
        );
        self.at(fast);
        let n = targets
            .iter()
            .map(|(_, c)| c.f.frame.formals)
            .max()
            .unwrap_or(0);
        let mut call = vec![vals[1], vals[2]];
        for k in 0..n {
            call.push(self.inst(Opcode::ActualArgOr(k), vec![], Some(MType::VAL_TOP)));
        }
        let r = self.inline_call_this(&targets, &call, Some(helper.clone()), raw, raw.is_some());
        if raw.is_some() {
            self.ctor_after(&targets, vals[2]);
        }
        self.term(
            Opcode::Jump,
            vec![],
            vec![Edge {
                block: join,
                args: vec![EdgeArg::Value(r)],
            }],
        );
        self.at(slow);
        if raw.is_some() {
            // Another `.apply` for a `this` under construction: baseline's
            // (`inline_call_this`'s rule).
            let ex = self.exit_block(false);
            self.term(Opcode::Jump, vec![], vec![Self::goto(ex)]);
        } else {
            let r = self.js(helper.0, helper.1, MType::VAL_TOP);
            self.term(
                Opcode::Jump,
                vec![],
                vec![Edge {
                    block: join,
                    args: vec![EdgeArg::Value(r)],
                }],
            );
        }
        self.at(join);
        result
    }

    /// `target.call(thisArg, args…)` at a site whose target the analysis
    /// resolved (`apply_targets`, `apply_target_sets`), operands `call,
    /// target, thisArg, args…`: with the `.call` the pristine builtin, the
    /// targets are inlined with `thisArg` as their `this` (a delegating
    /// constructor's `Super.call(this, …)`), built knowing the closures
    /// `fns` the call passes; any other target, or another `.call`, calls
    /// generically. `None` where nothing can be inlined.
    pub(super) fn call_forward(
        &mut self,
        vals: &[mir::Value],
        fns: &[Option<ScriptId>],
        this_ctor: Option<((u32, FieldSet, bool), Slot, mir::Value)>,
    ) -> Option<mir::Value> {
        let site = self.site(self.pc);
        let facts = &self.s.ctx.facts;
        if facts.apply_sites.get(&site) != Some(&crate::facts::CallForm::Call) {
            return None;
        }
        let sids: Vec<ScriptId> = match facts.apply_targets.get(&site) {
            Some(&k) => vec![k],
            None => facts
                .apply_target_sets
                .get(&site)
                .cloned()
                .unwrap_or_default(),
        };
        let tc = this_ctor.map(|(c, ..)| c);
        let targets = self.admit(&sids, false, &|s, k, ictx| {
            s.callee_ctx(k, fns, tc, None, ictx)
        })?;
        let raw = this_ctor.map(|(_, x, boxed)| self.ctor_pass(x, boxed));
        let generic = (Opcode::Call, vals.to_vec());
        // Arms with fences meet at `join`: `Obj` slots go in as `ObjHint`.
        if targets.iter().any(|(_, c)| c.fenced) || !self.keepable(1) {
            self.demote_objs();
        }
        let join = self.new_block();
        let result = self.f.add_param(join, MType::VAL_TOP);
        let (fast, slow) = (self.new_block(), self.new_block());
        let is_call = self.inst(
            Opcode::JsIsBuiltin(crate::wasm::translate::BC_FUN_CALL),
            vec![vals[0]],
            Some(MType::Bool),
        );
        self.term(
            Opcode::Br,
            vec![is_call],
            vec![Self::goto(fast), Self::goto(slow)],
        );
        self.at(fast);
        let r = self.inline_call_this(
            &targets,
            &vals[1..],
            Some(generic.clone()),
            raw,
            raw.is_some(),
        );
        if raw.is_some() {
            self.ctor_after(&targets, vals[2]);
        }
        self.term(
            Opcode::Jump,
            vec![],
            vec![Edge {
                block: join,
                args: vec![EdgeArg::Value(r)],
            }],
        );
        self.at(slow);
        let r = self.js(generic.0, generic.1, MType::VAL_TOP);
        self.term(
            Opcode::Jump,
            vec![],
            vec![Edge {
                block: join,
                args: vec![EdgeArg::Value(r)],
            }],
        );
        self.at(join);
        Some(result)
    }
}
