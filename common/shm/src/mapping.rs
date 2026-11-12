//! The only module of the workspace that uses `unsafe`.
//!
//! A [`Region`] is a file mapping shared with other processes. Its bytes are
//! never exposed as Rust references: header words are accessed as atomics and
//! payloads move through explicit copies. Payload copies may race with a writer
//! in another process; the ring's seqlock discards every copy that overlapped a
//! write, so a torn copy is never interpreted.

use std::{
    fs::File,
    io, ptr,
    sync::atomic::{AtomicU64, Ordering},
};

use memmap2::{Mmap, MmapMut};

/// Keeps the mapping alive; `Region::ptr` points into it.
enum Map {
    Writable(#[allow(dead_code)] MmapMut),
    ReadOnly(#[allow(dead_code)] Mmap),
}

pub(crate) struct Region {
    map: Map,
    ptr: *mut u8,
    len: usize,
}

// SAFETY: `ptr` points into the mapping owned by `map`, which moves with the
// region; nothing ties the mapping to the creating thread.
unsafe impl Send for Region {}

impl Region {
    /// Maps `file` read-write. The file must be opened for reading and writing.
    pub(crate) fn writable(file: &File) -> io::Result<Self> {
        // SAFETY: other processes may modify the file while it is mapped. The
        // mapping is only accessed through the atomics and raw copies below,
        // never through references to its bytes. Ring files are private (0600)
        // and are never resized after creation.
        let mut map = unsafe { MmapMut::map_mut(file)? };
        let (ptr, len) = (map.as_mut_ptr(), map.len());
        Ok(Self {
            map: Map::Writable(map),
            ptr,
            len,
        })
    }

    /// Maps `file` read-only.
    pub(crate) fn read_only(file: &File) -> io::Result<Self> {
        // SAFETY: as in `writable`; a read-only region is never written.
        let map = unsafe { Mmap::map(file)? };
        let (ptr, len) = (map.as_ptr().cast_mut(), map.len());
        Ok(Self {
            map: Map::ReadOnly(map),
            ptr,
            len,
        })
    }

    /// The 8-byte word at `offset` of a writable region as an atomic.
    pub(crate) fn atomic(&self, offset: usize) -> &AtomicU64 {
        assert!(
            matches!(self.map, Map::Writable(_)),
            "atomic in a read-only region"
        );
        self.word(offset)
    }

    /// Relaxed load of the 8-byte word at `offset`. Callers that need acquire
    /// semantics follow it with an acquire fence.
    pub(crate) fn load_word(&self, offset: usize) -> u64 {
        // Read-only memory permits only relaxed atomic loads.
        self.word(offset).load(Ordering::Relaxed)
    }

    fn word(&self, offset: usize) -> &AtomicU64 {
        self.check(offset, 8);
        assert_eq!(offset % 8, 0, "unaligned atomic at {offset}");
        // SAFETY: the word is in bounds and 8-byte aligned (mappings are page
        // aligned). `AtomicU64` has the layout of `u64`, and every process
        // accesses these words only atomically. On a read-only region the only
        // access is the relaxed load in `load_word`, which is permitted on
        // read-only memory for lock-free 8-byte atomics.
        unsafe { &*self.ptr.add(offset).cast::<AtomicU64>() }
    }

    /// Copies `src` into the region at `offset`.
    pub(crate) fn store(&mut self, offset: usize, src: &[u8]) {
        assert!(
            matches!(self.map, Map::Writable(_)),
            "store into a read-only region"
        );
        self.check(offset, src.len());
        // SAFETY: the destination is in bounds of a writable mapping that no
        // Rust reference covers, and cannot overlap the caller's `src`.
        unsafe { ptr::copy_nonoverlapping(src.as_ptr(), self.ptr.add(offset), src.len()) }
    }

    /// Copies `dst.len()` bytes from the region at `offset` into `dst`.
    pub(crate) fn load(&self, offset: usize, dst: &mut [u8]) {
        self.check(offset, dst.len());
        // SAFETY: the source is in bounds of the mapping and cannot overlap the
        // caller's `dst`. A concurrent writer can only make the copied bytes
        // stale, which the caller's seqlock check detects.
        unsafe { ptr::copy_nonoverlapping(self.ptr.add(offset), dst.as_mut_ptr(), dst.len()) }
    }

    fn check(&self, offset: usize, len: usize) {
        assert!(
            offset.checked_add(len).is_some_and(|end| end <= self.len),
            "range {offset}+{len} outside a {}-byte region",
            self.len
        );
    }
}
