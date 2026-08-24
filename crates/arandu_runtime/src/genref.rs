//! Safe generational arena core for the GenRef Gold runtime.
//!
//! This module owns the runtime state machine. Backend/JIT ABI adapters are
//! intentionally separate so a compact legacy representation cannot weaken
//! arena identity, retirement, or typed failure semantics.

use std::marker::PhantomData;
use std::rc::Rc;

/// A generational identity for one live arena inside a registry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ArenaId {
    slot: u32,
    generation: u32,
}

impl ArenaId {
    /// The reserved identity which never names a live arena.
    pub const INVALID: Self = Self {
        slot: 0,
        generation: 0,
    };

    #[must_use]
    pub const fn is_invalid(self) -> bool {
        self.slot == 0 && self.generation == 0
    }
}

/// Opaque, typed handle to a value owned by a specific arena generation.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct GenRef<T> {
    arena_slot: u32,
    arena_generation: u32,
    value_slot: u32,
    value_generation: u32,
    marker: PhantomData<fn() -> T>,
}

impl<T> GenRef<T> {
    /// The all-zero handle is reserved and permanently invalid.
    pub const INVALID: Self = Self {
        arena_slot: 0,
        arena_generation: 0,
        value_slot: 0,
        value_generation: 0,
        marker: PhantomData,
    };

    #[must_use]
    pub const fn is_invalid(self) -> bool {
        self.arena_slot == 0
            && self.arena_generation == 0
            && self.value_slot == 0
            && self.value_generation == 0
    }

    const fn arena_id(self) -> ArenaId {
        ArenaId {
            slot: self.arena_slot,
            generation: self.arena_generation,
        }
    }
}

impl<T> Clone for GenRef<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for GenRef<T> {}

impl<T> Default for GenRef<T> {
    fn default() -> Self {
        Self::INVALID
    }
}

/// Defined failures from safe GenRef operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenError {
    InvalidHandle,
    WrongArena,
    ArenaGone,
    Stale,
    CapacityOverflow,
    AllocationFailed,
}

#[derive(Debug)]
enum ValueState<T> {
    Vacant,
    Occupied(T),
    Retired,
}

#[derive(Debug)]
struct ValueSlot<T> {
    generation: u32,
    state: ValueState<T>,
}

#[derive(Debug)]
struct Arena<T> {
    slots: Vec<ValueSlot<T>>,
    free: Vec<u32>,
    generation_limit: u32,
}

impl<T> Arena<T> {
    fn new(generation_limit: u32) -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            generation_limit,
        }
    }

    fn insert(&mut self, arena_id: ArenaId, value: T) -> Result<GenRef<T>, GenError> {
        if let Some(index) = self.free.pop() {
            let slot = self
                .slots
                .get_mut(usize::try_from(index).map_err(|_| GenError::CapacityOverflow)?)
                .ok_or(GenError::CapacityOverflow)?;
            let generation = next_generation(slot.generation, self.generation_limit)?;
            slot.generation = generation;
            slot.state = ValueState::Occupied(value);
            return Ok(make_handle(arena_id, index, generation));
        }

        let index = u32::try_from(self.slots.len()).map_err(|_| GenError::CapacityOverflow)?;
        reserve_one(&mut self.slots)?;
        self.slots.push(ValueSlot {
            generation: 1,
            state: ValueState::Occupied(value),
        });
        Ok(make_handle(arena_id, index, 1))
    }

    fn get(&self, handle: GenRef<T>) -> Result<&T, GenError> {
        let slot = self.value_slot(handle)?;
        match &slot.state {
            ValueState::Occupied(value) => Ok(value),
            ValueState::Vacant | ValueState::Retired => Err(GenError::Stale),
        }
    }

    fn get_mut(&mut self, handle: GenRef<T>) -> Result<&mut T, GenError> {
        let index = usize::try_from(handle.value_slot).map_err(|_| GenError::Stale)?;
        let slot = self.slots.get_mut(index).ok_or(GenError::Stale)?;
        if slot.generation != handle.value_generation {
            return Err(GenError::Stale);
        }
        match &mut slot.state {
            ValueState::Occupied(value) => Ok(value),
            ValueState::Vacant | ValueState::Retired => Err(GenError::Stale),
        }
    }

    fn remove(&mut self, handle: GenRef<T>) -> Result<T, GenError> {
        let index = usize::try_from(handle.value_slot).map_err(|_| GenError::Stale)?;
        let slot = self.slots.get(index).ok_or(GenError::Stale)?;
        if slot.generation != handle.value_generation
            || !matches!(slot.state, ValueState::Occupied(_))
        {
            return Err(GenError::Stale);
        }

        let retire = slot.generation == self.generation_limit;
        if !retire {
            // Reserve before taking the value so allocation failure leaves the
            // arena byte-for-byte semantically unchanged.
            reserve_one(&mut self.free)?;
        }

        let slot = &mut self.slots[index];
        let next_state = if retire {
            ValueState::Retired
        } else {
            ValueState::Vacant
        };
        let ValueState::Occupied(value) = std::mem::replace(&mut slot.state, next_state) else {
            return Err(GenError::Stale);
        };
        if !retire {
            self.free.push(handle.value_slot);
        }
        Ok(value)
    }

    fn value_slot(&self, handle: GenRef<T>) -> Result<&ValueSlot<T>, GenError> {
        let slot = self
            .slots
            .get(usize::try_from(handle.value_slot).map_err(|_| GenError::Stale)?)
            .ok_or(GenError::Stale)?;
        if slot.generation != handle.value_generation {
            return Err(GenError::Stale);
        }
        Ok(slot)
    }
}

