#![no_std]
#![allow(async_fn_in_trait)]
#![allow(unknown_lints)]
#![allow(renamed_and_removed_lints)]
#![allow(unexpected_cfgs)]
#![allow(clippy::declare_interior_mutable_const)]
#![warn(clippy::large_futures)]
#![warn(clippy::large_stack_frames)]
#![warn(clippy::large_types_passed_by_value)]

#[cfg(feature = "std")]
#[allow(unused_imports)]
#[macro_use]
extern crate std;

#[allow(unused_imports)]
#[macro_use]
extern crate alloc;

#[cfg(feature = "rs-matter-stack")]
pub use stack::*;

pub mod ble;
pub mod mdns;
pub mod multicast;
mod stack;
