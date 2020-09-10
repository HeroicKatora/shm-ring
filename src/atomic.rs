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

// Currently empty due to unused.