#[derive(Debug)]
enum ArenaState<T> {
    Vacant,
    Live(Arena<T>),
    Retired,
}

#[derive(Debug)]
struct ArenaSlot<T> {
    generation: u32,
    state: ArenaState<T>,
}

/// Thread-confined owner of generational arenas for one payload type.
///
/// The `Rc` marker deliberately keeps this type `!Send + !Sync`. A future
/// synchronized runtime surface must define its own explicit concurrency
/// contract instead of inheriting one from a process-global mutex.
#[derive(Debug)]
pub struct ArenaRegistry<T> {
    arenas: Vec<ArenaSlot<T>>,
    free: Vec<u32>,
    generation_limit: u32,
    thread_confined: PhantomData<Rc<()>>,
}

impl<T> ArenaRegistry<T> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            arenas: Vec::new(),
            free: Vec::new(),
            generation_limit: u32::MAX,
            thread_confined: PhantomData,
        }
    }

    #[cfg(test)]
    fn with_generation_limit(generation_limit: u32) -> Result<Self, GenError> {
        if generation_limit == 0 {
            return Err(GenError::CapacityOverflow);
        }
        Ok(Self {
            arenas: Vec::new(),
            free: Vec::new(),
            generation_limit,
            thread_confined: PhantomData,
        })
    }

    pub fn create_arena(&mut self) -> Result<ArenaId, GenError> {
        if let Some(index) = self.free.pop() {
            let slot = self
                .arenas
                .get_mut(usize::try_from(index).map_err(|_| GenError::CapacityOverflow)?)
                .ok_or(GenError::CapacityOverflow)?;
            let generation = next_generation(slot.generation, self.generation_limit)?;
            slot.generation = generation;
            slot.state = ArenaState::Live(Arena::new(self.generation_limit));
            return Ok(ArenaId {
                slot: index,
                generation,
            });
        }

        let index = u32::try_from(self.arenas.len()).map_err(|_| GenError::CapacityOverflow)?;
        reserve_one(&mut self.arenas)?;
        self.arenas.push(ArenaSlot {
            generation: 1,
            state: ArenaState::Live(Arena::new(self.generation_limit)),
        });
        Ok(ArenaId {
            slot: index,
            generation: 1,
        })
    }

    pub fn insert(&mut self, arena_id: ArenaId, value: T) -> Result<GenRef<T>, GenError> {
        self.arena_mut(arena_id)?.insert(arena_id, value)
    }

    pub fn get(&self, arena_id: ArenaId, handle: GenRef<T>) -> Result<&T, GenError> {
        self.validate_identity(arena_id, handle)?;
        self.arena(arena_id)?.get(handle)
    }

    pub fn get_mut(&mut self, arena_id: ArenaId, handle: GenRef<T>) -> Result<&mut T, GenError> {
        self.validate_identity(arena_id, handle)?;
        self.arena_mut(arena_id)?.get_mut(handle)
    }

    pub fn remove(&mut self, arena_id: ArenaId, handle: GenRef<T>) -> Result<T, GenError> {
        self.validate_identity(arena_id, handle)?;
        self.arena_mut(arena_id)?.remove(handle)
    }

    pub fn destroy_arena(&mut self, arena_id: ArenaId) -> Result<(), GenError> {
        let index = usize::try_from(arena_id.slot).map_err(|_| GenError::ArenaGone)?;
        let slot = self.arenas.get(index).ok_or(GenError::ArenaGone)?;
        if slot.generation != arena_id.generation || !matches!(slot.state, ArenaState::Live(_)) {
            return Err(GenError::ArenaGone);
        }

        let retire = slot.generation == self.generation_limit;
        if !retire {
            reserve_one(&mut self.free)?;
        }
        let slot = &mut self.arenas[index];
        slot.state = if retire {
            ArenaState::Retired
        } else {
            self.free.push(arena_id.slot);
            ArenaState::Vacant
        };
        Ok(())
    }

    fn validate_identity(&self, arena_id: ArenaId, handle: GenRef<T>) -> Result<(), GenError> {
        if arena_id.is_invalid() || handle.is_invalid() {
            return Err(GenError::InvalidHandle);
        }
        if handle.arena_id() != arena_id {
            return Err(GenError::WrongArena);
        }
        Ok(())
    }

    fn arena(&self, arena_id: ArenaId) -> Result<&Arena<T>, GenError> {
        let slot = self
            .arenas
            .get(usize::try_from(arena_id.slot).map_err(|_| GenError::ArenaGone)?)
            .ok_or(GenError::ArenaGone)?;
        if slot.generation != arena_id.generation {
            return Err(GenError::ArenaGone);
        }
        match &slot.state {
            ArenaState::Live(arena) => Ok(arena),
            ArenaState::Vacant | ArenaState::Retired => Err(GenError::ArenaGone),
        }
    }

    fn arena_mut(&mut self, arena_id: ArenaId) -> Result<&mut Arena<T>, GenError> {
        let slot = self
            .arenas
            .get_mut(usize::try_from(arena_id.slot).map_err(|_| GenError::ArenaGone)?)
            .ok_or(GenError::ArenaGone)?;
        if slot.generation != arena_id.generation {
            return Err(GenError::ArenaGone);
        }
        match &mut slot.state {
            ArenaState::Live(arena) => Ok(arena),
            ArenaState::Vacant | ArenaState::Retired => Err(GenError::ArenaGone),
        }
    }
}

