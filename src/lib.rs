pub mod calendar;
pub mod catalog;
pub mod catalog_tdx;
pub mod collector;
pub mod compact_market;
pub mod dbf;
pub mod execution;
pub mod gbbq;
pub mod index_registry;
pub mod market_data;
pub mod mcp;
pub mod official_files;
pub mod official_market;
pub mod official_security;
mod progress_trace;
pub mod publish;
pub mod qfq;
pub mod security_master;
mod stage_trace;
pub mod storage;
pub mod task;
pub mod worker;

pub mod yearly;

pub mod day_file;

pub mod query;

pub mod file_consistency;
pub mod generation;
pub mod snapshot;

pub mod sqlite_tasks;
pub mod task_control;
pub mod trading_calendar;
pub mod trading_rules;

pub mod sqlite_task_mcp;

pub mod hash;

pub mod industry;

pub mod market_storage;
pub mod sqlite_market;

/// Research snapshot export (issue-383 §7, M1 PR-8). The read model is always
/// compiled; the Parquet writer and the `export_research` binary are behind the
/// `export` feature.
pub mod research;

pub mod data_supply;
pub mod sqlite_catalog;

pub mod sqlite_supply;

pub mod metrics;

pub mod sqlite_metrics;

pub mod market_update;
pub mod yearly_publication;

pub mod task_verification;

pub mod publication_verification;
pub mod sqlite_verification;

#[cfg(target_os = "linux")]
pub mod verification_runtime;

#[cfg(target_os = "linux")]
pub mod sqlite_verification_inspection;
#[cfg(target_os = "linux")]
pub mod sqlite_verification_runtime;
#[cfg(target_os = "linux")]
pub mod verification_execution;
#[cfg(target_os = "linux")]
pub mod verification_inspection;

pub mod gap_verification;

pub mod source_store;

pub mod acquire;

#[cfg(target_os = "linux")]
pub mod local_runtime;

#[cfg(target_os = "linux")]
mod local_filesystem;
