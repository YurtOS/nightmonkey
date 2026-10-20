//! iteration: methods of `Lower`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    /// A for-in's start inline, taking edge 0, as the JIT's
    /// `ObjectToIterator` (`maybeLoadIteratorFromShape`, `registerIterator`):
    /// for an object whose shape caches an iterator that no for-in is using,
    /// with no dense elements on it or its prototypes, and whose prototypes'
    /// shapes are the ones the iterator recorded, that iterator, holding the
    /// object, marked active and joined to the compartment's list of active
    /// ones. The object is stored raw: a nursery one gets the whole-cell
    /// barrier on the (tenured) iterator object. Anything else falls through
    /// to the helper, which also fills the shape's cache.
    pub(super) fn iter_from_shape(&mut self, inst: mir::Inst, v: Value) -> R<()> {
        use crate::wasm::mir::abi::{
            ITER_SLOT_OFFSET, NI_COUNT_OFFSET, NI_FIRST_PROPERTY_OFFSET, NI_FLAGS_OFFSET,
            NI_FLAG_ACTIVE, NI_FLAG_INDICES_ALLOCATED, NI_FLAG_INITIALIZED, NI_INDEX_BYTES,
            NI_NEXT_OFFSET, NI_OBJECT_OFFSET, NI_PREV_OFFSET, NI_PROTO_SHAPE_BYTES,
            SHAPE_CACHE_ITERATOR, SHAPE_CACHE_OFFSET, SHAPE_CACHE_TAG_MASK,
        };
        let miss = self.body.add_block();
        let tag = self.tag_of(v);
        let is_obj = self.tag_is(tag, TAG_OBJECT as u32);
        self.check(is_obj, miss);
        let obj = self.un(Operator::I32WrapI64, v, Type::I32);
        let shape = self.load_i32(obj, SHAPE_OFFSET);
        let cache = self.load_i32(shape, SHAPE_CACHE_OFFSET);
        let mask = self.i32c(SHAPE_CACHE_TAG_MASK);
        let kind = self.bin(Operator::I32And, cache, mask, Type::I32);
        let it = self.i32c(SHAPE_CACHE_ITERATOR);
        let is_iter = self.bin(Operator::I32Eq, kind, it, Type::I32);
        self.check(is_iter, miss);
        // No dense elements (the iterator lists only named properties).
        let no_elems = |l: &mut Self, o: Value| {
            let elements = l.load_i32(o, OBJ_ELEMENTS_OFFSET);
            let back = l.i32c(ELEMENTS_INITLEN_BACK);
            let header = l.bin(Operator::I32Sub, elements, back, Type::I32);
            let initlen = l.load_i32(header, 0);
            l.un(Operator::I32Eqz, initlen, Type::I32)
        };
        let ok = no_elems(self, obj);
        self.check(ok, miss);
        let notag = self.i32c(!SHAPE_CACHE_TAG_MASK);
        let iter = self.bin(Operator::I32And, cache, notag, Type::I32);
        let ni = self.load_i32(iter, ITER_SLOT_OFFSET);
        // Reusable: initialized and not active.
        let m8 = self.mem(0, NI_FLAGS_OFFSET);
        let flags = self.un(Operator::I32Load8U { memory: m8 }, ni, Type::I32);
        let ia = self.i32c(NI_FLAG_INITIALIZED | NI_FLAG_ACTIVE);
        let state = self.bin(Operator::I32And, flags, ia, Type::I32);
        let init = self.i32c(NI_FLAG_INITIALIZED);
        let reusable = self.bin(Operator::I32Eq, state, init, Type::I32);
        self.check(reusable, miss);
        // The recorded prototype shapes: after the properties, and their
        // indices if allocated.
        let count = self.load_i32(ni, NI_COUNT_OFFSET);
        let pb = self.i32c(4);
        let props = self.bin(Operator::I32Mul, count, pb, Type::I32);
        let ib = self.i32c(NI_INDEX_BYTES);
        let idx_bytes = self.bin(Operator::I32Mul, count, ib, Type::I32);
        let ab = self.i32c(NI_FLAG_INDICES_ALLOCATED);
        let has_idx = self.bin(Operator::I32And, flags, ab, Type::I32);
        let zero = self.i32c(0);
        let idx_bytes = self.select(Type::I32, idx_bytes, zero, has_idx);
        let skip = self.bin(Operator::I32Add, props, idx_bytes, Type::I32);
        let first = self.i32c(NI_FIRST_PROPERTY_OFFSET);
        let base = self.bin(Operator::I32Add, ni, first, Type::I32);
        let shapes = self.bin(Operator::I32Add, base, skip, Type::I32);
        // Each prototype: no dense elements and the recorded shape; a
        // matched shape fixes the next prototype, so the walk ends where
        // the recording does.
        let (walk, hit) = (self.body.add_block(), self.body.add_block());
        let cur_shape = self.body.add_blockparam(walk, Type::I32);
        let cur_rec = self.body.add_blockparam(walk, Type::I32);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: walk,
                args: vec![shape, shapes],
            },
        });
        self.cur = walk;
        let bs = self.load_i32(cur_shape, SHAPE_BASESHAPE_OFFSET);
        let proto = self.load_i32(bs, BASESHAPE_PROTO_OFFSET);
        let more = self.body.add_block();
        self.cond_br(proto, Self::to(more), Self::to(hit));
        self.cur = more;
        let ok = no_elems(self, proto);
        self.check(ok, miss);
        let pshape = self.load_i32(proto, SHAPE_OFFSET);
        let want = self.load_i32(cur_rec, 0);
        let same = self.bin(Operator::I32Eq, pshape, want, Type::I32);
        self.check(same, miss);
        let step = self.i32c(NI_PROTO_SHAPE_BYTES);
        let next_rec = self.bin(Operator::I32Add, cur_rec, step, Type::I32);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: walk,
                args: vec![pshape, next_rec],
            },
        });
        self.cur = hit;
        // Start it on this object.
        self.store_i32(ni, NI_OBJECT_OFFSET, obj);
        let act = self.i32c(NI_FLAG_ACTIVE);
        let nf = self.bin(Operator::I32Or, flags, act, Type::I32);
        let ms = self.mem(0, NI_FLAGS_OFFSET);
        self.op(Operator::I32Store8 { memory: ms }, &[ni, nf], None);
        let la = self.i32c(self.h.strlit_slot + crate::region_shape::STRLIT_ENUMERATORS_OFF);
        let list = self.load_i32(la, 0);
        self.store_i32(ni, NI_NEXT_OFFSET, list);
        let last = self.load_i32(list, NI_PREV_OFFSET);
        self.store_i32(ni, NI_PREV_OFFSET, last);
        self.store_i32(last, NI_NEXT_OFFSET, ni);
        self.store_i32(list, NI_PREV_OFFSET, ni);
        // The object in the nursery: the iterator object's whole-cell
        // barrier.
        let cmask = self.i32c(NOT_CHUNK_MASK);
        let chunk = self.bin(Operator::I32And, obj, cmask, Type::I32);
        let sb = self.load_i32(chunk, CHUNK_STORE_BUFFER_OFFSET);
        let (record, done) = (self.body.add_block(), self.body.add_block());
        self.cond_br(sb, Self::to(record), Self::to(done));
        self.cur = record;
        self.call(self.h.post_whole_cell, &[self.cx, iter], &[]);
        self.terminate(Terminator::Br {
            target: Self::to(done),
        });
        self.cur = done;
        let r = self.box_tagged(TAG_OBJECT, iter);
        let t = self.edge(inst, 0, &[r])?;
        self.terminate(Terminator::Br { target: t });
        self.cur = miss;
        Ok(())
    }

    /// A for-in iterator's next key, inline as the JIT's `iteratorMore`:
    /// the property at the cursor, advancing it past deleted ones, or the
    /// no-more magic.
    pub(super) fn iter_more(&mut self, iter: Value) -> Value {
        use crate::wasm::mir::abi::{
            ITER_SLOT_OFFSET, NI_COUNT_OFFSET, NI_CURSOR_OFFSET, NI_DELETED_BIT,
            NI_FIRST_PROPERTY_OFFSET,
        };
        let obj = self.un(Operator::I32WrapI64, iter, Type::I32);
        let ni = self.load_i32(obj, ITER_SLOT_OFFSET);
        let (lp, more, done) = (
            self.body.add_block(),
            self.body.add_block(),
            self.body.add_block(),
        );
        let r = self.body.add_blockparam(done, Type::I64);
        self.terminate(Terminator::Br {
            target: Self::to(lp),
        });
        self.cur = lp;
        let cur = self.load_i32(ni, NI_CURSOR_OFFSET);
        let cnt = self.load_i32(ni, NI_COUNT_OFFSET);
        let left = self.bin(Operator::I32LtU, cur, cnt, Type::I32);
        let none = self.i64c((TAG_MAGIC << 32) | crate::wasm::translate::MAGIC_NO_ITER_VALUE);
        self.cond_br(
            left,
            Self::to(more),
            BlockTarget {
                block: done,
                args: vec![none],
            },
        );
        self.cur = more;
        let two = self.i32c(2);
        let off = self.bin(Operator::I32Shl, cur, two, Type::I32);
        let at = self.bin(Operator::I32Add, ni, off, Type::I32);
        let p = self.load_i32(at, NI_FIRST_PROPERTY_OFFSET);
        let one = self.i32c(1);
        let next = self.bin(Operator::I32Add, cur, one, Type::I32);
        self.store_i32(ni, NI_CURSOR_OFFSET, next);
        let db = self.i32c(NI_DELETED_BIT);
        let deleted = self.bin(Operator::I32And, p, db, Type::I32);
        let key = self.box_tagged(TAG_STRING, p);
        self.cond_br(
            deleted,
            Self::to(lp),
            BlockTarget {
                block: done,
                args: vec![key],
            },
        );
        self.cur = done;
        r
    }

    /// A for-in iterator's close, inline as the JIT's `iteratorClose`:
    /// clear the iterated object, reset the cursor, clear the active bit
    /// and unlink it from the active list. The shared empty iterator is
    /// left alone. Incremental marking (the cleared object pointer owes a
    /// pre-barrier) and properties deleted unvisited (whose marks the
    /// close clears) take the helper.
    pub(super) fn iter_end(&mut self, iter: Value) {
        use crate::wasm::mir::abi::{
            ITER_SLOT_OFFSET, NI_CURSOR_OFFSET, NI_FLAGS_OFFSET, NI_FLAG_ACTIVE,
            NI_FLAG_EMPTY_SINGLETON, NI_FLAG_UNVISITED_DELETION, NI_NEXT_OFFSET, NI_OBJECT_OFFSET,
            NI_PREV_OFFSET,
        };
        let obj = self.un(Operator::I32WrapI64, iter, Type::I32);
        let ni = self.load_i32(obj, ITER_SLOT_OFFSET);
        let m8 = self.mem(0, NI_FLAGS_OFFSET);
        let flags = self.un(Operator::I32Load8U { memory: m8 }, ni, Type::I32);
        let (helper, inline, done) = (
            self.body.add_block(),
            self.body.add_block(),
            self.body.add_block(),
        );
        let es = self.i32c(NI_FLAG_EMPTY_SINGLETON);
        let empty = self.bin(Operator::I32And, flags, es, Type::I32);
        let go = self.body.add_block();
        self.cond_br(empty, Self::to(done), Self::to(go));
        self.cur = go;
        let ud = self.i32c(NI_FLAG_UNVISITED_DELETION);
        let deletions = self.bin(Operator::I32And, flags, ud, Type::I32);
        let zone = self.load_i32(self.cx, JSCONTEXT_ZONE_OFFSET);
        let marking = self.load_i32(zone, ZONE_NEEDS_BARRIER_OFFSET);
        let slow = self.bin(Operator::I32Or, deletions, marking, Type::I32);
        self.cond_br(slow, Self::to(helper), Self::to(inline));
        self.cur = helper;
        let boxed = self.box_tagged(TAG_OBJECT, obj);
        self.call(self.h.end_iter, &[self.cx, boxed], &[]);
        self.terminate(Terminator::Br {
            target: Self::to(done),
        });
        self.cur = inline;
        let z = self.i32c(0);
        self.store_i32(ni, NI_OBJECT_OFFSET, z);
        self.store_i32(ni, NI_CURSOR_OFFSET, z);
        let keep = self.i32c(!NI_FLAG_ACTIVE & 0xFF);
        let nf = self.bin(Operator::I32And, flags, keep, Type::I32);
        let ms = self.mem(0, NI_FLAGS_OFFSET);
        self.op(Operator::I32Store8 { memory: ms }, &[ni, nf], None);
        let next = self.load_i32(ni, NI_NEXT_OFFSET);
        let prev = self.load_i32(ni, NI_PREV_OFFSET);
        self.store_i32(next, NI_PREV_OFFSET, prev);
        self.store_i32(prev, NI_NEXT_OFFSET, next);
        self.terminate(Terminator::Br {
            target: Self::to(done),
        });
        self.cur = done;
    }

    /// `Object.hasOwn(obj, key)` for an object and a string key inline,
    /// taking edge 0: the runtime's has-own row for (the object's shape,
    /// the key's atom), whose slotEnc is the answer (`HasOwnRow`). A key
    /// that is not an atom matches no row; a miss takes the helper, which
    /// fills the row.
    pub(super) fn has_own_arm(&mut self, inst: mir::Inst, key: Value, obj: Value) -> R<()> {
        use crate::wasm::translate::{MEGA_GET_ENTRY_BYTES, MEGA_GET_SIZE};
        let (kt, ot) = (self.tag_of(key), self.tag_of(obj));
        let ks = self.tag_is(kt, TAG_STRING as u32);
        let oo = self.tag_is(ot, TAG_OBJECT as u32);
        let both = self.bin(Operator::I32And, ks, oo, Type::I32);
        let (probe, miss) = (self.body.add_block(), self.body.add_block());
        self.cond_br(both, Self::to(probe), Self::to(miss));
        self.cur = probe;
        let kp = self.un(Operator::I32WrapI64, key, Type::I32);
        let two = self.i32c(2);
        let ekey = self.bin(Operator::I32Or, kp, two, Type::I32);
        let op = self.un(Operator::I32WrapI64, obj, Type::I32);
        let shape = self.load_i32(op, SHAPE_OFFSET);
        // NightRuntime.cpp's CacheHash.
        let three = self.i32c(3);
        let sh = self.bin(Operator::I32ShrU, shape, three, Type::I32);
        let k1 = self.i32c(2654435761);
        let h1 = self.bin(Operator::I32Mul, sh, k1, Type::I32);
        let k2c = self.i32c(0x9e37_79b9);
        let k2 = self.bin(Operator::I32Mul, ekey, k2c, Type::I32);
        let hh = self.bin(Operator::I32Xor, h1, k2, Type::I32);
        let mask = self.i32c(MEGA_GET_SIZE - 1);
        let idx = self.bin(Operator::I32And, hh, mask, Type::I32);
        let stride = self.i32c(MEGA_GET_ENTRY_BYTES);
        let off = self.bin(Operator::I32Mul, idx, stride, Type::I32);
        let base = self.i32c(self.h.mega_get_base);
        let row = self.bin(Operator::I32Add, base, off, Type::I32);
        let rshape = self.load_i32(row, 0);
        let rkey = self.load_i32(row, 4);
        let ms = self.bin(Operator::I32Eq, rshape, shape, Type::I32);
        let mk = self.bin(Operator::I32Eq, rkey, ekey, Type::I32);
        let hit = self.bin(Operator::I32And, ms, mk, Type::I32);
        let hit_b = self.body.add_block();
        self.cond_br(hit, Self::to(hit_b), Self::to(miss));
        self.cur = hit_b;
        let own = self.load_i32(row, 16);
        let own = self.un(Operator::I64ExtendI32U, own, Type::I64);
        let tag = self.i64c(TAG_BOOLEAN << 32);
        let r = self.bin(Operator::I64Or, own, tag, Type::I64);
        let t = self.edge(inst, 0, &[r])?;
        self.terminate(Terminator::Br { target: t });
        self.cur = miss;
        Ok(())
    }

    /// The pristine `Map.prototype.get(k)` on a Map, inline as the JIT's
    /// `mapObjectGet` does, taking `ok_clean` with the value or `undefined`:
    /// an atom key hashed by its stored hash, an int32 key by its bits
    /// (`OrderedHashTableImpl::prepareHash`, the hash scrambled), the
    /// bucket by the table's hash shift, then the chain compared by key
    /// bits (keys are stored normalized: strings atomized, integral
    /// doubles as int32). Any other key or receiver takes `other`.
    pub(super) fn map_get_arm(&mut self, inst: mir::Inst, ops: &[Value], other: Block) -> R<()> {
        use crate::wasm::mir::abi::{
            ATOM_HASH_OFFSET, FAT_ATOM_HASH_OFFSET, GOLDEN_RATIO_U32, MAP_ENTRY_CHAIN_OFFSET,
            MAP_ENTRY_KEY_OFFSET, MAP_ENTRY_VALUE_OFFSET, MAP_HASH_SHIFT_OFFSET,
            MAP_HASH_TABLE_OFFSET, MAP_LIVE_COUNT_OFFSET, STRING_ATOM_BIT, STRING_FAT_INLINE_MASK,
        };
        let (callee, this, key) = (ops[0], ops[1], ops[2]);
        let is_get = self.builtin_is(callee, BC_MAP_GET);
        let ttag = self.tag_of(this);
        let this_obj = self.tag_is(ttag, TAG_OBJECT as u32);
        let m = self.bin(Operator::I32And, is_get, this_obj, Type::I32);
        self.check(m, other);
        let obj = self.un(Operator::I32WrapI64, this, Type::I32);
        let shape = self.load_i32(obj, SHAPE_OFFSET);
        let base = self.load_i32(shape, SHAPE_BASESHAPE_OFFSET);
        let clasp = self.load_i32(base, BASESHAPE_CLASP_OFFSET);
        let cslot = self.i32c(self.h.strlit_slot + crate::region_shape::STRLIT_MAP_CLASS_OFF);
        let map_class = self.load_i32(cslot, 0);
        let is_map = self.bin(Operator::I32Eq, clasp, map_class, Type::I32);
        self.check(is_map, other);
        // The key's hash.
        let hashed = self.body.add_block();
        let h = self.body.add_blockparam(hashed, Type::I32);
        let (str_b, not_str, int_b) = (
            self.body.add_block(),
            self.body.add_block(),
            self.body.add_block(),
        );
        let ktag = self.tag_of(key);
        let is_str = self.tag_is(ktag, TAG_STRING as u32);
        self.cond_br(is_str, Self::to(str_b), Self::to(not_str));
        self.cur = not_str;
        let is_int = self.tag_is(ktag, TAG_INT32 as u32);
        self.cond_br(is_int, Self::to(int_b), Self::to(other));
        self.cur = str_b;
        let golden = self.i32c(GOLDEN_RATIO_U32);
        let kp = self.un(Operator::I32WrapI64, key, Type::I32);
        let flags = self.load_i32(kp, STRING_FLAGS_OFFSET);
        let ab = self.i32c(STRING_ATOM_BIT);
        let atom = self.bin(Operator::I32And, flags, ab, Type::I32);
        self.check(atom, other);
        let fm = self.i32c(STRING_FAT_INLINE_MASK);
        let fbits = self.bin(Operator::I32And, flags, fm, Type::I32);
        let fat = self.bin(Operator::I32Eq, fbits, fm, Type::I32);
        let (fo, no) = (self.i32c(FAT_ATOM_HASH_OFFSET), self.i32c(ATOM_HASH_OFFSET));
        let hoff = self.select(Type::I32, fo, no, fat);
        let haddr = self.bin(Operator::I32Add, kp, hoff, Type::I32);
        let ah = self.load_i32(haddr, 0);
        let sh = self.bin(Operator::I32Mul, ah, golden, Type::I32);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: hashed,
                args: vec![sh],
            },
        });
        self.cur = int_b;
        // mozilla::HashGeneric over the value's two words, then the scramble:
        // G * (rotl5(G * lo) ^ hi) * G.
        let golden = self.i32c(GOLDEN_RATIO_U32);
        let lo = self.un(Operator::I32WrapI64, key, Type::I32);
        let r = self.bin(Operator::I32Mul, lo, golden, Type::I32);
        let five = self.i32c(5);
        let r = self.bin(Operator::I32Rotl, r, five, Type::I32);
        let r = self.bin(Operator::I32Xor, r, ktag, Type::I32);
        let gg = self.i32c(GOLDEN_RATIO_U32.wrapping_mul(GOLDEN_RATIO_U32));
        let ih = self.bin(Operator::I32Mul, r, gg, Type::I32);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: hashed,
                args: vec![ih],
            },
        });
        // The lookup.
        self.cur = hashed;
        let undef_b = self.body.add_block();
        let live = self.load_i32(obj, MAP_LIVE_COUNT_OFFSET);
        let (has, walk) = (self.body.add_block(), self.body.add_block());
        let e = self.body.add_blockparam(walk, Type::I32);
        self.cond_br(live, Self::to(has), Self::to(undef_b));
        self.cur = has;
        let shift = self.load_i32(obj, MAP_HASH_SHIFT_OFFSET);
        let bucket = self.bin(Operator::I32ShrU, h, shift, Type::I32);
        let table = self.load_i32(obj, MAP_HASH_TABLE_OFFSET);
        let two = self.i32c(2);
        let boff = self.bin(Operator::I32Shl, bucket, two, Type::I32);
        let baddr = self.bin(Operator::I32Add, table, boff, Type::I32);
        let first = self.load_i32(baddr, 0);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: walk,
                args: vec![first],
            },
        });
        self.cur = walk;
        let (probe, found, next) = (
            self.body.add_block(),
            self.body.add_block(),
            self.body.add_block(),
        );
        self.cond_br(e, Self::to(probe), Self::to(undef_b));
        self.cur = probe;
        let k = self.load_i64(e, MAP_ENTRY_KEY_OFFSET);
        let same = self.bin(Operator::I64Eq, k, key, Type::I32);
        self.cond_br(same, Self::to(found), Self::to(next));
        self.cur = next;
        let chain = self.load_i32(e, MAP_ENTRY_CHAIN_OFFSET);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: walk,
                args: vec![chain],
            },
        });
        self.cur = found;
        let v = self.load_i64(e, MAP_ENTRY_VALUE_OFFSET);
        let t = self.edge(inst, 0, &[v])?;
        self.terminate(Terminator::Br { target: t });
        self.cur = undef_b;
        let u = self.i64c(TAG_UNDEFINED << 32);
        let t = self.edge(inst, 0, &[u])?;
        self.terminate(Terminator::Br { target: t });
        self.cur = other;
        Ok(())
    }
}
