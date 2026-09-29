//! The kitty graphics protocol, from an APC payload to the list of images a
//! frame draws.
//!
//! [`frame`] is the contract with the renderer.  Nothing in this crate knows
//! about GL or about `alacritty_terminal`, which calls in with plain values.

pub mod frame;
