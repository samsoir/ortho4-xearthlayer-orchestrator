//! The HTTP surface. Wire types are owned here, serialized with serde;
//! the port types in oxo-tasks stay serde-free, so the wire contract can
//! change without touching the port.

pub mod error;
pub mod wire;
