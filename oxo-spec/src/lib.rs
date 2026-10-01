//! Region specification model and validation for the Ortho4 XEarthLayer
//! Orchestrator.
//!
//! This crate performs no I/O beyond being handed specification text. Any
//! validation that requires looking at an Ortho4XP installation, a
//! filesystem or a network is environmental validation and belongs to the
//! control plane, not here.

#![forbid(unsafe_code)]

pub mod metadata;
pub mod parameters;
pub mod policy;
pub mod raw;
pub mod target;
pub mod tile;

pub use metadata::Metadata;
pub use parameters::{ProductionParameters, RESERVED_RAW_KEYS, ZOOM_MAX, ZOOM_MIN};
pub use policy::FailurePolicy;
pub use raw::RawRegionSpec;
pub use target::TargetLocation;
pub use tile::{TileId, TileIdParseError};