impl<T> Default for ArenaRegistry<T> {
    fn default() -> Self {
        Self::new()
    }
}

fn make_handle<T>(arena_id: ArenaId, value_slot: u32, value_generation: u32) -> GenRef<T> {
    GenRef {
        arena_slot: arena_id.slot,
        arena_generation: arena_id.generation,
        value_slot,
        value_generation,
        marker: PhantomData,
    }
}

fn next_generation(current: u32, limit: u32) -> Result<u32, GenError> {
    current
        .checked_add(1)
        .filter(|generation| *generation <= limit)
        .ok_or(GenError::CapacityOverflow)
}

fn reserve_one<T>(values: &mut Vec<T>) -> Result<(), GenError> {
    let new_len = values
        .len()
        .checked_add(1)
        .ok_or(GenError::CapacityOverflow)?;
    let bytes = new_len
        .checked_mul(std::mem::size_of::<T>())
        .ok_or(GenError::CapacityOverflow)?;
    if bytes > isize::MAX.unsigned_abs() {
        return Err(GenError::CapacityOverflow);
    }
    values
        .try_reserve(1)
        .map_err(|_| GenError::AllocationFailed)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::cell::Cell;

    #[test]
    fn safe_core_rejects_zero_cross_arena_and_stale_handles() {
        let mut registry = ArenaRegistry::<i64>::with_generation_limit(3).unwrap();
        let first_arena = registry.create_arena().unwrap();
        let second_arena = registry.create_arena().unwrap();
        let handle = registry.insert(first_arena, 42).unwrap();

        assert_eq!(
            registry.get(first_arena, GenRef::INVALID),
            Err(GenError::InvalidHandle)
        );
        assert_eq!(
            registry.get(second_arena, handle),
            Err(GenError::WrongArena)
        );
        assert_eq!(registry.remove(first_arena, handle), Ok(42));
        assert_eq!(registry.get(first_arena, handle), Err(GenError::Stale));
    }

    #[test]
    fn value_and_arena_slots_retire_without_wrap() {
        let mut registry = ArenaRegistry::<i64>::with_generation_limit(2).unwrap();
        let first_arena = registry.create_arena().unwrap();
        let first = registry.insert(first_arena, 1).unwrap();
        assert_eq!(registry.remove(first_arena, first), Ok(1));
        let second = registry.insert(first_arena, 2).unwrap();
        assert_eq!(first.value_slot, second.value_slot);
        assert_eq!(registry.remove(first_arena, second), Ok(2));
        let third = registry.insert(first_arena, 3).unwrap();
        assert_ne!(first.value_slot, third.value_slot);

        registry.destroy_arena(first_arena).unwrap();
        let replacement = registry.create_arena().unwrap();
        assert_eq!(first_arena.slot, replacement.slot);
        registry.destroy_arena(replacement).unwrap();
        let after_retirement = registry.create_arena().unwrap();
        assert_ne!(first_arena.slot, after_retirement.slot);
    }

    #[test]
    fn get_borrows_remove_moves_and_destroy_drops_once() {
        #[derive(Debug)]
        struct Probe<'a>(&'a Cell<usize>, i32);
        impl Drop for Probe<'_> {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }

        let drops = Cell::new(0);
        let mut registry = ArenaRegistry::new();
        let arena = registry.create_arena().unwrap();
        let removed = registry.insert(arena, Probe(&drops, 7)).unwrap();
        let live = registry.insert(arena, Probe(&drops, 8)).unwrap();
        assert_eq!(registry.get(arena, removed).unwrap().1, 7);
        registry.get_mut(arena, live).unwrap().1 = 9;
        assert_eq!(drops.get(), 0);

        drop(registry.remove(arena, removed).unwrap());
        assert_eq!(drops.get(), 1);
        assert_eq!(registry.get(arena, live).unwrap().1, 9);
        registry.destroy_arena(arena).unwrap();
        assert_eq!(drops.get(), 2);
    }
}
