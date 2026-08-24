# RFC: GenRef ABI & AMIR propagation (F2.3.runtime)

**Status:** Superseded by `arandu-genref-gold-rfc-v0.1.md`
**Depends on:** F2.3.1 escape analysis (done), F2.2 loan windows (done), G2 policy (done)  
**Does not replace:** static OSSA for non-escaping refs  
**Residual (not F2-blocking):** typed `GenArena<T>` slot tables in self-host; host remains i64.

> The stabilization and replacement plan is normative in
> `docs/arandu-genref-gold-strategy-v0.1.md`. This RFC records the MVP and must
> not be read as claiming general-payload, cross-arena or ABA-safe Gold status.

---

## 1. Motivation

F2.3 detects escape and emits **O004** / **O010**. The product path for compilable
escape is GenRef + gen-arena check (implemented for **int** promotion + host arena):

```text
live range closed in CFG  →  plain &T / &mut T (pointer-width)     [F2.2]
return &local             →  O010 hard error                       [done]
escape but compilable     →  GenRef + gen_arena check (i64 MVP)    [done]
@NoFallback / CLI flag    →  O004 Error (no GenRef emission)       [G2]
```

Lessons from `str` fat pointers: **ABI + AMIR + both backends** must agree
before implementation spreads. This RFC freezes that contract.

---

## 2. Layering (stdlib body)

| Piece | Crate / package | Rationale |
|-------|-----------------|-----------|
| `abort_generational_mismatch` | `arandu_core::intrinsics` | Zero-heap fatal trap (5th invariant) |
| `GenArena` / `GenSlot` / `GenRef` | `arandu_alloc::gen_arena` | Dynamic recycle = same tier as arena/slab |
| Escape decision + O004 | Compiler (`escape_analysis`) | Magia Inspecionável |
| Lowering to GenRef | Compiler AMIR + backends | Same blast radius as Str |

**Never** put slot tables in `arandu_core` (would break zero-heap).

---

## 3. Physical ABI of `GenRef`

Canonical layout (target pointer width \(W\)):

```text
struct GenRef {
    index: u32,        // slot index into the owning GenArena
    generation: u32,   // remembered generation
}
```

| Target | Size | Align | Notes |
|--------|------|-------|--------|
| 64-bit | 8 | 4 (or 8 if packed as i64) | Prefer `{u32,u32}` in one machine word for pass-by-value |
| 32-bit | 8 | 4 | Same two fields |

**Not** a fat pointer to payload: payload lives in the arena slot.  
**Not** interchangeable with `ptr[T]` or `&T` without an explicit conversion
emitted by the compiler after promotion.

### Comparison with `str`

| | `str` | `GenRef` |
|--|-------|----------|
| Fields | `(ptr, len)` | `(index, generation)` |
| Points at | UTF-8 bytes | Arena slot metadata |
| Check on use | bounds (optional) | **generation match (mandatory)** |
| On free | free buffer | bump gen + free_list |

---

## 4. Slot layout in `GenArena<T>`

Per-slot (conceptual; exact padding in LayoutEngine later):

```text
struct GenSlotHeader {
    generation: u32,
    // padding to align_of(T)
}
// followed by T payload (or Option-tag + T)
```

Generation **starts at 0** on first insert; **increments on recycle** (after
`remove`), never reuses a live `(index, gen)` pair.

---

## 5. AMIR contract

### 5.1 Local flag / type

When escape analysis **promotes** a stack local `x: T` to generational storage:

- Local becomes **memory-backed** with payload type `T` in a process/module
  arena (debug JIT: process-lifetime ok; see `arandu-jit-memory-v0.1.md`).
- References derived from it are typed as **`GenRef`** (new middle-type or
  `Named` std type interned as `std.alloc.gen_arena.GenRef`) — **not**
  `ArType::Ref(T)` pure pointer.

Suggested middle representation (implementation choice):

```text
ArType::GenRef          // index+gen pair, like a small struct
// or ArType::Named(GenRef_symbol, [])
```

### 5.2 Operations

| Source event | AMIR | Runtime |
|--------------|------|---------|
| Promote / first escape path | `GenInsert(place)` → `GenRef` temp | `GenArena::insert` |
| Borrow of promoted local | `Borrow` yields `GenRef` (copy of index+gen) | no heap |
| Load / field through gen ref | `GenGet(ref) → &T` or load T | `get` + abort on mismatch |
| Destroy / free of owner | `GenRemove(ref)` | `remove` (bump gen) |
| Return `&local` (stack) | still **O010** | never GenRef-of-frame |

### 5.3 What stays pointer-width

- Non-escaping `&T` / `&mut T` (F2.0–F2.2 path)
- Raw `ptr[T]` (unsafe)
- Heap `malloc` results that are already owning pointers (BC.4a)

---

## 6. Backend checklist (do not start half-done)

Same order of work that made `str` stable:

1. **LayoutEngine** — size/align of `GenRef` and optional slot header  
2. **Cranelift** — lower `GenInsert` / `GenGet` / `GenRemove`; trap on mismatch  
3. **C backend** — same ops as C helpers  
4. **Pretty / validate AMIR** — exhaustiveness  
5. **JIT tests** — insert → get OK; remove → get traps  

Host debug JIT may implement arena tables in Rust (`arandu_backend_cranelift`
runtime) until full `stdlib` self-host; **ABI must still match** this RFC.

---

## 7. Escape analysis → promotion policy

```text
EscapeKind::Return     → O010 (+ O004 note). No GenRef. Unfixable.
EscapeKind::HeapStore  → if !no_fallback: // internal HIR field
                           O004 note + mark local for GenRef promotion
                         else:
                           O004 Error (G2)
```

Promotion is **compiler-driven** and **visible** (O004). No silent heap.

---

## 8. Out of scope (later RFCs)

- Per-module vs global arena lifetime  
- Generational refs into async coroutine frames (needs A3)  
- Cross-language FFI of `GenRef`  
- Niche optimization (null GenRef, gen=0 empty)

---

## 9. Acceptance criteria for “F2.3.runtime done”

- [x] `stdlib/core` exposes `abort_generational_mismatch`  
- [x] `stdlib/alloc/gen_arena.aru` defines `GenRef` / `GenArena` API  
- [x] AMIR + Cranelift + C: `GenInsert`/`GenGet`/`GenRemove` (i64 payload MVP)  
- [x] Cranelift host arena + JIT test `jit_gen_insert_get_i64`  
- [x] Escape `HeapStore` path auto-promotes int locals (`gen_promote` after lower)  
- [x] O004 still emitted by escape analysis whenever fallback path applies  
- [ ] Gen arena for general `T` (not only i64)  
- [x] Roadmap **F2.3.runtime** MVP checked (typed-T left open)  

---

## 10. Effort note

Expect **similar cross-cutting cost to Str fat ABI** (layout + AMIR + 2
backends + tests). `gen_arena` data structure alone is small; **propagation**
is the critical path.
