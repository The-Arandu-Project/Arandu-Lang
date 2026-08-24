//! Compiler-managed generational arena used by generated code.
//!
//! The compact ABI remains `i64` while the Gold ABI is introduced, but its
//! identity rules are already strict: zero is permanently invalid, generation
//! counters never wrap, exhausted slots retire, and capacity is dynamic.

use std::sync::Mutex;

#[derive(Debug)]
enum GenSlotState {
    Vacant,
    Occupied(i64),
    Retired,
}

#[derive(Debug)]
struct GenSlot {
    state: GenSlotState,
    generation: u32,
}

#[derive(Debug, Default)]
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

    fn insert(&mut self, value: i64) -> Option<i64> {
        if let Some(index) = self.free_list.pop() {
            let slot = self.slots.get_mut(usize::try_from(index).ok()?)?;
            let generation = slot.generation.checked_add(1)?;
            slot.generation = generation;
            slot.state = GenSlotState::Occupied(value);
            return pack_ref(index, generation);
        }

        let index = u32::try_from(self.slots.len()).ok()?;
        index.checked_add(1)?;
        self.slots.try_reserve(1).ok()?;
        self.slots.push(GenSlot {
            state: GenSlotState::Occupied(value),
            generation: 1,
        });
        pack_ref(index, 1)
    }

    fn get(&self, handle: i64) -> Option<i64> {
        let (index, expected_generation) = unpack_ref(handle)?;
        let slot = self.slots.get(usize::try_from(index).ok()?)?;
        if slot.generation != expected_generation {
            return None;
        }
        match slot.state {
            GenSlotState::Occupied(value) => Some(value),
            GenSlotState::Vacant | GenSlotState::Retired => None,
        }
    }

    fn remove(&mut self, handle: i64) -> Option<i64> {
        let (index, expected_generation) = unpack_ref(handle)?;
        let slot_index = usize::try_from(index).ok()?;
        let slot = self.slots.get(slot_index)?;
        if slot.generation != expected_generation
            || !matches!(slot.state, GenSlotState::Occupied(_))
        {
            return None;
        }

        let retire = slot.generation == u32::MAX;
        if !retire {
            self.free_list.try_reserve(1).ok()?;
        }
        let slot = &mut self.slots[slot_index];
        let next = if retire {
            GenSlotState::Retired
        } else {
            GenSlotState::Vacant
        };
        let GenSlotState::Occupied(value) = std::mem::replace(&mut slot.state, next) else {
            return None;
        };
        if !retire {
            self.free_list.push(index);
        }
        Some(value)
    }

    fn set(&mut self, handle: i64, value: i64) -> Option<i64> {
        let (index, expected_generation) = unpack_ref(handle)?;
        let slot = self.slots.get_mut(usize::try_from(index).ok()?)?;
        if slot.generation != expected_generation {
            return None;
        }
        let GenSlotState::Occupied(current) = &mut slot.state else {
            return None;
        };
        *current = value;
        Some(handle)
    }

    fn upsert(&mut self, handle: i64, value: i64) -> Option<i64> {
        if handle == 0 {
            self.insert(value)
        } else {
            self.set(handle, value)
        }
    }
}

fn pack_ref(index: u32, generation: u32) -> Option<i64> {
    let encoded_index = index.checked_add(1)?;
    let bits = (u64::from(encoded_index) << 32) | u64::from(generation);
    Some(i64::from_ne_bytes(bits.to_ne_bytes()))
}

fn unpack_ref(handle: i64) -> Option<(u32, u32)> {
    let bits = u64::from_ne_bytes(handle.to_ne_bytes());
    let encoded_index = u32::try_from(bits >> 32).ok()?;
    let generation = u32::try_from(bits & u64::from(u32::MAX)).ok()?;
    if encoded_index == 0 || generation == 0 {
        return None;
    }
    Some((encoded_index - 1, generation))
}

