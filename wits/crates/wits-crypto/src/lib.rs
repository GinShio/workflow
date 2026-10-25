//! `wits-crypto` — the authenticated encryption behind `transcrypt` and the
//! dotfiles overlays.
//!
//! A leaf crate: it names no other `wits` crate and exists so the commands that
//! seal and open packets do not also link the project, build, or action layers.
//! [`crypto`] holds the packet format, the KDFs, and the filter entry points;
//! its module documentation is the contract for wire compatibility.

pub mod crypto;
