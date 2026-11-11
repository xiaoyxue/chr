use std::sync::Arc;

use super::SceneError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Key {
    owner: u64,
    slot: usize,
    generation: u64,
}

#[derive(Clone, Debug)]
struct Slot<T> {
    generation: u64,
    value: Option<Arc<T>>,
}

/// Private storage shared by the concrete resource types, not a public framework.
#[derive(Clone, Debug)]
pub(super) struct Store<T> {
    owner: u64,
    slots: Vec<Slot<T>>,
    free: Vec<usize>,
}

impl<T> Store<T> {
    pub fn new(owner: u64) -> Self {
        Self {
            owner,
            slots: Vec::new(),
            free: Vec::new(),
        }
    }

    pub fn get(&self, key: Key) -> Result<&Arc<T>, SceneError> {
        if key.owner != self.owner {
            return Err(SceneError::InvalidHandle);
        }
        self.slots
            .get(key.slot)
            .filter(|slot| slot.generation == key.generation)
            .and_then(|slot| slot.value.as_ref())
            .ok_or(SceneError::InvalidHandle)
    }

    pub fn insert(&mut self, value: T) -> Key {
        let index = if let Some(&index) = self.free.last() {
            self.free.pop();
            self.slots[index].value = Some(Arc::new(value));
            index
        } else {
            let index = self.slots.len();
            self.slots.push(Slot {
                generation: 0,
                value: Some(Arc::new(value)),
            });
            index
        };
        Key {
            owner: self.owner,
            slot: index,
            generation: self.slots[index].generation,
        }
    }

    pub fn replace(&mut self, key: Key, value: T) -> Result<(), SceneError> {
        self.get(key)?;
        self.slots[key.slot].value = Some(Arc::new(value));
        Ok(())
    }

    pub fn remove(&mut self, key: Key) -> Result<(), SceneError> {
        self.get(key)?;
        let next = key.generation.checked_add(1);
        let slot = &mut self.slots[key.slot];
        slot.value = None;
        if let Some(generation) = next {
            slot.generation = generation;
            self.free.push(key.slot);
        }
        // A generation that cannot advance retires its slot instead of aliasing an old key.
        Ok(())
    }

    pub fn iter(&self) -> impl Iterator<Item = (Key, &Arc<T>)> {
        self.slots.iter().enumerate().filter_map(|(index, slot)| {
            slot.value.as_ref().map(|value| {
                (
                    Key {
                        owner: self.owner,
                        slot: index,
                        generation: slot.generation,
                    },
                    value,
                )
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_generation_retires_the_slot() {
        let mut store = Store::new(1);
        let mut old = store.insert(10);
        store.slots[0].generation = u64::MAX;
        old.generation = u64::MAX;
        store.remove(old).unwrap();
        let next = store.insert(20);
        assert_ne!(old.slot, next.slot);
        assert!(matches!(store.get(old), Err(SceneError::InvalidHandle)));
    }
}
