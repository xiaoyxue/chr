use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
};

use super::ResourceError;
use chr_common::scene_frame::Id;

const PENDING: u8 = 0;
const READY: u8 = 1;
const FAILED: u8 = 2;

/// Readiness is not physical completion. Failed work may still hold storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Readiness {
    Pending,
    Ready,
    Failed,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct Key {
    pub slot: usize,
    pub generation: u64,
}

struct Entry<M> {
    metadata: M,
    pixels: Box<[u8]>,
    ticket_id: Id,
    image_id: Id,
    readiness: AtomicU8,
    producer_complete: AtomicBool,
    work: AtomicUsize,
}

struct Cell<M> {
    generation: u64,
    entry: Option<Arc<Entry<M>>>,
    public: bool,
    published: bool,
}

/// The controller serializes pool mutation; workers only access immutable data
/// and atomic dependency state. No pool operation waits for a worker.
pub(super) struct Pool<M> {
    cells: Vec<Cell<M>>,
    max_bytes: usize,
}

impl<M: PartialEq> Pool<M> {
    pub fn new(capacity: usize, max_bytes: usize) -> Result<Self, ResourceError> {
        if capacity == 0 || max_bytes == 0 {
            return Err(ResourceError::InvalidConfiguration);
        }
        Ok(Self {
            cells: (0..capacity)
                .map(|_| Cell {
                    generation: 0,
                    entry: None,
                    public: false,
                    published: false,
                })
                .collect(),
            max_bytes,
        })
    }

    pub fn reclaim(&mut self) {
        for cell in &mut self.cells {
            if !cell.public
                && cell.entry.as_ref().is_some_and(|entry| {
                    entry.producer_complete.load(Ordering::Acquire)
                        && entry.work.load(Ordering::Acquire) == 0
                        && Arc::strong_count(entry) == 1
                })
            {
                cell.entry = None;
            }
        }
    }

    pub fn occupied(&self) -> usize {
        self.cells
            .iter()
            .filter(|cell| cell.entry.is_some())
            .count()
    }

    pub fn reserve(
        &mut self,
        metadata: M,
        pixels: &[u8],
        ticket_id: Id,
        image_id: Id,
    ) -> Result<(Key, ProducerGuard<M>), ResourceError> {
        self.reclaim();
        let used: usize = self
            .cells
            .iter()
            .filter_map(|cell| cell.entry.as_ref())
            .map(|entry| entry.pixels.len())
            .sum();
        if pixels.len() > self.max_bytes.saturating_sub(used) {
            return Err(ResourceError::Capacity);
        }
        let (slot, cell) = self
            .cells
            .iter_mut()
            .enumerate()
            .find(|(_, cell)| cell.entry.is_none() && cell.generation < u64::MAX)
            .ok_or(ResourceError::Capacity)?;
        cell.generation += 1;
        let entry = Arc::new(Entry {
            metadata,
            pixels: pixels.into(),
            ticket_id,
            image_id,
            readiness: AtomicU8::new(PENDING),
            producer_complete: AtomicBool::new(false),
            work: AtomicUsize::new(0),
        });
        cell.entry = Some(Arc::clone(&entry));
        cell.public = true;
        cell.published = false;
        Ok((
            Key {
                slot,
                generation: cell.generation,
            },
            ProducerGuard { entry },
        ))
    }

    fn cell(&self, key: Key) -> Result<&Cell<M>, ResourceError> {
        self.cells
            .get(key.slot)
            .filter(|cell| cell.generation == key.generation && cell.public)
            .ok_or(ResourceError::NotFound)
    }

    pub fn key_for_id(&self, id: &Id, published: bool) -> Option<Key> {
        self.cells.iter().enumerate().find_map(|(slot, cell)| {
            let entry = cell.entry.as_ref()?;
            let expected = if published {
                &entry.image_id
            } else {
                &entry.ticket_id
            };
            (cell.public && cell.published == published && id == expected).then_some(Key {
                slot,
                generation: cell.generation,
            })
        })
    }

    pub fn image_id(&self, key: Key) -> Result<&Id, ResourceError> {
        Ok(&self
            .cell(key)?
            .entry
            .as_ref()
            .ok_or(ResourceError::NotFound)?
            .image_id)
    }

    pub fn publish(&mut self, key: Key, metadata: &M) -> Result<(), ResourceError> {
        let cell = self.cell(key)?;
        if cell.published {
            return Err(ResourceError::InvalidState);
        }
        let entry = cell.entry.as_ref().ok_or(ResourceError::NotFound)?;
        if &entry.metadata != metadata {
            return Err(ResourceError::IncompatibleInput);
        }
        self.cells[key.slot].published = true;
        Ok(())
    }

