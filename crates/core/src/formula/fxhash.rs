//! Lightweight inline FxHash (zero external dependencies) and compact `CellKey` packing
//! for formula engine hot-path hash maps and sets.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};

const FX_K: u64 = 0x517cc1b727220a95;

#[derive(Clone, Copy, Default)]
pub(crate) struct FxHasher {
    hash: u64,
}

impl FxHasher {
    #[inline]
    fn add_to_hash(&mut self, i: u64) {
        self.hash = (self.hash.rotate_left(5) ^ i).wrapping_mul(FX_K);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }

    #[inline]
    fn write(&mut self, mut bytes: &[u8]) {
        while bytes.len() >= 8 {
            let chunk = u64::from_ne_bytes(bytes[..8].try_into().unwrap());
            self.add_to_hash(chunk);
            bytes = &bytes[8..];
        }
        if bytes.len() >= 4 {
            let chunk = u32::from_ne_bytes(bytes[..4].try_into().unwrap()) as u64;
            self.add_to_hash(chunk);
            bytes = &bytes[4..];
        }
        if bytes.len() >= 2 {
            let chunk = u16::from_ne_bytes(bytes[..2].try_into().unwrap()) as u64;
            self.add_to_hash(chunk);
            bytes = &bytes[2..];
        }
        if let Some(&b) = bytes.first() {
            self.add_to_hash(b as u64);
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add_to_hash(i as u64);
    }

    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.add_to_hash(i as u64);
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add_to_hash(i as u64);
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add_to_hash(i);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add_to_hash(i as u64);
    }

    #[inline]
    fn write_i32(&mut self, i: i32) {
        self.add_to_hash(i as u32 as u64);
    }

    #[inline]
    fn write_i64(&mut self, i: i64) {
        self.add_to_hash(i as u64);
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FxBuildHasher;

impl BuildHasher for FxBuildHasher {
    type Hasher = FxHasher;

    #[inline]
    fn build_hasher(&self) -> FxHasher {
        FxHasher::default()
    }
}

pub(crate) type FxHashMap<K, V> = HashMap<K, V, FxBuildHasher>;
pub(crate) type FxHashSet<K> = HashSet<K, FxBuildHasher>;

#[inline]
pub(crate) fn fx_hash_one<T: std::hash::Hash + ?Sized>(val: &T) -> u64 {
    FxBuildHasher.hash_one(val)
}

/// Pack a `(sheet, row, col)` `CellKey` into a compact `u64` when within extended bounds:
/// - row: 26 bits (0..67,108,863, 64x Excel max rows)
/// - col: 18 bits (0..262,143, 16x Excel max cols)
/// - sheet: 20 bits (0..1,048,575 sheets)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PackedCellKey(u64);

impl PackedCellKey {
    #[inline]
    pub(crate) fn pack(key: (usize, u32, u32)) -> Option<Self> {
        let (si, r, c) = key;
        if r < (1 << 26) && c < (1 << 18) && si < (1 << 20) {
            Some(Self(((si as u64) << 44) | ((c as u64) << 26) | (r as u64)))
        } else {
            None
        }
    }

    #[inline]
    pub(crate) fn pack_lossy(key: (usize, u32, u32)) -> Self {
        let (si, r, c) = key;
        Self(
            (((si as u64) & 0xF_FFFF) << 44)
                | (((c as u64) & 0x3_FFFF) << 26)
                | ((r as u64) & 0x3FF_FFFF),
        )
    }

    #[inline]
    pub(crate) fn unpack(self) -> (usize, u32, u32) {
        let v = self.0;
        let r = (v & 0x3FF_FFFF) as u32;
        let c = ((v >> 26) & 0x3_FFFF) as u32;
        let si = (v >> 44) as usize;
        (si, r, c)
    }
}
