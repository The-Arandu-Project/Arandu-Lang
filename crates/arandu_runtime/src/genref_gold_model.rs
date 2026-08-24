//! G0 executable model for the GenRef Gold contract.
//!
//! This module is deliberately test-only and independent from the MVP host
//! runtime. It freezes the safety properties that the production runtime must
//! satisfy before its ABI or lowering is changed.

#![allow(clippy::unwrap_used)]

use std::cell::Cell;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ArenaId {
    slot: u32,
    generation: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct GenRef {
    arena_slot: u32,
    arena_generation: u32,
    value_slot: u32,
    value_generation: u32,
}

impl GenRef {
    fn arena(self) -> ArenaId {
        ArenaId {
            slot: self.arena_slot,
            generation: self.arena_generation,
        }
    }

    fn is_invalid(self) -> bool {
        self == Self::default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModelError {
    InvalidHandle,
    WrongArena,
    ArenaGone,
    Stale,
    CapacityExhausted,
    InvalidProjection,
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
}

impl<T> Default for Arena<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
        }
    }
}

impl<T> Arena<T> {
    fn insert<const MAX_GENERATION: u32>(
        &mut self,
        arena_id: ArenaId,
        value: T,
    ) -> Result<GenRef, ModelError> {
        if let Some(index) = self.free.pop() {
            let slot = self
                .slots
                .get_mut(usize::try_from(index).map_err(|_| ModelError::CapacityExhausted)?)
                .ok_or(ModelError::CapacityExhausted)?;
            let generation = slot
                .generation
                .checked_add(1)
                .filter(|generation| *generation <= MAX_GENERATION)
                .ok_or(ModelError::CapacityExhausted)?;
            slot.generation = generation;
            slot.state = ValueState::Occupied(value);
            return Ok(GenRef {
                arena_slot: arena_id.slot,
                arena_generation: arena_id.generation,
                value_slot: index,
                value_generation: generation,
            });
        }

        let index = u32::try_from(self.slots.len()).map_err(|_| ModelError::CapacityExhausted)?;
        self.slots.push(ValueSlot {
            generation: 1,
            state: ValueState::Occupied(value),
        });
        Ok(GenRef {
            arena_slot: arena_id.slot,
            arena_generation: arena_id.generation,
            value_slot: index,
            value_generation: 1,
        })
    }

    fn get(&self, handle: GenRef) -> Result<&T, ModelError> {
        let slot = self
            .slots
            .get(usize::try_from(handle.value_slot).map_err(|_| ModelError::Stale)?)
            .ok_or(ModelError::Stale)?;
        if slot.generation != handle.value_generation {
            return Err(ModelError::Stale);
        }
        match &slot.state {
            ValueState::Occupied(value) => Ok(value),
            ValueState::Vacant | ValueState::Retired => Err(ModelError::Stale),
        }
    }