    pub fn acquire(&self, key: Key) -> Result<Lease<M>, ResourceError> {
        let cell = self.cell(key)?;
        if !cell.published {
            return Err(ResourceError::InvalidState);
        }
        Ok(Lease {
            entry: Arc::clone(cell.entry.as_ref().ok_or(ResourceError::NotFound)?),
        })
    }

    pub fn revoke(&mut self, key: Key, published: bool) -> bool {
        if self.cell(key).is_ok_and(|cell| cell.published == published) {
            self.cells[key.slot].public = false;
            self.reclaim();
            true
        } else {
            false
        }
    }
}

/// Native producer dependency. Move this guard to the completion observer.
///
/// `ready`/`fail` resolve visibility, not lifetime. Call `complete` only after
/// all producer access has stopped. Dropping it without completion marks pending
/// readiness failed but permanently quarantines its bounded cell (fail closed).
#[must_use = "retain the producer guard until physical completion"]
pub struct ProducerGuard<M> {
    entry: Arc<Entry<M>>,
}

impl<M> ProducerGuard<M> {
    pub fn ready(&self) -> Result<(), ResourceError> {
        self.resolve(READY)
    }

    pub fn fail(&self) -> Result<(), ResourceError> {
        self.resolve(FAILED)
    }

    fn resolve(&self, value: u8) -> Result<(), ResourceError> {
        self.entry
            .readiness
            .compare_exchange(PENDING, value, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| ResourceError::InvalidState)
    }

    /// Confirm physical completion. Unresolved readiness becomes failed.
    pub fn complete(self) {
        let _ = self.fail();
        self.entry.producer_complete.store(true, Ordering::Release);
    }
}

impl<M> Drop for ProducerGuard<M> {
    fn drop(&mut self) {
        let _ = self.fail();
    }
}

/// Retained consumer access, including while readiness is pending.
///
/// Keep this lease through synchronous reads. For work continuing after the
/// caller returns (GPU commands, DMA, another task), create a `WorkGuard` before
/// submitting that work and move it to the physical completion observer.
pub struct Lease<M> {
    entry: Arc<Entry<M>>,
}

impl<M> Lease<M> {
    pub fn metadata(&self) -> &M {
        &self.entry.metadata
    }

    pub fn readiness(&self) -> Readiness {
        match self.entry.readiness.load(Ordering::Acquire) {
            READY => Readiness::Ready,
            FAILED => Readiness::Failed,
            _ => Readiness::Pending,
        }
    }

    pub fn pixels(&self) -> Result<&[u8], ResourceError> {
        match self.readiness() {
            Readiness::Ready => Ok(&self.entry.pixels),
            Readiness::Pending => Err(ResourceError::WritePending),
            Readiness::Failed => Err(ResourceError::WriteFailed),
        }
    }

    /// Must precede asynchronous submission; never infer completion from a
    /// cancelled future, a logical timeout, public release, or queue submission.
    pub fn begin_work(&self) -> Result<WorkGuard<M>, ResourceError> {
        self.pixels()?;
        self.entry
            .work
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .map_err(|_| ResourceError::Capacity)?;
        Ok(WorkGuard {
            lease: Self {
                entry: Arc::clone(&self.entry),
            },
        })
    }
}

/// Explicit physical-consumer completion. Drop is deliberately NOT completion.
/// An abandoned guard leaves its cell charged; there is no private overflow pool.
#[must_use = "retain the work guard until physical completion"]
pub struct WorkGuard<M> {
    lease: Lease<M>,
}

impl<M> WorkGuard<M> {
    pub fn pixels(&self) -> &[u8] {
        &self.lease.entry.pixels
    }

    /// Call only once all accesses associated with this guard have finished.
    pub fn complete(self) {
        self.lease.entry.work.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_generation_never_wraps_or_resurrects_old_handles() {
        let mut pool = Pool::new(1, 4).unwrap();
        pool.cells[0].generation = u64::MAX - 1;
        let ticket = Id::try_from("ticket".to_owned()).unwrap();
        let image = Id::try_from("image".to_owned()).unwrap();
        let (key, producer) = pool
            .reserve((), &[0; 4], ticket.clone(), image.clone())
            .unwrap();
        pool.publish(key, &()).unwrap();
        producer.complete();
        assert!(pool.revoke(key, true));
        assert_eq!(pool.occupied(), 0);
        assert!(matches!(
            pool.reserve((), &[0; 4], ticket, image),
            Err(ResourceError::Capacity)
        ));
        assert!(matches!(pool.acquire(key), Err(ResourceError::NotFound)));
    }
}
