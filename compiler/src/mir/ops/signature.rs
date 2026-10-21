//! Operand and result typing of every opcode: `signature`, and the type helpers it
//! uses, split out of `ops.rs` unchanged.

use super::*;

/// Whether value type `t` has none of `tags`.
pub(super) fn free_of(t: &Type, tags: TagSet) -> bool {
    match t {
        Type::Val(v) => v.tags.intersect(tags).is_empty(),
        _ => false,
    }
}

/// Whether a numeric operator on `args` cannot yield a BigInt: that needs
/// both operands BigInt after ToNumeric, and one that is no BigInt and no
/// object (whose valueOf might give one) makes the result a Number, or a
/// TypeError for mixing.
pub(super) fn no_bigint(args: &[Type]) -> bool {
    let big = TagSet::prims(PRIM_BIGINT).union(TagSet::OBJECT);
    args.iter().any(|t| free_of(t, big))
}

type SigResult = Result<Sig, String>;

fn arity(args: &[Type], n: usize) -> Result<(), String> {
    if args.len() != n {
        return Err(format!("expected {n} operand(s), got {}", args.len()));
    }
    Ok(())
}

fn val(t: &Type, what: &str) -> Result<VSet, String> {
    match t {
        Type::Val(v) => Ok(*v),
        t => Err(format!(
            "{what}: expected a val, got {}",
            crate::mir::print::type_str(t)
        )),
    }
}

fn obj(t: &Type, what: &str) -> Result<ObjInfo, String> {
    match t {
        Type::Obj(o) => Ok(*o),
        t => Err(format!(
            "{what}: expected an obj, got {}",
            crate::mir::print::type_str(t)
        )),
    }
}

fn i32r(t: &Type, what: &str) -> Result<IRange, String> {
    match t {
        Type::I32(r) => Ok(*r),
        t => Err(format!(
            "{what}: expected an i32, got {}",
            crate::mir::print::type_str(t)
        )),
    }
}

fn want(ok: bool, msg: impl FnOnce() -> String) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(msg())
    }
}

fn want_kind(o: &ObjInfo, k: ObjKind, what: &str) -> Result<(), String> {
    want(o.kind.le(k), || {
        format!("{what}: expected an object of kind {k:?}, got {:?}", o.kind)
    })
}

/// Interval arithmetic in i128, which no i64 product overflows.
fn arith_range(op: ArithOp, a: IRange, b: IRange) -> (i128, i128) {
    let (al, ah, bl, bh) = (a.lo as i128, a.hi as i128, b.lo as i128, b.hi as i128);
    match op {
        ArithOp::Add => (al + bl, ah + bh),
        ArithOp::Sub => (al - bh, ah - bl),
        ArithOp::Mul => {
            let p = [al * bl, al * bh, ah * bl, ah * bh];
            (*p.iter().min().unwrap(), *p.iter().max().unwrap())
        }
        ArithOp::Rem => {
            // |r| < |divisor| and |r| <= |dividend|, with the dividend's
            // sign.
            let m = bl.abs().max(bh.abs()) - 1;
            let m = m.max(0);
            (al.max(-m).min(0), ah.min(m).max(0))
        }
    }
}

/// Whether range `r` contains 0.
fn has_zero(r: IRange) -> bool {
    r.lo <= 0 && 0 <= r.hi
}

/// Whether an integer `op` on operands in `x` and `y` may give -0 (as a
/// double), which no integer representation holds: a zero product with a
/// negative factor, or a zero remainder of a negative dividend.
pub fn may_neg_zero(op: ArithOp, x: IRange, y: IRange) -> bool {
    match op {
        ArithOp::Add | ArithOp::Sub => false,
        ArithOp::Mul => (has_zero(x) && y.lo < 0) || (has_zero(y) && x.lo < 0),
        ArithOp::Rem => x.lo < 0,
    }
}

/// The result range of integer `op` on operands in `x` and `y`, if every
/// result is an int32 other than -0 (and, for `%`, the divisor is never
/// 0): the op then needs no check.
pub fn i32_exact(op: ArithOp, x: IRange, y: IRange) -> Option<IRange> {
    if may_neg_zero(op, x, y) || (op == ArithOp::Rem && has_zero(y)) {
        return None;
    }
    let (lo, hi) = arith_range(op, x, y);
    (lo >= I32_MIN as i128 && hi <= I32_MAX as i128).then(|| IRange::new(lo as i64, hi as i64))
}

/// The result range of an int32 bit op: `&` with a non-negative operand
/// is at most it; `>>` by a constant count scales; `|` and `^` of
/// non-negative operands stay below the next power of two.
fn bit_range(b: BitOp, x: IRange, y: IRange) -> IRange {
    let pow2 = |h: i64| {
        if h <= 0 {
            0
        } else {
            (1i64 << (64 - h.leading_zeros())) - 1
        }
    };
    match b {
        BitOp::And if x.lo >= 0 && y.lo >= 0 => IRange::new(0, x.hi.min(y.hi)),
        BitOp::And if x.lo >= 0 => IRange::new(0, x.hi),
        BitOp::And if y.lo >= 0 => IRange::new(0, y.hi),
        BitOp::Or | BitOp::Xor if x.lo >= 0 && y.lo >= 0 => IRange::new(0, pow2(x.hi.max(y.hi))),
        BitOp::Shr if y.lo == y.hi => {
            let k = y.lo & 31;
            IRange::new(x.lo >> k, x.hi >> k)
        }
        // A shift by a constant that loses no bit of any value in range.
        BitOp::Shl
            if y.lo == y.hi
                && (x.lo << (y.lo & 31)) >= I32_MIN
                && (x.hi << (y.lo & 31)) <= I32_MAX =>
        {
            let k = y.lo & 31;
            IRange::new(x.lo << k, x.hi << k)
        }
        _ => IRange::new(I32_MIN, I32_MAX),
    }
}

fn clamp_range(lo: i128, hi: i128, min: i64, max: i64) -> IRange {
    let lo = lo.clamp(min as i128, max as i128) as i64;
    let hi = hi.clamp(min as i128, max as i128) as i64;
    IRange::new(lo, hi)
}

