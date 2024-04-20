//! User-space shared-memory ring communication.
//!
//! This is IO-uring but for the user-space and agnostic of the actual protocol spoken over the
//! ring between clients. Only the allocation of rings is done via a central server component.
#![deny(clippy::incompatible_msrv)]
#![no_std]

#[cfg(not(target_has_atomic = "64"))]
compile_error!("Requires 8-byte atomics operations");

extern crate alloc;

pub mod client;
pub mod data;
pub mod frame;
/// Contains all code to interacts with the OS directly.
mod uapi;
pub mod server;

#[no_mangle]
extern "C" fn user_ring_have() {}
