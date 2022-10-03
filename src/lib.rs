#![doc = include_str!("../README.md")]
#![no_std]
#![feature(alloc_layout_extra)]
#![feature(allocator_api)]
#![feature(anonymous_lifetime_in_impl_trait)]
#![feature(int_roundings)]
#![cfg_attr(feature = "std", feature(io_slice_advance))]
#![feature(maybe_uninit_slice)]
#![feature(ptr_metadata)]
#![feature(slice_ptr_get)]

#[cfg(feature = "std")]
extern crate std;

extern crate alloc;

mod imp;
mod io;

pub use self::{imp::BlockedVec, io::*};
