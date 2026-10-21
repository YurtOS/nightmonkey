//! Effects of every opcode: regions read and written, GC, throws, flags, kills;
//! split out of `ops.rs` unchanged.

use super::*;

/// The effect-flags contribution of an op (`wasm/mir/abi.rs` names the bits).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct FlagBits(u8);

impl FlagBits {
    pub const NONE: FlagBits = FlagBits(0);
    pub const MUT_THIS: FlagBits = FlagBits(1);
    pub const MUT_OTHER: FlagBits = FlagBits(2);
    pub const STAMPS: FlagBits = FlagBits(4);
    pub const BIND: FlagBits = FlagBits(8);
    pub const ALL: FlagBits = FlagBits(15);

    pub const fn union(self, o: FlagBits) -> FlagBits {
        FlagBits(self.0 | o.0)
    }

    pub const fn bits(self) -> u8 {
        self.0
    }
}

/// How an op contributes to the flow-carried flags word (§6).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FlagsEffect {
    Bits(FlagBits),
    /// Whatever the helper reports at runtime (at most `ALL`).
    Dynamic,
    /// The callee's returned flags.
    Callee,
}

/// Where an op's kill pattern applies, which follows from its shape.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum KillSite {
    /// The op kills nothing.
    None,
    /// A single-successor op: a fence at the op itself.
    Op,
    /// On the `ok` edge (and the `err` edge): a static kill.
    OkEdge,
    /// On the `ok_dirty` edge (and `err`) only: a dynamic kill.
    DirtyEdge,
}

/// An op's effect summary.
#[derive(Clone, PartialEq, Debug)]
pub struct Effects {
    pub reads: Vec<Region>,
    pub writes: Vec<Region>,
    pub may_gc: bool,
    pub may_run_js: bool,
    pub may_throw: bool,
    pub kill: KillPattern,
    pub flags: FlagsEffect,
}

impl Effects {
    pub const PURE: Effects = Effects {
        reads: vec![],
        writes: vec![],
        may_gc: false,
        may_run_js: false,
        may_throw: false,
        kill: KillPattern::NONE,
        flags: FlagsEffect::Bits(FlagBits::NONE),
    };

    /// Arbitrary JS may run: every region, every killable component.
    fn generic(flags: FlagsEffect) -> Effects {
        Effects {
            reads: vec![Region::Unknown],
            writes: vec![Region::Unknown],
            may_gc: true,
            may_run_js: true,
            may_throw: true,
            kill: KillPattern::ALL,
            flags,
        }
    }

    pub fn is_pure(&self) -> bool {
        *self == Effects::PURE
    }
}

impl Opcode {
    /// Where this op's kill pattern (if any) applies.
    pub fn kill_site(&self, fx: &Effects) -> KillSite {
        if fx.kill.is_empty() {
            return KillSite::None;
        }
        let roles = self.roles();
        if roles.contains(&SuccRole::OkDirty) {
            KillSite::DirtyEdge
        } else if roles.contains(&SuccRole::Ok) {
            KillSite::OkEdge
        } else {
            KillSite::Op
        }
    }
}

/// The field region an access through a receiver of type `o` touches:
/// specific when a layout claim proves the receiver's class, the
/// `Field(*, name)` wildcard otherwise.
fn field_region(o: Option<&ObjInfo>, name: AtomId) -> Region {
    Region::Field {
        name,
        keys: o.and_then(|o| o.layout).map(|c| c.keys),
    }
}

/// What a by-name data read of `name` through an unproven receiver may
/// read: the field in any class, the global binding of that name (the
/// receiver may be the global object), and for `length`/`byteLength` the
/// array, string and typed-array lengths the runtime's pure arms serve.
fn prop_regions(name: AtomId, m: &Module) -> Vec<Region> {
    let mut r = vec![Region::Field { name, keys: None }];
    r.extend(
        m.bindings
            .iter()
            .filter(|(_, d)| d.name == name)
            .map(|(b, _)| Region::Global(b)),
    );
    let chars = m.atoms.get(name).map(|a| a.chars());
    let is = |s: &str| chars.is_some_and(|c| c.iter().copied().eq(s.encode_utf16()));
    if is("length") || is("byteLength") {
        r.push(Region::ArrayLength(None));
        r.push(Region::TypedArrayLength);
    }
    r
}

/// Whether atom `name` is `length`.
fn is_length(name: AtomId, m: &Module) -> bool {
    m.atoms
        .get(name)
        .is_some_and(|a| a.chars().iter().copied().eq("length".encode_utf16()))
}

/// The elements root a receiver's layout claim proves, if every layout in
/// its range names the same one.
fn elements_root(o: Option<&ObjInfo>, m: &Module) -> Option<crate::ids::RegionRoot> {
    let c = o?.layout?;
    let mut root = None;
    for k in c.keys.keys() {
        let r = m.layouts.get(&k)?.elements?;
        if root.is_some_and(|x| x != r) {
            return None;
        }
        root = Some(r);
    }
    root
}

