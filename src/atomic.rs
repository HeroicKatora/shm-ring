//! A helper module for atomics.
//!
//! Turns out, llvm is pretty bad with atomics. For example consider:
//!
//! ```
//! # use std::sync::atomic::{AtomicU8, Ordering};
//! #[repr(align(4))]
//! struct Values([AtomicU8; 4]);
//!
//! pub fn test(x: &Values) {
//!     for v in &x.0 {
//!         v.load(Ordering::Relaxed);
//!     }
//! }
//! ```
//!
//! There is no semantic reason not to aggregate these loads into a single `mov` on `x86` targets.
//! Indeed, any mov already satisfies the atomicity requirements and the lack of ordering
//! constraints between the memory cells expressed in the code means that these is undetectable.
//! The possible linearizations of the replacement are a strict subset.
//!
//! ```
//! # use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
//! # #[repr(align(4))] struct Values([AtomicU8; 4]);
//! pub fn test(x: &Values) {
//!     let x = unsafe { &*(x as *const _ as *const AtomicU32) };
//!     x.load(Ordering::Relaxed);
//! }
//! ```
//!
//! BUT. llvm does not perform that replacement and instead does 4 loads. Aggrevating. So here is a
//! module for small helpers for casting where this makes sense..
use core::sync::atomic::{AtomicU8, Ordering};

/// An atomic memory region.
#[derive(Copy, Clone)]
pub(crate) struct AtomicMem<'slice> {
    inner: &'slice [AtomicU8],
}

pub(crate) trait Slice {
    fn copy_from_relaxed(&mut self, atomics: &[AtomicU8]);
    fn copy_to_relaxed(&self, atomics: &[AtomicU8]);
}

impl<'slice> From<&'slice [AtomicU8]> for AtomicMem<'slice> {
    fn from(inner: &'slice [AtomicU8]) -> Self {
        AtomicMem { inner }
    }
}

impl core::ops::Deref for AtomicMem<'_> {
    type Target = [AtomicU8];
    fn deref(&self) -> &Self::Target {
        self.inner
    }
}

impl Slice for [u8] {
    fn copy_from_relaxed(&mut self, atomics: &[AtomicU8]) {
        assert_eq!(self.len(), atomics.len());
        for (byte, atom) in self.iter_mut().zip(atomics) {
            // TODO: aggregation as described in module. But that's unsafe.
            *byte = atom.load(Ordering::Relaxed);
        }
    }

    fn copy_to_relaxed(&self, atomics: &[AtomicU8]) {
        assert_eq!(self.len(), atomics.len());
        for (byte, atom) in self.iter().zip(atomics) {
            // TODO: aggregation as described in module. But that's unsafe.
            atom.store(*byte, Ordering::Relaxed);
        }
    }
}
