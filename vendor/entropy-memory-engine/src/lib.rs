#![allow(
    clippy::manual_clamp,
    clippy::unnecessary_sort_by,
    clippy::len_without_is_empty
)]

pub mod consolidation;
pub mod core;
pub mod export;
pub mod l0;
pub mod l1;
pub mod l2;
pub mod l3;
pub mod memory_store;
pub mod metrics;
pub mod orchestrator;
pub mod phase1;
pub mod phase2;
pub mod phase4;
pub mod phase5;
pub mod router;