/// The effect summary of `op` on operands of types `args`. Total: an
/// ill-typed op gets the summary its opcode implies with wildcard regions.
pub fn effects(op: &Opcode, args: &[Type], m: &Module) -> Effects {
    use Opcode::*;
    let recv = args.first().and_then(|t| t.obj_info());
    let mut fx = Effects::PURE;
    match op {
        JsAdd | JsBinop(_) | JsUnop(_) | JsCompare(_) | JsToNumeric | JsGetProp(_)
        | JsSetProp(..) | JsGetElem | JsSetElem(..) | JsBindGName(_) | JsSetName(..) => {
            return Effects::generic(FlagsEffect::Dynamic)
        }
        JsGetName(_) => return Effects::generic(FlagsEffect::Dynamic),
        // No user code runs: a concatenation or a BigInt allocates.
        Prim(_) => fx.may_gc = true,
        // Any location; interning the key or a string's char allocates.
        GetElemData => {
            fx.reads = vec![Region::Unknown];
            fx.may_gc = true;
        }
        // An int32 key names an element: the elements, an array's length,
        // a typed array's data; any other key may name a field.
        SetElemData(_) => {
            let int_key = matches!(args.get(1), Some(Type::Val(v)) if v.tags.is_nonempty_subset_of(TagSet::INT32));
            fx.writes = if int_key {
                [Region::Elements(None), Region::ArrayLength(None)]
                    .into_iter()
                    .chain(TaKind::ALL.iter().map(|&k| Region::TypedArrayData(k)))
                    .collect()
            } else {
                vec![Region::Unknown]
            };
            fx.may_gc = true;
            fx.kill = KillPattern::ALL;
            fx.flags = FlagsEffect::Dynamic;
        }
        JsRt(_) => return Effects::generic(FlagsEffect::Dynamic),
        ApplyFwd => return Effects::generic(FlagsEffect::Callee),
        // Allocations: they run no JS and change no class word (a GC moves
        // objects but keeps their words), so they kill nothing.
        ArgsObject | RestArray(_) | JsLambda(_) | JsBoxThis | NewThis(..) | NewThisInit { .. } => {
            fx.may_gc = true;
            fx.may_throw = true;
        }
        JsThrow => return Effects::generic(FlagsEffect::Bits(FlagBits::ALL)),
        // `.prototype` of the callee: generic, reported.
        CreateThis(..) => return Effects::generic(FlagsEffect::Dynamic),
        // Writes the class word of an object no guard can have proven.
        CtorStamp(..) | CtorPublish(..) => {
            fx.writes = vec![Region::Unknown];
            fx.flags = FlagsEffect::Bits(FlagBits::MUT_THIS);
        }
        // The object is fresh: no fact about its word exists yet.
        StampFresh(_) => fx.flags = FlagsEffect::Bits(FlagBits::MUT_THIS),
        // Advances a prefix key to its full one: slots stay put, so a
        // fact about the prefix stays true; a prefix-key guard just misses.
        Restamp(_) => {
            fx.writes = vec![Region::Unknown];
            fx.flags = FlagsEffect::Bits(FlagBits::MUT_OTHER);
        }
        // The rest of the callee, in baseline: anything.
        ExitInline { .. } => return Effects::generic(FlagsEffect::Dynamic),
        Call | CallIter | CallEval(_) | CallDirect | Construct(..) | CallNative(_) => {
            return Effects::generic(FlagsEffect::Callee)
        }
        // The atom's string: its lowering calls a may-GC helper.
        ConstStr(_) => fx.may_gc = true,
        LoadField(name) => {
            // SLOTS is tested locally; the IC arm may GC and reports dirt.
            fx.reads = vec![field_region(recv, *name)];
            fx.may_gc = true;
            fx.may_throw = true;
            fx.kill = KillPattern::ALL;
            fx.flags = FlagsEffect::Dynamic;
        }
        StoreField(name) => {
            // Not a fence for `types` (§4.3): the value conforms by type.
            // The IC arm (add transition) reports dirt dynamically.
            fx.writes = vec![field_region(recv, *name)];
            fx.may_gc = true;
            fx.may_throw = true;
            fx.kill = KillPattern::ALL;
            fx.flags = FlagsEffect::Dynamic;
        }
        LoadSlot(name) => fx.reads = vec![field_region(recv, *name)],
        StoreSlot(name) => {
            fx.writes = vec![field_region(recv, *name)];
            fx.flags = FlagsEffect::Bits(FlagBits::MUT_THIS.union(FlagBits::MUT_OTHER));
        }
        GetPropData(name) => fx.reads = prop_regions(*name, m),
        // A field (never the global's), or an array's length (which may
        // drop elements): an add may grow the slots; a demotion takes the
        // dirty edge.
        SetPropData(name) => {
            fx.writes = prop_regions(*name, m)
                .into_iter()
                .filter(|r| !matches!(r, Region::Global(_)))
                .chain(is_length(*name, m).then_some(Region::Elements(None)))
                .collect();
            fx.may_gc = true;
            fx.kill = KillPattern::ALL;
            fx.flags = FlagsEffect::Dynamic;
        }
        InitField(name) => {
            fx.writes = vec![field_region(recv, *name)];
            fx.may_gc = true;
            fx.flags = FlagsEffect::Bits(FlagBits::MUT_THIS);
        }
        PublishLayout => {
            fx.kill = KillPattern::of(KillSet::CONSTRUCTING);
        }
        NewObject(_) | NewArray => fx.may_gc = true,
        LitNew(_) => {
            fx.may_gc = true;
            fx.may_throw = true;
        }
        LitInit(name, k) => {
            fx.writes = vec![Region::Field {
                name: *name,
                keys: Some(KeyRange::one(*k)),
            }];
            fx.may_gc = true;
            fx.may_throw = true;
            fx.flags = FlagsEffect::Bits(FlagBits::MUT_OTHER);
        }
        LoadElem => fx.reads = vec![Region::Elements(elements_root(recv, m))],
        ArgsMapped(_) => fx.reads = vec![Region::Unknown],
        ArgsMappedSet(_) => {
            fx.writes = vec![Region::Unknown];
            fx.flags = FlagsEffect::Bits(FlagBits::MUT_OTHER);
        }
        StoreElem(_, append) => {
            // An overwrite, or an append (the lengths too).
            let root = elements_root(recv, m);
            fx.writes = vec![Region::Elements(root)];
            if *append {
                fx.writes.push(Region::ArrayLength(root));
                // The runtime's append may grow the elements.
                fx.may_gc = true;
            }
            fx.flags = FlagsEffect::Bits(FlagBits::MUT_OTHER);
        }
        LoadTa | StoreTa => {
            let r = match recv.map(|o| o.kind) {
                Some(ObjKind::TypedArray(k)) => Region::TypedArrayData(k),
                _ => Region::Unknown,
            };
            if *op == LoadTa {
                fx.reads = vec![r, Region::TypedArrayLength];
            } else {
                fx.reads = vec![Region::TypedArrayLength];
                fx.writes = vec![r];
                fx.flags = FlagsEffect::Bits(FlagBits::MUT_OTHER);
            }
        }
        LengthArray => fx.reads = vec![Region::ArrayLength(elements_root(recv, m))],
        // The iterator's own state, which nothing else reads.
        IterMore | IterEnd => {
            fx.reads = vec![Region::Unknown];
            fx.writes = vec![Region::Unknown];
        }
        // The value's shape and the realm's iteration fuses.
        IterOptimizable => fx.reads = vec![Region::Unknown],
        LengthTa => fx.reads = vec![Region::TypedArrayLength],
        ElementsPtr => fx.reads = vec![Region::Elements(elements_root(recv, m))],
        // Reading a rope's chars flattens it, which allocates.
        StrCharCodeAt => fx.may_gc = true,
        LoadGName(b) => fx.reads = vec![Region::Global(*b)],
        // Its cell, which the runtime arms and unarms.
        MethodLoad { .. } => fx.reads = vec![Region::Unknown],
        StoreGName(b) => {
            fx.writes = vec![Region::Global(*b)];
            fx.kill = KillPattern::of(store_gname_kills(*b, m));
            fx.flags = FlagsEffect::Bits(FlagBits::BIND);
        }
        // The frame's current environment, which a scope's entry and exit
        // replace.
        EnvCurrent | EnvCallee(_) => fx.reads = vec![Region::FrameEnv],
        EnvSet => fx.writes = vec![Region::FrameEnv],
        // An assignment of a local: baseline's frame slot, in program order.
        FrameStore(_) => fx.writes = vec![Region::FrameSlots],
        EnvPop => {
            fx.reads = vec![Region::FrameEnv];
            fx.writes = vec![Region::FrameEnv];
        }
        EnvLoad(s) => fx.reads = vec![Region::Env(*s)],
        EnvStore(s) => {
            fx.writes = vec![Region::Env(*s)];
            fx.flags = FlagsEffect::Bits(FlagBits::MUT_OTHER);
        }
        _ => {}
    }
    fx
}

/// What `store_gname` to binding `b` kills: every fuse (a fused literal's
/// may blow), and, for a binding with a predicted function, the binding
/// facts (the value may no longer be that function).
pub fn store_gname_kills(b: BindingId, m: &Module) -> KillSet {
    match m.bindings.get(b).and_then(|d| d.pred) {
        Some(_) => KillSet::FUSE.union(KillSet::BINDING),
        None => KillSet::FUSE,
    }
}
