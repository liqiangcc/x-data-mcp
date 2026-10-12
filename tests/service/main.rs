// Consolidated integration test target (#272 S2).
mod acquire_crash_points;
mod acquire_service;
#[cfg(target_os = "linux")]
mod local_runtime;
mod official_files;
mod official_market;
mod release_smoke;
mod sqlite_binaries;