/// The type a boxed constant has.
pub fn const_val_type(c: ConstVal) -> Type {
    match c {
        ConstVal::Undefined => Type::val(TagSet::prims(PRIM_UNDEFINED)),
        ConstVal::Null => Type::val(TagSet::prims(PRIM_NULL)),
        ConstVal::Bool(_) => Type::val(TagSet::BOOLEAN),
        ConstVal::Int32(n) => Type::Val(VSet::new(
            TagSet::INT32,
            NumInfo::int(n.into(), n.into()),
            ObjInfo::TOP,
            StrInfo::TOP,
        )),
        ConstVal::Double(bits) => Type::Val(VSet::new(
            TagSet::DOUBLE,
            NumInfo::exact(f64::from_bits(bits)),
            ObjInfo::TOP,
            StrInfo::TOP,
        )),
        ConstVal::Uninitialized | ConstVal::IsConstructing | ConstVal::Hole => {
            Type::val(TagSet::MAGIC)
        }
        ConstVal::Dead => Type::VAL_TOP,
    }
}

/// Boxing is infallible and keeps every refinement.
pub fn box_type(t: &Type) -> Result<Type, String> {
    Ok(match t {
        Type::I32(r) => Type::Val(VSet::new(
            TagSet::INT32,
            r.num(),
            ObjInfo::TOP,
            StrInfo::TOP,
        )),
        // A raw integer or double may box with either number tag.
        Type::Int(r) => Type::Val(VSet::new(
            TagSet::NUMBER,
            r.num(),
            ObjInfo::TOP,
            StrInfo::TOP,
        )),
        Type::F64(n) => Type::Val(VSet::new(TagSet::NUMBER, *n, ObjInfo::TOP, StrInfo::TOP)),
        Type::Bool => Type::val(TagSet::BOOLEAN),
        Type::Obj(o) => Type::Val(VSet::new(TagSet::OBJECT, NumInfo::TOP, *o, StrInfo::TOP)),
        Type::Str(s) => Type::Val(VSet::new(TagSet::STRING, NumInfo::TOP, ObjInfo::TOP, *s)),
        t => {
            return Err(format!(
                "box: cannot box {}",
                crate::mir::print::type_str(t)
            ))
        }
    })
}

/// The claim a field op on a receiver of type `o` relies on: every layout
/// in the claim's key range must have `name`, within the supported
/// prefix. Returns the slot and, per layout, the field's claimed type.
fn field_claims(
    o: &ObjInfo,
    name: AtomId,
    m: &Module,
    what: &str,
) -> Result<(LayoutClaim, Vec<Type>), String> {
    let c = o
        .layout
        .ok_or_else(|| format!("{what}: receiver has no layout claim"))?;
    let mut claims = vec![];
    for k in c.keys.keys() {
        let layout = m
            .layouts
            .get(&k)
            .ok_or_else(|| format!("{what}: layout L{k} is not in the module"))?;
        // A predicted field (at its slot), or a typed field with no slot
        // prediction (an access finds it through an IC).
        match layout.field(name) {
            Some((slot, f)) => {
                if let Some(n) = c.state.fields() {
                    want(n.contains(u32::try_from(slot).unwrap()), || {
                        format!(
                            "{what}: field {} (slot {slot}) is not yet added ({:#x})",
                            m.atoms[name], n.0
                        )
                    })?;
                }
                claims.push(f.claim);
            }
            None => {
                let f = layout
                    .named_field(name)
                    .ok_or_else(|| format!("{what}: layout L{k} has no field {}", m.atoms[name]))?;
                want(c.state == LayoutState::Published, || {
                    format!(
                        "{what}: slotless field {} under construction",
                        m.atoms[name]
                    )
                })?;
                claims.push(f.claim);
            }
        }
    }
    Ok((c, claims))
}

/// The slot of field `name` through a receiver of type `o`, if the type
/// proves it: a layout claim with SLOTS whose every layout has the field
/// at one slot (below the prefix, for an object under construction).
pub fn slot_of(o: &ObjInfo, name: AtomId, m: &Module) -> Option<u32> {
    let c = o.layout.filter(|c| c.slots)?;
    let mut slot = None;
    for k in c.keys.keys() {
        let (s, _) = m.layouts.get(&k)?.field(name)?;
        if slot.is_some_and(|x| x != s)
            || c.state
                .fields()
                .is_some_and(|n| !n.contains(u32::try_from(s).unwrap()))
        {
            return None;
        }
        slot = Some(s);
    }
    slot.map(|s| u32::try_from(s).unwrap())
}

fn math_result(_f: MathFn) -> Type {
    Type::F64_TOP
}

