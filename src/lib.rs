//! User-space shared-memory ring communication.
//!
//! This is IO-uring but for the user-space and agnostic of the actual protocol spoken over the
//! ring between clients. Only the allocation of rings is done via a central server component.
#![deny(clippy::incompatible_msrv)]
#![no_std]

#[cfg(not(target_has_atomic = "64"))]
compile_error!("Requires 8-byte atomics operations");

extern crate alloc;

mod data;
pub mod frame;
mod server;
