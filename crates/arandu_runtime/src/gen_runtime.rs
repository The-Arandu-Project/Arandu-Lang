//! Host generational arena for debug JIT (F2.3.runtime).
//!
//! Mirrors `std.alloc.gen_arena` for **i64 payloads** only (MVP).
//! `GenRef` is packed as `i64`: `(index as u64) << 32 | generation as u64`.
//! Generation mismatch → `abort()` (same family as `abort_generational_mismatch`).

use std::sync::Mutex;

struct GenSlot {
    value: Option<i64>,
    generation: u32,
}

struct GenArenaI64 {
    slots: Vec<GenSlot>,
    free_list: Vec<u32>,
}

impl GenArenaI64 {
    const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free_list: Vec::new(),
        }
    }

    fn insert(&mut self, value: i64) -> i64 {
        if let Some(idx) = self.free_list.pop() {
            let slot = &mut self.slots[idx as usize];
            slot.generation = slot.generation.wrapping_add(1);
            slot.value = Some(value);
            pack_ref(idx, slot.generation)
        } else {
            let idx = self.slots.len() as u32;
            self.slots.push(GenSlot {
                value: Some(value),
                generation: 0,
            });
            pack_ref(idx, 0)
        }
    }

    fn get(&self, r: i64) -> i64 {
        let (idx, expected_generation) = unpack_ref(r);
        let Some(slot) = self.slots.get(idx as usize) else {
            abort_gen_mismatch();
        };
        if slot.generation != expected_generation {
            abort_gen_mismatch();
        }
        match slot.value {
            Some(v) => v,
            None => abort_gen_mismatch(),
        }
    }

    fn remove(&mut self, r: i64) -> i64 {
        let (idx, expected_generation) = unpack_ref(r);
        let Some(slot) = self.slots.get_mut(idx as usize) else {
            return 0;
        };
        if slot.generation != expected_generation {
            return 0;
        }
        let v = slot.value.take().unwrap_or(0);
        self.free_list.push(idx);
        // Keep generation; next insert will bump.
        v
    }
}

fn pack_ref(index: u32, generation: u32) -> i64 {
    ((index as u64) << 32 | generation as u64) as i64
}

fn unpack_ref(r: i64) -> (u32, u32) {
    let u = r as u64;
    ((u >> 32) as u32, u as u32)
}

fn abort_gen_mismatch() -> ! {
    // Host stand-in for `std.core.intrinsics.abort_generational_mismatch`.
    eprintln!("arandu: generational reference mismatch (use-after-free)");
    std::process::abort();
}

static ARENA: Mutex<GenArenaI64> = Mutex::new(GenArenaI64::new());

/// Insert `v` into the process gen-arena; returns packed GenRef.
///
/// # Safety
/// C ABI for Cranelift JIT symbol table.
pub unsafe extern "C" fn ar_gen_insert_i64(v: i64) -> i64 {
    ARENA.lock().unwrap_or_else(|e| e.into_inner()).insert(v)
}

/// Load payload; aborts on generation mismatch.
///
/// # Safety
/// C ABI for Cranelift JIT symbol table.
pub unsafe extern "C" fn ar_gen_get_i64(r: i64) -> i64 {
    ARENA.lock().unwrap_or_else(|e| e.into_inner()).get(r)
}

/// Remove/recycle slot; returns payload (0 if already dead).
///
/// # Safety
/// C ABI for Cranelift JIT symbol table.
pub unsafe extern "C" fn ar_gen_remove_i64(r: i64) -> i64 {
    ARENA.lock().unwrap_or_else(|e| e.into_inner()).remove(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_get_remove_cycle() {
        let mut a = GenArenaI64::new();
        let r = a.insert(42);
        assert_eq!(a.get(r), 42);
        assert_eq!(a.remove(r), 42);
        // Stale ref must not return live data.
        let r2 = a.insert(99);
        assert_eq!(a.get(r2), 99);
        // Old r is dead (generation or empty).
        // remove returned; get on stale aborts — tested via process only.
        let (i1, g1) = unpack_ref(r);
        let (i2, g2) = unpack_ref(r2);
        assert_eq!(i1, i2); // recycled index
        assert_ne!(g1, g2);
    }
}