/// The typing rule for `op` applied to operands of types `args`.
pub fn signature(op: &Opcode, args: &[Type], m: &Module) -> SigResult {
    use Opcode::*;
    let number_or_bigint = TagSet::prims(PRIM_INT32 | PRIM_DOUBLE | PRIM_BIGINT);
    Ok(match op {
        ConstVal(c) => {
            arity(args, 0)?;
            Sig::result(const_val_type(*c))
        }
        ConstI32(n) => {
            arity(args, 0)?;
            Sig::result(Type::I32(IRange::new((*n).into(), (*n).into())))
        }
        ConstF64(bits) => {
            arity(args, 0)?;
            Sig::result(Type::F64(NumInfo::exact(f64::from_bits(*bits))))
        }
        ConstBool(_) => {
            arity(args, 0)?;
            Sig::result(Type::Bool)
        }
        ConstObj(s) => {
            arity(args, 0)?;
            let def = m
                .snap_objs
                .get(*s)
                .ok_or_else(|| format!("const.obj: {s} is not in the module"))?;
            Sig::result(Type::Obj(ObjInfo {
                kind: def.kind,
                singleton: Some(*s),
                layout: None,
            }))
        }
        ConstStr(a) => {
            arity(args, 0)?;
            want(m.atoms.contains(*a), || {
                format!("const.str: {a} is not in the module")
            })?;
            Sig::result(Type::Str(StrInfo { atom: Some(*a) }))
        }

        Box => {
            arity(args, 1)?;
            Sig::result(box_type(&args[0])?)
        }
        BoxDouble => {
            arity(args, 1)?;
            let Type::F64(n) = args[0] else {
                return Err("box.double: operand is not f64".into());
            };
            Sig::result(Type::Val(VSet::new(
                TagSet::DOUBLE,
                n,
                ObjInfo::TOP,
                StrInfo::TOP,
            )))
        }
        Unbox(k) => {
            arity(args, 1)?;
            let v = val(&args[0], "unbox")?;
            want(v.tags.is_nonempty_subset_of(k.tags()), || {
                format!(
                    "unbox.{}: operand {} is not proven to have tags {}",
                    k.name(),
                    crate::mir::print::type_str(&args[0]),
                    crate::mir::print::tags_str(k.tags())
                )
            })?;
            Sig::result(k.result(&v))
        }
        I32ToInt => {
            arity(args, 1)?;
            Sig::result(Type::Int(i32r(&args[0], "i32.to_int")?))
        }
        I32ToF64 => {
            arity(args, 1)?;
            Sig::result(Type::F64(i32r(&args[0], "i32.to_f64")?.num()))
        }
        IntToF64 => {
            arity(args, 1)?;
            match args[0] {
                Type::Int(r) => Sig::result(Type::F64(r.num())),
                _ => return Err("int.to_f64: expected an int".into()),
            }
        }
        Weaken => {
            arity(args, 1)?;
            Sig::result(args[0])
        }

        GuardUnbox(k) => {
            arity(args, 1)?;
            let v = val(&args[0], "guard.unbox")?.restrict(k.tags());
            Sig::output(k.result(&v))
        }
        GuardTags(t) => {
            arity(args, 1)?;
            Sig::output(Type::Val(val(&args[0], "guard.tags")?.restrict(*t)))
        }
        GuardKind(k) => {
            arity(args, 1)?;
            let o = obj(&args[0], "guard.kind")?;
            let kind = if o.kind.le(*k) { o.kind } else { *k };
            Sig::output(Type::Obj(ObjInfo { kind, ..o }))
        }
        GuardLayout {
            keys,
            types,
            slots,
            closed,
        } => {
            arity(args, 1)?;
            let o = obj(&args[0], "guard.layout")?;
            let new = LayoutClaim {
                keys: *keys,
                types: *types,
                slots: *slots,
                closed: *closed,
                state: LayoutState::Published,
            };
            let layout = match o.layout {
                Some(c) if c.le(&new) => c,
                _ => new,
            };
            Sig::output(Type::Obj(ObjInfo {
                layout: Some(layout),
                ..o
            }))
        }
        GuardCtor { key, n, types } => {
            arity(args, 1)?;
            let o = obj(&args[0], "guard.ctor")?;
            Sig::output(Type::Obj(ObjInfo {
                layout: Some(LayoutClaim {
                    keys: KeyRange::one(*key),
                    types: *types,
                    slots: true,
                    closed: false,
                    state: LayoutState::Constructing(*n),
                }),
                ..o
            }))
        }
        GuardSingleton(s) => {
            arity(args, 1)?;
            let o = obj(&args[0], "guard.singleton")?;
            let def = m
                .snap_objs
                .get(*s)
                .ok_or_else(|| format!("guard.singleton: {s} is not in the module"))?;
            Sig::output(Type::Obj(ObjInfo {
                kind: if o.kind.le(def.kind) {
                    o.kind
                } else {
                    def.kind
                },
                singleton: Some(*s),
                ..o
            }))
        }
        GuardScript(s) => {
            arity(args, 1)?;
            let o = obj(&args[0], "guard.script")?;
            Sig::output(Type::Obj(ObjInfo {
                kind: ObjKind::Function(Some(*s)),
                ..o
            }))
        }
        MethodLoad { atom, script, .. } => {
            arity(args, 1)?;
            let o = obj(&args[0], "method.load")?;
            let c = o
                .layout
                .ok_or("method.load: receiver has no layout claim")?;
            want(c.state == LayoutState::Published, || {
                "method.load: receiver is not of a published layout".into()
            })?;
            // Every layout the receiver may be lacks the name: CLOSED (which
            // the op tests) then says it is not the receiver's own.
            for k in c.keys.lo.get()..=c.keys.hi.get() {
                let has = m
                    .layouts
                    .get(&crate::ids::LayoutKey::new(k))
                    .is_some_and(|l| l.fields.iter().flatten().any(|f| f.name == *atom));
                want(!has, || format!("method.load: layout L{k} has the name"))?;
            }
            Sig::output(Type::Obj(ObjInfo::kind(ObjKind::Function(Some(*script)))))
        }
        IntToI32 => {
            arity(args, 1)?;
            match args[0] {
                Type::Int(r) => Sig::output(Type::I32(clamp_range(
                    r.lo.into(),
                    r.hi.into(),
                    I32_MIN,
                    I32_MAX,
                ))),
                _ => return Err("int.to_i32: expected an int".into()),
            }
        }
        GuardRange(r) => {
            arity(args, 1)?;
            match args[0] {
                Type::I32(x) => {
                    let (lo, hi) = (x.lo.max(r.lo), x.hi.min(r.hi));
                    Sig::output(Type::I32(if lo <= hi { IRange::new(lo, hi) } else { *r }))
                }
                _ => return Err("guard.range: expected an i32".into()),
            }
        }
        F64ToIntExact => {
            arity(args, 1)?;
            match args[0] {
                Type::F64(n) => Sig::output(Type::I32(n.int_range(I32_MIN, I32_MAX))),
                _ => return Err("f64.to_int_exact: expected an f64".into()),
            }
        }
        CheckFuse(f) => {
            arity(args, 0)?;
            want(m.fuses.contains(*f), || {
                format!("check.fuse: {f} is not in the module")
            })?;
            Sig::output(Type::Fact(FactKind::Fuse(*f)))
        }
        CheckBinding(b, k) => {
            arity(args, 0)?;
            let def = m
                .bindings
                .get(*b)
                .ok_or_else(|| format!("check.binding: {b} is not in the module"))?;
            if *k == BindingCheck::Fn {
                want(def.pred.is_some(), || {
                    format!("check.binding.fn: {b} has no predicted function")
                })?;
                Sig::output(Type::Fact(FactKind::BindingFn(*b)))
            } else {
                Sig::output(Type::Fact(FactKind::Binding(*b)))
            }
        }
        AccessorProbe(..) => {
            arity(args, 1)?;
            val(&args[0], "accessor.probe receiver")?;
            Sig::output(Type::val(TagSet::OBJECT))
        }
        // The callee is native `n` (its pristine JSNative).
        CheckNative(n) => {
            arity(args, 1)?;
            val(&args[0], "check.native callee")?;
            want(m.natives.contains(*n), || {
                format!("check.native: {n} is not in the module")
            })?;
            Sig::output(Type::Fact(FactKind::NativeIntact(*n)))
        }

        Jump | Unreachable => {
            arity(args, 0)?;
            Sig::none()
        }
        Br => {
            arity(args, 1)?;
            want(args[0] == Type::Bool, || "br: expected a bool".into())?;
            Sig::none()
        }
        Switch(_) => {
            arity(args, 1)?;
            i32r(&args[0], "switch")?;
            Sig::none()
        }
        SwitchStr(n) => {
            arity(args, 1 + *n as usize)?;
            val(&args[0], "switch.str")?;
            for a in &args[1..] {
                want(matches!(a, Type::Str(StrInfo { atom: Some(_) })), || {
                    "switch.str: cases must be atoms".into()
                })?;
            }
            Sig::none()
        }
        Return => {
            arity(args, 1)?;
            val(&args[0], "return")?;
            Sig::none()
        }
        InlineEnter => {
            want(args.len() >= 2, || {
                "inline.enter: expected callee and this".into()
            })?;
            Sig::none()
        }
        ExitInline { nargs, nlocals, .. } => {
            want(frame_parts(args, *nargs, *nlocals).is_some(), || {
                "exit.inline: fewer operands than this + args + locals + rval".into()
            })?;
            for (i, t) in args.iter().enumerate() {
                if !matches!(t, Type::Val(_)) {
                    box_type(t).map_err(|e| format!("exit.inline operand {i}: {e}"))?;
                }
            }
            Sig::output(Type::VAL_TOP)
        }
        Exit { nargs, nlocals, .. }
        | ExitThrow { nargs, nlocals, .. }
        | GenSuspend { nargs, nlocals, .. } => {
            want(frame_parts(args, *nargs, *nlocals).is_some(), || {
                "exit: fewer operands than this + args + locals + rval".into()
            })?;
            // Any boxable representation: the lowering boxes each operand
            // once per exit shape (§5.1), not once per exit.
            for (i, t) in args.iter().enumerate() {
                if !matches!(t, Type::Val(_)) {
                    box_type(t).map_err(|e| format!("exit operand {i}: {e}"))?;
                }
            }
            Sig::none()
        }

        I32Ovf(a) => {
            arity(args, 2)?;
            let (x, y) = (i32r(&args[0], "i32 arith")?, i32r(&args[1], "i32 arith")?);
            let (lo, hi) = arith_range(*a, x, y);
            Sig::output(Type::I32(clamp_range(lo, hi, I32_MIN, I32_MAX)))
        }
        I32Wrap(a) => {
            arity(args, 2)?;
            let (x, y) = (i32r(&args[0], "i32 arith")?, i32r(&args[1], "i32 arith")?);
            // Wasm's `rem_s` traps on 0.
            want(*a != ArithOp::Rem || !has_zero(y), || {
                "i32.rem.wrap: the divisor may be 0".into()
            })?;
            let (lo, hi) = arith_range(*a, x, y);
            let fits = lo >= I32_MIN as i128 && hi <= I32_MAX as i128;
            Sig::result(if fits {
                Type::I32(IRange::new(lo as i64, hi as i64))
            } else {
                Type::I32_TOP
            })
        }
        IntArith(a) => {
            arity(args, 2)?;
            let r = |t: &Type| match t {
                Type::Int(r) => Ok(*r),
                _ => Err("int arith: expected int operands".to_string()),
            };
            let (x, y) = (r(&args[0])?, r(&args[1])?);
            want(
                !may_neg_zero(*a, x, y) && (*a != ArithOp::Rem || !has_zero(y)),
                || "int arith: may give -0 or divide by 0".into(),
            )?;
            let (lo, hi) = arith_range(*a, x, y);
            // `Int` arithmetic is only for interval proofs: an op that may
            // leave the exact-double domain is ill-typed, not a wraparound.
            want(lo >= -(INT_LIM as i128) && hi <= INT_LIM as i128, || {
                format!("int arith: result range [{lo}, {hi}] may leave [-2^53, 2^53]")
            })?;
            Sig::result(Type::Int(IRange::new(lo as i64, hi as i64)))
        }
        F64Arith(_) => {
            arity(args, 2)?;
            for t in args {
                want(matches!(t, Type::F64(_)), || {
                    "f64 arith: expected f64 operands".into()
                })?;
            }
            Sig::result(Type::F64_TOP)
        }
        F64Neg => {
            arity(args, 1)?;
            want(matches!(args[0], Type::F64(_)), || {
                "f64.neg: expected an f64".into()
            })?;
            Sig::result(Type::F64_TOP)
        }
        I32Bit(b) => {
            arity(args, 2)?;
            let x = i32r(&args[0], "i32 bitop")?;
            let y = i32r(&args[1], "i32 bitop")?;
            Sig::result(Type::I32(bit_range(*b, x, y)))
        }
        I32Ushr => {
            arity(args, 2)?;
            i32r(&args[0], "i32.ushr")?;
            i32r(&args[1], "i32.ushr")?;
            Sig::result(Type::Int(IRange::new(0, u32::MAX.into())))
        }
        ToInt32 => {
            arity(args, 1)?;
            want(matches!(args[0], Type::F64(_) | Type::Int(_)), || {
                "to_int32: expected an f64 or int".into()
            })?;
            Sig::result(Type::I32_TOP)
        }
        Cmp(r, _) => {
            arity(args, 2)?;
            for t in args {
                let ok = matches!(
                    (r, t),
                    (NumRepr::I32, Type::I32(_))
                        | (NumRepr::Int, Type::Int(_))
                        | (NumRepr::F64, Type::F64(_))
                );
                want(ok, || format!("cmp: operand is not {r:?}"))?;
            }
            Sig::result(Type::Bool)
        }
        Math(f) => {
            arity(args, f.arity())?;
            for t in args {
                want(matches!(t, Type::F64(_)), || {
                    "math: expected f64 operands".into()
                })?;
            }
            Sig::result(math_result(*f))
        }

        JsAdd => {
            arity(args, 2)?;
            val(&args[0], "js.add")?;
            val(&args[1], "js.add")?;
            // No string or object operand (whose ToPrimitive might give a
            // string): no concatenation.
            let strish = TagSet::STRING.union(TagSet::OBJECT);
            if free_of(&args[0], strish) && free_of(&args[1], strish) {
                Sig::output(Type::val(if no_bigint(args) {
                    TagSet::NUMBER
                } else {
                    number_or_bigint
                }))
            } else {
                Sig::output(Type::val(number_or_bigint.union(TagSet::STRING)))
            }
        }
        JsBinop(b) => {
            arity(args, 2)?;
            val(&args[0], "js.binop")?;
            val(&args[1], "js.binop")?;
            let nb = no_bigint(args);
            Sig::output(Type::val(match b {
                self::JsBinop::Ursh => TagSet::NUMBER,
                self::JsBinop::BitAnd
                | self::JsBinop::BitOr
                | self::JsBinop::BitXor
                | self::JsBinop::Lsh
                | self::JsBinop::Rsh
                    if nb =>
                {
                    TagSet::INT32
                }
                self::JsBinop::BitAnd
                | self::JsBinop::BitOr
                | self::JsBinop::BitXor
                | self::JsBinop::Lsh
                | self::JsBinop::Rsh => TagSet::prims(PRIM_INT32 | PRIM_BIGINT),
                _ if nb => TagSet::NUMBER,
                _ => number_or_bigint,
            }))
        }
        JsUnop(u) => {
            arity(args, 1)?;
            val(&args[0], "js.unop")?;
            Sig::output(Type::val(match u {
                self::JsUnop::Pos => TagSet::NUMBER,
                _ => number_or_bigint,
            }))
        }
        JsCompare(_) => {
            arity(args, 2)?;
            val(&args[0], "js.compare")?;
            val(&args[1], "js.compare")?;
            Sig::output(Type::Bool)
        }
        Prim(p) => return signature(&p.generic(), args, m),
        GetElemData => {
            arity(args, 2)?;
            val(&args[0], "getelem.data receiver")?;
            val(&args[1], "getelem.data key")?;
            Sig::output(Type::VAL_TOP)
        }
        SetElemData(_) => {
            arity(args, 3)?;
            for t in args {
                val(t, "setelem.data operand")?;
            }
            Sig::none()
        }
        JsTypeof => {
            arity(args, 1)?;
            val(&args[0], "js.typeof")?;
            Sig::result(Type::val(TagSet::STRING))
        }
        JsRt(r) => {
            let (n, out) = match r {
                RtOp::Instanceof | RtOp::In | RtOp::HasOwn | RtOp::DelElem(_) => {
                    (2, Some(Type::val(TagSet::BOOLEAN)))
                }
                RtOp::DelProp(..) => (1, Some(Type::val(TagSet::BOOLEAN))),
                RtOp::NewObject(_) | RtOp::NewArray(_) => (0, Some(Type::val(TagSet::OBJECT))),
                RtOp::InitProp(..) => (2, None),
                RtOp::InitElem(..) => (3, None),
                RtOp::ToPropertyKey => (1, Some(Type::VAL_TOP)),
                RtOp::RegExp(_) => (0, Some(Type::val(TagSet::OBJECT))),
                RtOp::InitPropGetSet(..) => (2, None),
                RtOp::Intrinsic(_) | RtOp::GetNameTypeof(_) => (0, Some(Type::VAL_TOP)),
                RtOp::Iter => (1, Some(Type::val(TagSet::OBJECT))),
                RtOp::Check(_) => (1, None),
                RtOp::SetFunName(_) | RtOp::MutateProto => (2, None),
                RtOp::GlobalThis => (0, Some(Type::val(TagSet::OBJECT))),
                RtOp::BigInt(_) => (0, Some(Type::VAL_TOP)),
                RtOp::CheckPrivateField(..) => (2, Some(Type::val(TagSet::BOOLEAN))),
                RtOp::CheckIsObj(_) | RtOp::CloseIter(_) => (1, None),
                RtOp::OptimizeSpreadCall => (1, Some(Type::VAL_TOP)),
                RtOp::PushEnv(..) | RtOp::FreshenEnv(_) | RtOp::BindName(..) | RtOp::BindVar => {
                    (0, Some(Type::val(TagSet::OBJECT)))
                }
                RtOp::EnterWith(_) => (1, Some(Type::val(TagSet::OBJECT))),
                RtOp::GetName(..) => (0, Some(Type::VAL_TOP)),
                RtOp::DelName(_) => (0, Some(Type::val(TagSet::BOOLEAN))),
                RtOp::SetName(..) => (2, None),
                RtOp::InitElemGetSet(_) => (3, None),
                RtOp::SuperBase | RtOp::SuperFun => (1, Some(Type::VAL_TOP)),
                RtOp::GetPropSuper(_) | RtOp::CheckReturn => (2, Some(Type::VAL_TOP)),
                RtOp::GetElemSuper | RtOp::SetPropSuper(..) => (3, Some(Type::VAL_TOP)),
                RtOp::SetElemSuper(_) => (4, Some(Type::VAL_TOP)),
                RtOp::InitHomeObject => (2, None),
                RtOp::AddDisposable(_) => (3, None),
                RtOp::TakeDisposeCapability => (0, Some(Type::VAL_TOP)),
                RtOp::CreateSuppressedError | RtOp::DynamicImport => {
                    (2, Some(Type::val(TagSet::OBJECT)))
                }
                RtOp::GetBoundName(_) => (1, Some(Type::VAL_TOP)),
                RtOp::ObjWithProto => (1, Some(Type::val(TagSet::OBJECT))),
                RtOp::NewPrivateName(_) => {
                    (0, Some(Type::val(TagSet::prims(crate::opsem::PRIM_SYMBOL))))
                }
                RtOp::SpreadEval(_) => (3, Some(Type::VAL_TOP)),
                RtOp::CreateGenerator => (0, Some(Type::val(TagSet::OBJECT))),
                RtOp::GenFinal => (1, None),
                RtOp::GenCheckResume => (3, None),
                RtOp::AsyncAwait(_) | RtOp::MaybeExtractAwait => (2, Some(Type::VAL_TOP)),
                RtOp::AsyncReject | RtOp::Resume => (3, Some(Type::VAL_TOP)),
                RtOp::CanSkipAwait => (1, Some(Type::val(TagSet::BOOLEAN))),
                RtOp::FunWithProto(_) => (1, Some(Type::val(TagSet::OBJECT))),
                RtOp::SpreadCall(0) => (4, Some(Type::VAL_TOP)),
                RtOp::SpreadCall(_) => (4, Some(Type::val(TagSet::OBJECT))),
                RtOp::ToString => (1, Some(Type::val(TagSet::STRING))),
                RtOp::Symbol(_) => (0, Some(Type::val(TagSet::prims(crate::opsem::PRIM_SYMBOL)))),
                RtOp::BuiltinObject(_) => (0, Some(Type::val(TagSet::OBJECT))),
            };
            arity(args, n)?;
            for t in args {
                val(t, "js.rt operand")?;
            }
            match out {
                Some(t) => Sig::output(t),
                None => Sig::none(),
            }
        }
        JsThrow => {
            arity(args, 1)?;
            val(&args[0], "js.throw")?;
            Sig::none()
        }
        ArgsObject | RestArray(_) => {
            arity(args, 0)?;
            Sig::output(Type::val(TagSet::OBJECT))
        }
        ArgsLength => {
            arity(args, 0)?;
            Sig::result(Type::I32(IRange::new(0, i64::from(i32::MAX))))
        }
        FrameNewTarget => {
            arity(args, 0)?;
            Sig::result(Type::VAL_TOP)
        }
        FrameCallee => {
            arity(args, 0)?;
            Sig::result(Type::val(TagSet::OBJECT))
        }
        IterMore => {
            arity(args, 1)?;
            val(&args[0], "iter.more")?;
            Sig::result(Type::VAL_TOP)
        }
        IsGenClosing => {
            arity(args, 1)?;
            val(&args[0], "is_gen_closing")?;
            Sig::result(Type::Bool)
        }
        IterIsDone => {
            arity(args, 1)?;
            val(&args[0], "iter.done")?;
            Sig::result(Type::Bool)
        }
        IterEnd => {
            arity(args, 1)?;
            val(&args[0], "iter.end")?;
            Sig::none()
        }
        IterOptimizable => {
            arity(args, 1)?;
            val(&args[0], "iter.optimizable")?;
            Sig::result(Type::Bool)
        }
        ActualArg => {
            arity(args, 1)?;
            i32r(&args[0], "args.actual")?;
            Sig::result(Type::VAL_TOP)
        }
        ActualArgOr(_) => {
            arity(args, 0)?;
            Sig::result(Type::VAL_TOP)
        }
        JsIsBuiltin(_) => {
            arity(args, 1)?;
            val(&args[0], "js.is_builtin")?;
            Sig::result(Type::Bool)
        }
        ApplyFwd => {
            arity(args, 3)?;
            for t in args {
                val(t, "js.apply_fwd operand")?;
            }
            Sig::output(Type::VAL_TOP)
        }
        JsToBool => {
            arity(args, 1)?;
            val(&args[0], "js.tobool")?;
            Sig::result(Type::Bool)
        }
        JsTypeofEq(_) | JsConstantStrictEq(_) => {
            arity(args, 1)?;
            val(&args[0], "js leaf compare")?;
            Sig::result(Type::Bool)
        }
        ArgsMapped(_) => {
            arity(args, 0)?;
            Sig::result(Type::VAL_TOP)
        }
        ArgsMappedSet(_) => {
            arity(args, 1)?;
            val(&args[0], "args.mapped_set")?;
            Sig::none()
        }
        JsToNumeric => {
            arity(args, 1)?;
            val(&args[0], "js.tonumeric")?;
            Sig::output(Type::val(number_or_bigint))
        }
        GetPropData(_) => {
            arity(args, 1)?;
            val(&args[0], "getprop.data receiver")?;
            Sig::output(Type::VAL_TOP)
        }
        SetPropData(_) => {
            arity(args, 2)?;
            val(&args[0], "setprop.data receiver")?;
            val(&args[1], "setprop.data value")?;
            Sig::none()
        }
        JsGetProp(_) | JsGetElem | JsSetProp(..) | JsSetElem(..) => {
            let n = match op {
                JsGetProp(_) => 1,
                JsGetElem | JsSetProp(..) => 2,
                _ => 3,
            };
            arity(args, n)?;
            for t in args {
                val(t, "js prop op")?;
            }
            if matches!(op, JsGetProp(_) | JsGetElem) {
                Sig::output(Type::VAL_TOP)
            } else {
                Sig::none()
            }
        }
        JsGetName(_) => {
            arity(args, 0)?;
            Sig::output(Type::VAL_TOP)
        }
        JsBoxThis => {
            arity(args, 1)?;
            val(&args[0], "js.box_this")?;
            Sig::output(Type::val(TagSet::OBJECT))
        }
        JsBindGName(_) => {
            arity(args, 0)?;
            Sig::output(Type::val(TagSet::OBJECT))
        }
        FrameStore(_) => {
            arity(args, 1)?;
            want(
                matches!(
                    args[0],
                    Type::Val(_) | Type::I32(_) | Type::F64(_) | Type::Bool | Type::Obj(_)
                ),
                || "frame.store: expected a val, i32, f64, bool or obj".into(),
            )?;
            Sig::none()
        }
        CreateThis(..) => {
            arity(args, 2)?;
            val(&args[0], "create_this callee")?;
            val(&args[1], "create_this new.target")?;
            Sig::output(Type::val(TagSet::OBJECT))
        }
        NewThis(..) => {
            arity(args, 2)?;
            let o = obj(&args[0], "new_this callee")?;
            want(matches!(o.kind, ObjKind::Function(Some(_))), || {
                "new_this: callee's script is not known".into()
            })?;
            val(&args[1], "new_this proto")?;
            Sig::output(Type::val(TagSet::OBJECT))
        }
        NewThisInit { key, types, n, .. } => {
            arity(args, 2 + *n as usize)?;
            let o = obj(&args[0], "new_this.init callee")?;
            want(matches!(o.kind, ObjKind::Function(Some(_))), || {
                "new_this.init: callee's script is not known".into()
            })?;
            val(&args[1], "new_this.init proto")?;
            let layout = m
                .layouts
                .get(key)
                .ok_or_else(|| format!("new_this.init: layout {key} is not in the module"))?;
            for (i, t) in args[2..].iter().enumerate() {
                let f = layout
                    .fields
                    .get(i)
                    .and_then(Option::as_ref)
                    .ok_or_else(|| format!("new_this.init: field {i} of {key} is not described"))?;
                want(is_subtype(t, &f.claim), || {
                    format!("new_this.init: value {i} does not conform to the field's claim")
                })?;
            }
            Sig::output(Type::Obj(ObjInfo {
                layout: Some(LayoutClaim {
                    keys: KeyRange::one(*key),
                    types: *types,
                    slots: true,
                    closed: false,
                    state: LayoutState::Constructing(FieldSet::prefix(*n)),
                }),
                ..ObjInfo::TOP
            }))
        }
        FnIsCtor => {
            arity(args, 1)?;
            obj(&args[0], "fn.is_ctor")?;
            Sig::result(Type::Bool)
        }
        ObjEmulatesUndef => {
            arity(args, 1)?;
            obj(&args[0], "obj.emulates_undef")?;
            Sig::result(Type::Bool)
        }
        Restamp(_) => {
            arity(args, 1)?;
            val(&args[0], "restamp")?;
            Sig::none()
        }
        StampFresh(_) => {
            arity(args, 1)?;
            val(&args[0], "stamp.fresh")?;
            Sig::none()
        }
        CtorStamp(..) | CtorPublish(..) => {
            arity(args, 1)?;
            val(&args[0], "ctor.stamp")?;
            Sig::none()
        }
        JsLambda(_) => {
            arity(args, 1)?;
            want_kind(&obj(&args[0], "js.lambda")?, ObjKind::Env, "js.lambda")?;
            Sig::output(Type::val(TagSet::OBJECT))
        }
        JsSetName(..) => {
            arity(args, 2)?;
            val(&args[0], "js.setname env")?;
            val(&args[1], "js.setname value")?;
            Sig::none()
        }

        LoadField(name) => {
            arity(args, 1)?;
            let o = obj(&args[0], "load_field")?;
            let (c, claims) = field_claims(&o, *name, m, "load_field")?;
            // `types` backs a field's claim: without it (the identity
            // alone) the value is any `Val`, to be guarded at the def.
            if !c.types {
                return Ok(Sig::output(Type::VAL_TOP));
            }
            let mut t = claims[0];
            for c in &claims[1..] {
                t = join(&t, c).ok_or("load_field: field claims disagree in representation")?;
            }
            Sig::output(t.shallow())
        }
        LoadSlot(name) => {
            arity(args, 1)?;
            let o = obj(&args[0], "load_slot")?;
            want(slot_of(&o, *name, m).is_some(), || {
                format!(
                    "load_slot: the receiver does not prove field {}'s slot",
                    m.atoms[*name]
                )
            })?;
            Sig::result(signature(&LoadField(*name), args, m)?.outputs[0])
        }
        StoreSlot(name) => {
            arity(args, 2)?;
            let o = obj(&args[0], "store_slot")?;
            want(slot_of(&o, *name, m).is_some(), || {
                "store_slot: the receiver's type does not prove the slot".into()
            })?;
            let (c, claims) = field_claims(&o, *name, m, "store_slot")?;
            want(c.types, || {
                "store_slot: the receiver's claim has no TYPES".into()
            })?;
            for claim in &claims {
                want(is_subtype(&args[1], claim), || {
                    "store_slot: value does not conform to the field's claim".into()
                })?;
            }
            Sig::none()
        }
        StoreField(name) => {
            arity(args, 2)?;
            let o = obj(&args[0], "store_field")?;
            let (c, claims) = field_claims(&o, *name, m, "store_field")?;
            // Without `types` there is no claim to keep: any value, and
            // the store's own check maintains the bits (§4.3).
            if !c.types {
                val(&args[1], "store_field value")?;
                return Ok(Sig::none());
            }
            for claim in &claims {
                want(is_subtype(&args[1], claim), || {
                    format!(
                        "store_field: value {} does not conform to the claim {}",
                        crate::mir::print::type_str(&args[1]),
                        crate::mir::print::type_str(claim)
                    )
                })?;
            }
            Sig::none()
        }
        InitField(name) => {
            arity(args, 2)?;
            let o = obj(&args[0], "init_field")?;
            let c = o.layout.ok_or("init_field: receiver has no layout claim")?;
            let n = match c.state {
                LayoutState::Constructing(n) => n,
                _ => return Err("init_field: receiver is not under construction".into()),
            };
            want(c.keys.lo == c.keys.hi, || {
                "init_field: receiver's layout is not exact".into()
            })?;
            let layout = m
                .layouts
                .get(&c.keys.lo)
                .ok_or_else(|| format!("init_field: layout L{} is not in the module", c.keys.lo))?;
            let (i, f) = layout
                .field(*name)
                .ok_or_else(|| format!("init_field: the layout has no field {}", m.atoms[*name]))?;
            let i = u32::try_from(i).unwrap();
            want(n.may_add(i), || {
                format!(
                    "init_field: field {} may not be added to {:#x}",
                    m.atoms[*name], n.0
                )
            })?;
            want(is_subtype(&args[1], &f.claim), || {
                "init_field: value does not conform to the field's claim".into()
            })?;
            Sig::output(Type::Obj(ObjInfo {
                layout: Some(LayoutClaim {
                    state: LayoutState::Constructing(n.with(i)),
                    ..c
                }),
                ..o
            }))
        }
        PublishLayout => {
            arity(args, 1)?;
            let o = obj(&args[0], "publish_layout")?;
            let c = o
                .layout
                .ok_or("publish_layout: receiver has no layout claim")?;
            let n = match c.state {
                LayoutState::Constructing(n) => n,
                _ => return Err("publish_layout: receiver is not under construction".into()),
            };
            let layout = m.layouts.get(&c.keys.lo).ok_or_else(|| {
                format!("publish_layout: layout L{} is not in the module", c.keys.lo)
            })?;
            want(
                c.keys.lo == c.keys.hi
                    && n == FieldSet::prefix(u32::try_from(layout.fields.len()).unwrap()),
                || "publish_layout: not every field is initialized".into(),
            )?;
            // Exactly the row (a constructing state is exact: anything
            // that might add to the object kills it), so CLOSED where the
            // layout may be.
            Sig::result(Type::Obj(ObjInfo {
                layout: Some(LayoutClaim {
                    state: LayoutState::Published,
                    closed: layout.closed,
                    ..c
                }),
                ..o
            }))
        }
        LitNew(_) => {
            arity(args, 0)?;
            Sig::output(Type::val(TagSet::OBJECT))
        }
        LitInit(..) => {
            arity(args, 2)?;
            val(&args[0], "lit.init object")?;
            val(&args[1], "lit.init value")?;
            Sig::none()
        }
        NewObject(k) => {
            arity(args, 0)?;
            want(m.layouts.contains_key(k), || {
                format!("new_object: layout L{k} is not in the module")
            })?;
            Sig::result(Type::Obj(ObjInfo {
                kind: ObjKind::Plain,
                singleton: None,
                layout: Some(LayoutClaim {
                    keys: KeyRange::one(*k),
                    types: true,
                    slots: true,
                    closed: false,
                    state: LayoutState::Constructing(FieldSet::EMPTY),
                }),
            }))
        }
        NewArray => {
            arity(args, 1)?;
            i32r(&args[0], "new_array")?;
            Sig::result(Type::Obj(ObjInfo::kind(ObjKind::Array)))
        }
        LoadElem | StoreElem(..) => {
            arity(args, if *op == LoadElem { 2 } else { 3 })?;
            want_kind(&obj(&args[0], "elem op")?, ObjKind::Native, "elem op")?;
            i32r(&args[1], "elem op index")?;
            if *op == LoadElem {
                Sig::output(Type::VAL_TOP)
            } else {
                val(&args[2], "store_elem value")?;
                Sig::none()
            }
        }
        LoadTa | StoreTa => {
            arity(args, if *op == LoadTa { 2 } else { 3 })?;
            let o = obj(&args[0], "typed array op")?;
            let k = match o.kind {
                ObjKind::TypedArray(k) => k,
                _ => {
                    return Err(
                        "typed array op: receiver is not a typed array of known kind".into(),
                    )
                }
            };
            i32r(&args[1], "typed array op index")?;
            if *op == LoadTa {
                Sig::output(match (k, k.proven_range()) {
                    (TaKind::Float32 | TaKind::Float64, _) => Type::F64_TOP,
                    (TaKind::Uint32, Some((lo, hi))) => Type::Int(IRange::new(lo, hi)),
                    (_, Some((lo, hi))) => Type::I32(IRange::new(lo, hi)),
                    (_, None) => Type::I32_TOP,
                })
            } else {
                want(
                    matches!(args[2], Type::I32(_) | Type::Int(_) | Type::F64(_)),
                    || "store_ta: value must be a raw number".into(),
                )?;
                Sig::none()
            }
        }
        LengthArray => {
            arity(args, 1)?;
            want_kind(
                &obj(&args[0], "length.array")?,
                ObjKind::Array,
                "length.array",
            )?;
            Sig::result(Type::Int(IRange::new(0, u32::MAX.into())))
        }
        LengthString => {
            arity(args, 1)?;
            want(matches!(args[0], Type::Str(_)), || {
                "length.string: expected a str".into()
            })?;
            Sig::result(Type::I32(IRange::new(0, (1 << 30) - 2)))
        }
        LengthTa => {
            arity(args, 1)?;
            let o = obj(&args[0], "length.ta")?;
            want(matches!(o.kind, ObjKind::TypedArray(_)), || {
                "length.ta: expected a typed array".into()
            })?;
            // A wasm32 engine's typed arrays are shorter than 2^31.
            Sig::result(Type::I32(IRange::new(0, I32_MAX)))
        }
        ElementsPtr => {
            arity(args, 1)?;
            want_kind(
                &obj(&args[0], "elements_ptr")?,
                ObjKind::Native,
                "elements_ptr",
            )?;
            Sig::result(Type::Raw(RawKind::Elements))
        }
        StrCharCodeAt => {
            arity(args, 2)?;
            want(matches!(args[0], Type::Str(_)), || {
                "str.char_code_at: expected a str".into()
            })?;
            i32r(&args[1], "str.char_code_at index")?;
            Sig::output(Type::I32(IRange::new(0, 0xffff)))
        }

        LoadGName(b) | StoreGName(b) => {
            let def = m
                .bindings
                .get(*b)
                .ok_or_else(|| format!("gname op: {b} is not in the module"))?;
            let typed = matches!(op, LoadGName(_))
                && args.first() == Some(&Type::Fact(FactKind::BindingFn(*b)));
            want(
                typed || args.first() == Some(&Type::Fact(FactKind::Binding(*b))),
                || format!("gname op: first operand must be the fact.binding({b}) ghost"),
            )?;
            if let LoadGName(_) = op {
                arity(args, 1)?;
                match def.pred.filter(|_| typed) {
                    // The predicted function (`check.binding.fn`).
                    Some(k) => Sig::result(Type::Val(VSet::new(
                        TagSet::OBJECT,
                        NumInfo::TOP,
                        ObjInfo::kind(ObjKind::Function(Some(k))),
                        StrInfo::TOP,
                    ))),
                    None => Sig::result(def.claim),
                }
            } else {
                arity(args, 2)?;
                want(is_subtype(&args[1], &def.claim), || {
                    "store_gname: value does not conform to the binding's claim".into()
                })?;
                Sig::none()
            }
        }
        GlobalObject => {
            arity(args, 0)?;
            Sig::result(Type::val(TagSet::OBJECT))
        }
        EnvCurrent => {
            arity(args, 0)?;
            Sig::result(Type::Obj(ObjInfo::kind(ObjKind::Env)))
        }
        EnvCallee(_) | ObjectLit(_) => {
            arity(args, 0)?;
            Sig::result(Type::val(TagSet::OBJECT))
        }
        EnvSet => {
            arity(args, 1)?;
            val(&args[0], "env.set")?;
            Sig::none()
        }
        EnvPop => {
            arity(args, 0)?;
            Sig::none()
        }
        EnvParent => {
            arity(args, 1)?;
            want_kind(&obj(&args[0], "env.parent")?, ObjKind::Env, "env.parent")?;
            Sig::result(Type::Obj(ObjInfo::kind(ObjKind::Env)))
        }
        EnvLoad(_) => {
            arity(args, 1)?;
            want_kind(&obj(&args[0], "env.load")?, ObjKind::Env, "env.load")?;
            Sig::result(Type::VAL_TOP)
        }
        EnvStore(_) => {
            arity(args, 2)?;
            want_kind(&obj(&args[0], "env.store")?, ObjKind::Env, "env.store")?;
            val(&args[1], "env.store value")?;
            Sig::none()
        }

        Call | CallIter | CallEval(_) | Construct(..) => {
            want(args.len() >= 2, || "call: expected callee and this".into())?;
            for t in args {
                val(t, "call operand")?;
            }
            Sig::output(if matches!(op, Construct(..)) {
                Type::val(TagSet::OBJECT)
            } else {
                Type::VAL_TOP
            })
        }
        CallDirect => {
            want(args.len() >= 2, || {
                "call_direct: expected callee and this".into()
            })?;
            let o = obj(&args[0], "call_direct callee")?;
            want(matches!(o.kind, ObjKind::Function(Some(_))), || {
                "call_direct: callee's script is not known".into()
            })?;
            for t in &args[1..] {
                val(t, "call operand")?;
            }
            Sig::output(Type::VAL_TOP)
        }
        CallNative(n) => {
            want(m.natives.contains(*n), || {
                format!("call_native: {n} is not in the module")
            })?;
            want(!args.is_empty(), || "call_native: expected this".into())?;
            for t in args {
                val(t, "call operand")?;
            }
            Sig::output(Type::VAL_TOP)
        }
    })
}