    fn remove<const MAX_GENERATION: u32>(&mut self, handle: GenRef) -> Result<T, ModelError> {
        let index = usize::try_from(handle.value_slot).map_err(|_| ModelError::Stale)?;
        let slot = self.slots.get_mut(index).ok_or(ModelError::Stale)?;
        if slot.generation != handle.value_generation {
            return Err(ModelError::Stale);
        }
        let old = std::mem::replace(
            &mut slot.state,
            if slot.generation == MAX_GENERATION {
                ValueState::Retired
            } else {
                ValueState::Vacant
            },
        );
        match old {
            ValueState::Occupied(value) => {
                if !matches!(slot.state, ValueState::Retired) {
                    self.free.push(handle.value_slot);
                }
                Ok(value)
            }
            ValueState::Vacant | ValueState::Retired => Err(ModelError::Stale),
        }
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

#[derive(Debug)]
struct Registry<T, const MAX_GENERATION: u32> {
    arenas: Vec<ArenaSlot<T>>,
    free: Vec<u32>,
}

impl<T, const MAX_GENERATION: u32> Default for Registry<T, MAX_GENERATION> {
    fn default() -> Self {
        Self {
            arenas: Vec::new(),
            free: Vec::new(),
        }
    }
}

impl<T, const MAX_GENERATION: u32> Registry<T, MAX_GENERATION> {
    fn create_arena(&mut self) -> Result<ArenaId, ModelError> {
        if let Some(index) = self.free.pop() {
            let slot = self
                .arenas
                .get_mut(usize::try_from(index).map_err(|_| ModelError::CapacityExhausted)?)
                .ok_or(ModelError::CapacityExhausted)?;
            let generation = slot
                .generation
                .checked_add(1)
                .filter(|generation| *generation <= MAX_GENERATION)
                .ok_or(ModelError::CapacityExhausted)?;
            slot.generation = generation;
            slot.state = ArenaState::Live(Arena::default());
            return Ok(ArenaId {
                slot: index,
                generation,
            });
        }

        let index = u32::try_from(self.arenas.len()).map_err(|_| ModelError::CapacityExhausted)?;
        self.arenas.push(ArenaSlot {
            generation: 1,
            state: ArenaState::Live(Arena::default()),
        });
        Ok(ArenaId {
            slot: index,
            generation: 1,
        })
    }

    fn insert(&mut self, arena_id: ArenaId, value: T) -> Result<GenRef, ModelError> {
        let arena = self.arena_mut(arena_id)?;
        arena.insert::<MAX_GENERATION>(arena_id, value)
    }

    fn get_in(&self, arena_id: ArenaId, handle: GenRef) -> Result<&T, ModelError> {
        if handle.is_invalid() {
            return Err(ModelError::InvalidHandle);
        }
        if handle.arena() != arena_id {
            return Err(ModelError::WrongArena);
        }
        self.arena(arena_id)?.get(handle)
    }

    fn remove(&mut self, arena_id: ArenaId, handle: GenRef) -> Result<T, ModelError> {
        if handle.is_invalid() {
            return Err(ModelError::InvalidHandle);
        }
        if handle.arena() != arena_id {
            return Err(ModelError::WrongArena);
        }
        self.arena_mut(arena_id)?.remove::<MAX_GENERATION>(handle)
    }

    fn project(
        &self,
        arena_id: ArenaId,
        handle: GenRef,
        offset: u32,
        size: u32,
        owner_size: u32,
    ) -> Result<&T, ModelError> {
        // Temporal validation deliberately happens before offset arithmetic.
        let owner = self.get_in(arena_id, handle)?;
        let end = offset
            .checked_add(size)
            .filter(|end| *end <= owner_size)
            .ok_or(ModelError::InvalidProjection)?;
        let _ = end;
        Ok(owner)
    }

    fn destroy_arena(&mut self, arena_id: ArenaId) -> Result<(), ModelError> {
        let index = usize::try_from(arena_id.slot).map_err(|_| ModelError::ArenaGone)?;
        let slot = self.arenas.get_mut(index).ok_or(ModelError::ArenaGone)?;
        if slot.generation != arena_id.generation {
            return Err(ModelError::ArenaGone);
        }
        if !matches!(slot.state, ArenaState::Live(_)) {
            return Err(ModelError::ArenaGone);
        }
        slot.state = if slot.generation == MAX_GENERATION {
            ArenaState::Retired
        } else {
            self.free.push(arena_id.slot);
            ArenaState::Vacant
        };
        Ok(())
    }

    fn arena(&self, arena_id: ArenaId) -> Result<&Arena<T>, ModelError> {
        let slot = self
            .arenas
            .get(usize::try_from(arena_id.slot).map_err(|_| ModelError::ArenaGone)?)
            .ok_or(ModelError::ArenaGone)?;
        if slot.generation != arena_id.generation {
            return Err(ModelError::ArenaGone);
        }
        match &slot.state {
            ArenaState::Live(arena) => Ok(arena),
            ArenaState::Vacant | ArenaState::Retired => Err(ModelError::ArenaGone),
        }
    }

    fn arena_mut(&mut self, arena_id: ArenaId) -> Result<&mut Arena<T>, ModelError> {
        let slot = self
            .arenas
            .get_mut(usize::try_from(arena_id.slot).map_err(|_| ModelError::ArenaGone)?)
            .ok_or(ModelError::ArenaGone)?;
        if slot.generation != arena_id.generation {
            return Err(ModelError::ArenaGone);
        }
        match &mut slot.state {
            ArenaState::Live(arena) => Ok(arena),
            ArenaState::Vacant | ArenaState::Retired => Err(ModelError::ArenaGone),
        }
    }
}

#[derive(Debug)]
struct DropProbe(Rc<Cell<usize>>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

#[test]
fn zero_handle_is_permanently_invalid() {
    let mut registry = Registry::<i64, 3>::default();
    let arena = registry.create_arena().unwrap();
    assert_eq!(
        registry.get_in(arena, GenRef::default()),
        Err(ModelError::InvalidHandle)
    );
    let first = registry.insert(arena, 7).unwrap();
    assert_ne!(first, GenRef::default());
    assert_ne!(first.arena(), ArenaId::default());
}

#[test]
fn cross_arena_handle_is_rejected_before_slot_lookup() {
    let mut registry = Registry::<i64, 3>::default();
    let first_arena = registry.create_arena().unwrap();
    let second_arena = registry.create_arena().unwrap();
    let first = registry.insert(first_arena, 11).unwrap();
    let second = registry.insert(second_arena, 22).unwrap();
    assert_eq!(first.value_slot, second.value_slot);
    assert_eq!(first.value_generation, second.value_generation);
    assert_eq!(
        registry.get_in(second_arena, first),
        Err(ModelError::WrongArena)
    );
}

#[test]
fn exhausted_value_slot_is_retired_and_stale_never_revives() {
    let mut registry = Registry::<i64, 2>::default();
    let arena = registry.create_arena().unwrap();
    let first = registry.insert(arena, 1).unwrap();
    assert_eq!(registry.remove(arena, first), Ok(1));
    let second = registry.insert(arena, 2).unwrap();
    assert_eq!(first.value_slot, second.value_slot);
    assert_ne!(first.value_generation, second.value_generation);
    assert_eq!(registry.remove(arena, second), Ok(2));

    let third = registry.insert(arena, 3).unwrap();
    assert_ne!(
        third.value_slot, first.value_slot,
        "retired slot was reused"
    );
    assert_eq!(registry.get_in(arena, first), Err(ModelError::Stale));
}

#[test]
fn destroyed_arena_identity_cannot_be_revalidated_by_address_reuse() {
    let mut registry = Registry::<i64, 3>::default();
    let first_arena = registry.create_arena().unwrap();
    let stale = registry.insert(first_arena, 9).unwrap();
    registry.destroy_arena(first_arena).unwrap();
    let replacement = registry.create_arena().unwrap();
    assert_eq!(first_arena.slot, replacement.slot);
    assert_ne!(first_arena.generation, replacement.generation);
    assert_eq!(
        registry.get_in(first_arena, stale),
        Err(ModelError::ArenaGone)
    );
    assert_eq!(
        registry.get_in(replacement, stale),
        Err(ModelError::WrongArena)
    );
}

#[test]
fn exhausted_arena_slot_is_retired_and_never_reused() {
    let mut registry = Registry::<i64, 2>::default();
    let first = registry.create_arena().unwrap();
    registry.destroy_arena(first).unwrap();
    let second = registry.create_arena().unwrap();
    assert_eq!(first.slot, second.slot);
    registry.destroy_arena(second).unwrap();

    let third = registry.create_arena().unwrap();
    assert_ne!(third.slot, first.slot, "retired arena slot was reused");
}

#[test]
fn remove_and_arena_destroy_drop_each_payload_exactly_once() {
    let drops = Rc::new(Cell::new(0));
    let mut registry = Registry::<DropProbe, 3>::default();
    let arena = registry.create_arena().unwrap();
    let removed = registry
        .insert(arena, DropProbe(Rc::clone(&drops)))
        .unwrap();
    let live = registry
        .insert(arena, DropProbe(Rc::clone(&drops)))
        .unwrap();

    drop(registry.remove(arena, removed).unwrap());
    assert_eq!(drops.get(), 1);
    assert!(registry.get_in(arena, live).is_ok());
    registry.destroy_arena(arena).unwrap();
    assert_eq!(drops.get(), 2);
}

#[test]
fn projection_validates_owner_before_checked_offset_arithmetic() {
    let mut registry = Registry::<[u8; 8], 3>::default();
    let arena = registry.create_arena().unwrap();
    let owner = registry.insert(arena, [0; 8]).unwrap();
    assert!(registry.project(arena, owner, 4, 4, 8).is_ok());
    assert_eq!(
        registry.project(arena, owner, 7, 2, 8),
        Err(ModelError::InvalidProjection)
    );
    assert_eq!(
        registry.project(arena, owner, u32::MAX, 2, 8),
        Err(ModelError::InvalidProjection)
    );

    let _ = registry.remove(arena, owner).unwrap();
    assert_eq!(
        registry.project(arena, owner, u32::MAX, 2, 8),
        Err(ModelError::Stale),
        "stale owner must fail before projection arithmetic"
    );
}

#[test]
fn mvp_wrapping_generation_can_revalidate_a_stale_handle() {
    let stale_generation = 0_u8;
    let mut current_generation = stale_generation;
    for _ in 0..=u8::MAX {
        current_generation = current_generation.wrapping_add(1);
    }
    assert_eq!(current_generation, stale_generation);
}

#[test]
fn mvp_index_and_generation_do_not_identify_the_owning_arena() {
    let first_arena_handle = (0_u32, 0_u32);
    let second_arena_handle = (0_u32, 0_u32);
    assert_eq!(first_arena_handle, second_arena_handle);
}

#[test]
fn mvp_zero_remove_sentinel_is_ambiguous_with_a_valid_payload() {
    let valid_removed_payload = 0_i64;
    let stale_remove_sentinel = 0_i64;
    assert_eq!(valid_removed_payload, stale_remove_sentinel);
}
