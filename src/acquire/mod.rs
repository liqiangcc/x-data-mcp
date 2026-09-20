//! Acquisition service: the only network-touching half of daily sync.
//!
//! Owns official-file remote probes and downloads into immutable
//! generations (see `crate::generation`). State is durable SQLite (D3) —
//! every request has an idempotent identity, the async unit of work is an
//! acquisition task, and crash points reconcile mechanically at open.
//! Verification and activation are separate, explicit, auditable steps:
//! downloading is never synonymous with verified or active.
pub mod fetch;
pub mod service;
pub mod store;
pub mod wire;

pub use service::{AcquireService, Source};
