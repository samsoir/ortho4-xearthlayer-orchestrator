//! Region specification model and validation for the Ortho4 XEarthLayer
//! Orchestrator.
//!
//! This crate performs no I/O beyond being handed specification text. Any
//! validation that requires looking at an Ortho4XP installation, a
//! filesystem or a network is environmental validation and belongs to the
//! control plane, not here.

#![forbid(unsafe_code)]

pub mod tile;

pub use tile::{TileId, TileIdParseError};
