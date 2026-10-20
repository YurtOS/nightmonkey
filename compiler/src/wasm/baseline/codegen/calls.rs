//! calls: methods of `Gen`, split out of `codegen.rs` unchanged.

use super::*;

impl<'a> Gen<'a> {
    /// A call whose `need` operands (callee, this, args[, new.target]) sit
    /// on the operand stack: the helper reads them in place as the
    /// callee's frame, and the result replaces them.
    pub(super) fn call_op(&mut self, f: Func, argc: u32, need: u32, extra: &[Value]) {
        let d = self.d;
        let base = self.top_addr(d - need);
        let av = self.i32c(argc);
        let mut args = vec![base, av];
        args.extend_from_slice(extra);
        let r = self.rt(f, &args);
        self.set_slot(d - need, r);
    }

    /// A call that enters a compiled callee directly when it can: the
    /// callee's body through `call_indirect` (no C++ re-entry, so JS call
    /// depth is bounded by the NightStack, not the native stack), else the
    /// generic helper. The frame is the same either way: the callee's
    /// `[callee, this, args...]` sit on the operand stack.
    pub(super) fn call_direct_or_generic(&mut self, argc: u32) {
        /// Headroom a compiled body may use past its actuals (the runtime
        /// entries' `kNightStackHeadroomSlots`).
        const HEADROOM: u32 = 64 * 1024;
        let d = self.d;
        let need = argc + 2;
        let base = self.top_addr(d - need);
        let top = self.top_addr(d);
        let callee = self.slot(d - need);
        let zero = self.i32c(0);
        let cls = {
            let args = self
                .body
                .arg_pool
                .from_iter([callee, zero, zero].into_iter());
            let tys = self
                .body
                .type_pool
                .from_iter([Type::I32, Type::I32, Type::I32].into_iter());
            self.push_val(ValueDef::Operator(
                Operator::Call {
                    function_index: self.h.call_classify,
                },
                args,
                tys,
            ))
        };
        let funcidx = self.push_val(ValueDef::PickOutput(cls, 0, Type::I32));
        let script = self.push_val(ValueDef::PickOutput(cls, 1, Type::I32));
        // The value stack is a region aligned to its size: the frame fits
        // iff its last byte has the frame base's high bits (no load).
        let last = self.add_off(top, 8 * (2 + argc) + HEADROOM - 1);
        let x = self.binop(Operator::I32Xor, last, self.sp, Type::I32);
        let size = self.i32c(1 << crate::region_shape::VALUE_STACK_LOG2);
        let fits = self.binop(Operator::I32LtU, x, size, Type::I32);
        let z = self.i32c(0);
        let compiled = self.binop(Operator::I32Ne, funcidx, z, Type::I32);
        let direct = self.binop(Operator::I32And, compiled, fits, Type::I32);
        let direct_blk = self.body.add_block();
        let generic_blk = self.body.add_block();
        let join = self.body.add_block();
        let ok = self.body.add_blockparam(join, Type::I32);
        self.cond_br(direct, Self::goto(direct_blk), Self::goto(generic_blk));

        self.cur = direct_blk;
        self.live = true;
        let argc_v = self.i32c(argc);
        let undef = self.i64c(UNDEF);
        // Bodies sit `N` table slots below their adapters (`wasm/mod.rs`).
        let off = self.i32c(u32::MAX);
        self.body_off_patches.push(off);
        let body_idx = self.binop(Operator::I32Sub, funcidx, off, Type::I32);
        let (base_d, top_d) = (self.top_addr(d - need), self.top_addr(d));
        let args = self
            .body
            .arg_pool
            .from_iter([self.cx, base_d, argc_v, top_d, script, undef, body_idx].into_iter());
        let tys = self
            .body
            .type_pool
            .from_iter([Type::I32, Type::I32].into_iter());
        let call = self.push_val(ValueDef::Operator(
            Operator::CallIndirect {
                sig_index: self.h.night_abi_sig2,
                table_index: self.h.indirect_table,
            },
            args,
            tys,
        ));
        let err = self.push_val(ValueDef::PickOutput(call, 0, Type::I32));
        let ok_direct = self.unop(Operator::I32Eqz, err, Type::I32);
        self.br(BlockTarget {
            block: join,
            args: vec![ok_direct],
        });

        self.cur = generic_blk;
        self.live = true;
        let argc_v = self.i32c(argc);
        let ok_generic = self
            .call(self.h.call, &[self.cx, top, base, argc_v], Some(Type::I32))
            .unwrap();
        self.br(BlockTarget {
            block: join,
            args: vec![ok_generic],
        });

        self.cur = join;
        self.live = true;
        self.branch_on_err(ok);
        let top = self.top_addr(d);
        let r = self.load_i64(top, 0);
        self.set_slot(d - need, r);
    }

    pub(super) fn function_this(&mut self) {
        let d = self.d;
        let thisv = self.load_i64(self.sp, FrameLayout::THIS);
        if self.script.strict {
            self.set_slot(d, thisv);
            return;
        }
        // Sloppy: an object is its own `this`; anything else is boxed (or
        // replaced by the global `this`).
        let is_obj = self.tag_eq(thisv, TAG_OBJECT);
        let obj_blk = self.body.add_block();
        let box_blk = self.body.add_block();
        let join = self.body.add_block();
        self.cond_br(is_obj, Self::goto(obj_blk), Self::goto(box_blk));
        self.cur = box_blk;
        self.live = true;
        let thisv2 = self.load_i64(self.sp, FrameLayout::THIS);
        let r = self.rt(self.h.box_nonstrict_this, &[thisv2]);
        self.set_slot(d, r);
        self.br(Self::goto(join));
        self.cur = obj_blk;
        self.live = true;
        let thisv3 = self.load_i64(self.sp, FrameLayout::THIS);
        self.set_slot(d, thisv3);
        self.br(Self::goto(join));
        self.cur = join;
        self.live = true;
    }

    /// `Arguments`: build the arguments object once per activation.
    pub(super) fn arguments(&mut self) {
        let d = self.d;
        let cached = self.load_i64(self.vp, self.layout.args_obj());
        let is_undef = self.tag_eq(cached, TAG_UNDEFINED);
        let build = self.body.add_block();
        let join = self.body.add_block();
        self.cond_br(is_undef, Self::goto(build), Self::goto(join));
        self.cur = build;
        self.live = true;
        let f = self.h.arguments_;
        let r = self.rt(f, &[self.sp, self.argc]);
        self.store_i64(self.vp, self.layout.args_obj(), r);
        self.br(Self::goto(join));
        self.cur = join;
        self.live = true;
        let v = self.load_i64(self.vp, self.layout.args_obj());
        self.set_slot(d, v);
    }
}
