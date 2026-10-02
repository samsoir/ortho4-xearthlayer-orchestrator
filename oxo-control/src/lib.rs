//! OXO control plane library: the planner that atomizes a region
//! specification into tasks, the HTTP API workers claim and report
//! through, and the reaper loop that drives lease expiry.
//!
//! This crate depends on the task-store **port**, never on an adapter:
//! no sqlx, no database driver. The composition root (`oxo-controld`)
//! injects the adapter behind `Arc<dyn TaskStore>`.

#![forbid(unsafe_code)]

pub mod planner;