fn abort_gen_mismatch() -> ! {
    eprintln!("arandu: generational reference mismatch (use-after-free)");
    std::process::abort();
}

static ARENA: Mutex<GenArenaI64> = Mutex::new(GenArenaI64::new());

/// Insert `value` into the process compiler-managed arena.
///
/// # Safety
/// C ABI entrypoint registered in the Cranelift JIT symbol table.
pub unsafe extern "C" fn ar_gen_insert_i64(value: i64) -> i64 {
    ARENA
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(value)
        .unwrap_or_else(|| abort_gen_mismatch())
}

/// Load a payload, aborting deterministically for any invalid handle.
///
/// # Safety
/// C ABI entrypoint registered in the Cranelift JIT symbol table.
pub unsafe extern "C" fn ar_gen_get_i64(handle: i64) -> i64 {
    ARENA
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(handle)
        .unwrap_or_else(|| abort_gen_mismatch())
}

/// Replace a live payload and return the unchanged handle.
///
/// # Safety
/// C ABI entrypoint registered in the Cranelift JIT symbol table.
pub unsafe extern "C" fn ar_gen_set_i64(handle: i64, value: i64) -> i64 {
    ARENA
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .set(handle, value)
        .unwrap_or_else(|| abort_gen_mismatch())
}

/// Insert for the reserved zero handle, otherwise replace the live payload.
///
/// # Safety
/// C ABI entrypoint registered in the Cranelift JIT symbol table.
pub unsafe extern "C" fn ar_gen_upsert_i64(handle: i64, value: i64) -> i64 {
    ARENA
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .upsert(handle, value)
        .unwrap_or_else(|| abort_gen_mismatch())
}

/// Remove a payload, invalidating all copies of its handle.
///
/// # Safety
/// C ABI entrypoint registered in the Cranelift JIT symbol table.
pub unsafe extern "C" fn ar_gen_remove_i64(handle: i64) -> i64 {
    ARENA
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(handle)
        .unwrap_or_else(|| abort_gen_mismatch())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn zero_is_invalid_and_first_handle_is_not_zero() {
        let mut arena = GenArenaI64::new();
        assert_eq!(arena.get(0), None);
        assert_eq!(arena.remove(0), None);
        assert_ne!(arena.insert(42), Some(0));
        let inserted = arena.upsert(0, 7).unwrap();
        assert_ne!(inserted, 0);
        assert_eq!(arena.upsert(inserted, 8), Some(inserted));
        assert_eq!(arena.get(inserted), Some(8));
    }

    #[test]
    fn insert_get_remove_cycle_recycles_without_aba() {
        let mut arena = GenArenaI64::new();
        let first = arena.insert(42).unwrap();
        assert_eq!(arena.get(first), Some(42));
        assert_eq!(arena.set(first, 43), Some(first));
        assert_eq!(arena.get(first), Some(43));
        assert_eq!(arena.remove(first), Some(43));
        let second = arena.insert(99).unwrap();
        assert_eq!(arena.get(second), Some(99));
        assert_eq!(arena.get(first), None);
        assert_eq!(unpack_ref(first).unwrap().0, unpack_ref(second).unwrap().0);
        assert_ne!(unpack_ref(first).unwrap().1, unpack_ref(second).unwrap().1);
    }

    #[test]
    fn exhausted_slot_retires_instead_of_wrapping() {
        let mut arena = GenArenaI64 {
            slots: vec![GenSlot {
                state: GenSlotState::Occupied(7),
                generation: u32::MAX,
            }],
            free_list: Vec::new(),
        };
        let exhausted = pack_ref(0, u32::MAX).unwrap();
        assert_eq!(arena.remove(exhausted), Some(7));
        let replacement = arena.insert(8).unwrap();
        assert_eq!(unpack_ref(replacement).unwrap().0, 1);
        assert_eq!(arena.get(exhausted), None);
    }
}
