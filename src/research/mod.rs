//! Research snapshot read model (issue-383 §7, M1 PR-8).
//!
//! Separation points (`docs/tasks/issue-383-m1-research-data.md` §11):
//! - **4 存储模型 ↔ 研究读模型**: this module is the read model. It reads the
//!   storage layer's rows through [`read`] (its own row iterators for the bulk
//!   scans, the storage read API for the identity rows) and exposes *datasets*,
//!   not tables. Nothing here is reachable from an MCP tool (AC-M1-10).
//! - **5 核心用例 ↔ 入口适配**: [`plan`] is the pure read-only core the design
//!   calls `research::build_snapshot` — it has no side effects and no I/O
//!   beyond SQL reads. `src/bin/export_research.rs` is the M1 adapter: argument
//!   parsing, output path and one-line reporting only.
//! - **2/3 解析事实 ↔ 派生**: every exported row is either a parsed fact
//!   (`series_bars` 不复权 rows, `adjustment_events`) or carries the
//!   derivation label that produced it (`qfq-exact-v1`, `cal-union-v1`,
//!   `gbbq-cat1-2dp-v1`), so a consumer can recompute the derived layer from
//!   the exported facts. `known_at` is exported as an all-NULL column because
//!   the source has no announcement date (BR-M1-9); nothing is guessed.
//!
//! Failure semantics (§7.1): every rejection is a distinct
//! [`Error`] variant whose `Display` is a single structured line
//! (`EXPORT_DRIFT: …`), and a failed export never leaves a snapshot directory
//! behind — the manifest is written last and the directory is renamed into
//! place only after it exists (§7.2).

pub mod manifest;
pub mod read;

#[cfg(feature = "export")]
pub mod export;
#[cfg(feature = "export")]
pub mod parquet;

#[cfg(test)]
mod tests;

use std::path::Path;

use chrono::NaiveDate;
use rusqlite::Connection;
use serde_json::Value;

use crate::market_data::{Adjustment, CoverageStatus};

/// Structured export failures. The `Display` prefix is the machine-readable
/// code the CLI prints as its one-line reason (§7.1).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The database is not the schema this build exports (D-08: fresh init).
    #[error("EXPORT_SCHEMA: {0}")]
    Schema(String),
    /// A requested binding (catalog version, explicit entity, dataset) is absent.
    #[error("EXPORT_NOT_FOUND: {0}")]
    NotFound(String),
    /// The requested range/selection selects no row (§7.1 "范围为空").
    #[error("EXPORT_EMPTY: {0}")]
    Empty(String),
    /// The selected rows do not all resolve to one source generation.
    #[error("EXPORT_MIXED_GENERATION: {0}")]
    MixedGeneration(String),
    /// The bound inputs changed between planning and the manifest write.
    #[error("EXPORT_DRIFT: {0}")]
    Drift(String),
    /// `<out>/<snapshot_id>` already exists (§7.2: never overwrite).
    #[error("EXPORT_OUTPUT_EXISTS: {0}")]
    OutputExists(String),
    /// Invalid request (argument combination, non-canonical entity, …).
    #[error("EXPORT_INVALID: {0}")]
    Invalid(String),
    /// Stored rows disagree with the identity that claims them.
    #[error("EXPORT_INTEGRITY: {0}")]
    Integrity(String),
    #[error("EXPORT_IO: {0}")]
    Io(String),
    #[error("EXPORT_PARQUET: {0}")]
    Parquet(String),
    #[error("EXPORT_DB: {0}")]
    Db(String),
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

impl From<rusqlite::Error> for Error {
    fn from(value: rusqlite::Error) -> Self {
        Self::Db(value.to_string())
    }
}

impl From<crate::market_storage::Error> for Error {
    fn from(value: crate::market_storage::Error) -> Self {
        match value {
            crate::market_storage::Error::Integrity(message) => Self::Integrity(message),
            other => Self::Db(other.to_string()),
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::Integrity(value.to_string())
    }
}

/// `--dataset` (§7.1). `All` = daily(raw+qfq) + index + calendar + factors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatasetSelection {
    All,
    Daily,
    Index,
    Calendar,
    Factors,
}

impl DatasetSelection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Daily => "daily",
            Self::Index => "index",
            Self::Calendar => "calendar",
            Self::Factors => "factors",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "all" => Self::All,
            "daily" => Self::Daily,
            "index" => Self::Index,
            "calendar" => Self::Calendar,
            "factors" => Self::Factors,
            _ => return None,
        })
    }

    pub fn includes_daily(self) -> bool {
        matches!(self, Self::All | Self::Daily)
    }

    pub fn includes_index(self) -> bool {
        matches!(self, Self::All | Self::Index)
    }

    pub fn includes_calendar(self) -> bool {
        matches!(self, Self::All | Self::Calendar)
    }

    pub fn includes_factors(self) -> bool {
        matches!(self, Self::All | Self::Factors)
    }
}

/// `--adjustment` (§7.1). Only constrains the stock daily files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdjustmentSelection {
    Both,
    Raw,
    Qfq,
}

impl AdjustmentSelection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::Raw => "raw",
            Self::Qfq => "qfq",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "both" => Self::Both,
            "raw" => Self::Raw,
            "qfq" => Self::Qfq,
            _ => return None,
        })
    }

    /// Does this selection include the given series 口径?
    pub fn includes(self, adjustment: Adjustment) -> bool {
        matches!(
            (self, adjustment),
            (Self::Both, _) | (Self::Raw, Adjustment::Raw) | (Self::Qfq, Adjustment::Forward)
        )
    }
}

/// `--entities` / `--catalog-version` (§7.1). They only constrain the stock
/// daily series and the factor sets; index and calendar are registry-driven.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntitySelection {
    CatalogHead,
    CatalogVersion(String),
    Explicit(Vec<String>),
}

impl EntitySelection {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CatalogHead => "catalog",
            Self::CatalogVersion(_) => "catalog_version",
            Self::Explicit(_) => "explicit",
        }
    }

    pub fn explicit(&self) -> Option<&[String]> {
        match self {
            Self::Explicit(list) => Some(list),
            _ => None,
        }
    }
}

/// The validated export request. It is part of the snapshot identity, so every
/// field is normalized here: entities sorted+deduped, dates as `NaiveDate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportRequest {
    pub dataset: DatasetSelection,
    pub adjustment: AdjustmentSelection,
    pub start: Option<NaiveDate>,
    pub end: Option<NaiveDate>,
    pub entities: EntitySelection,
}

impl Default for ExportRequest {
    fn default() -> Self {
        Self {
            dataset: DatasetSelection::All,
            adjustment: AdjustmentSelection::Both,
            start: None,
            end: None,
            entities: EntitySelection::CatalogHead,
        }
    }
}

impl ExportRequest {
    /// Normalize and reject impossible combinations. Called by [`plan`] so the
    /// library and the CLI agree on what a valid request is.
    pub fn validate(mut self) -> Result<Self, Error> {
        if let (Some(start), Some(end)) = (self.start, self.end)
            && start > end
        {
            return Err(Error::Invalid(format!(
                "--start {start} is after --end {end}"
            )));
        }
        if !self.dataset.includes_daily() && self.adjustment != AdjustmentSelection::Both {
            return Err(Error::Invalid(format!(
                "--adjustment {} has no meaning for --dataset {}",
                self.adjustment.as_str(),
                self.dataset.as_str()
            )));
        }
        if let EntitySelection::Explicit(list) = &mut self.entities {
            if list.is_empty() {
                return Err(Error::Invalid("--entities names no entity".into()));
            }
            for entity in list.iter() {
                crate::compact_market::canonical_entity(entity)
                    .map_err(|_| Error::Invalid(format!("non-canonical entity {entity}")))?;
            }
            list.sort();
            list.dedup();
        }
        Ok(self)
    }
}

/// Layer 1 of the four-layer binding (§7.3, BR-M1-7): the catalog the entity
/// set came from. `version_id` is `None` for an explicit entity list — the id
/// must be a function of the inputs, and an explicit list does not read the
/// catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogBinding {
    pub selection: &'static str,
    pub version_id: Option<String>,
    pub member_count: usize,
}

/// Layer 3 of the binding: the one source generation every selected row came
/// from. Two generations in one export is a half-updated snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationBinding {
    pub generation_id: String,
    pub manifest_sha256: String,
}

/// Per-index unit/scope metadata (§3.9, BR-M1-6). It is data, so the exporter
/// copies it into the file and the manifest instead of branching per index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexBinding {
    pub aliases: String,
    pub aggregate_scope: String,
    pub member_count: Option<i64>,
}

/// One exported bar series (layer 2 of the binding).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesEntry {
    pub series_key: i64,
    pub entity: String,
    pub adjustment: Adjustment,
    /// 0=stock / 1=index (2=ETF is M3).
    pub instrument_class: i64,
    pub price_scale: u32,
    pub version_id: String,
    pub record_count: i64,
    pub first_day: NaiveDate,
    pub last_day: NaiveDate,
    pub coverage: CoverageStatus,
    /// Hex of `market_series.source_binding_digest`.
    pub source_binding_digest: String,
    pub index: Option<IndexBinding>,
}

impl SeriesEntry {
    pub fn instrument_class_name(&self) -> &'static str {
        match self.instrument_class {
            crate::market_data::INSTRUMENT_CLASS_STOCK => "stock",
            crate::market_data::INSTRUMENT_CLASS_INDEX => "index",
            _ => "etf",
        }
    }

    pub fn coverage_name(&self) -> &'static str {
        coverage_name(self.coverage)
    }
}

pub fn coverage_name(status: CoverageStatus) -> &'static str {
    match status {
        CoverageStatus::Complete => "complete",
        CoverageStatus::Incomplete => "incomplete",
        CoverageStatus::Unknown => "unknown",
    }
}

/// Which non-bar dataset a [`DerivedEntry`] names (§3.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DerivedDataset {
    TradingCalendar,
    AdjustmentEvents,
}

impl DerivedDataset {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TradingCalendar => "trading_calendar",
            Self::AdjustmentEvents => "adjustment_events",
        }
    }
}

/// One exported non-bar dataset, registered in `derived_dataset_versions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedEntry {
    pub dataset: DerivedDataset,
    /// `exchange_union` for the calendar, the entity for a factor set.
    pub key: String,
    pub version_id: String,
    pub record_count: i64,
    pub first_day: NaiveDate,
    pub last_day: NaiveDate,
    pub digest_format: String,
    pub algorithm_version: String,
    pub source_binding_digest: String,
}

/// One file the export will produce. `rows`/`bytes`/`sha256` are only known
/// after the file is written (§7.3 `files`), and they are excluded from the
/// snapshot identity; the compression is an output option, not an input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePlan {
    pub path: &'static str,
    pub columns: &'static [&'static str],
}

/// A written file, with the layer-4 binding (`sha256`, §7.3/BR-M1-7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    pub path: String,
    pub rows: i64,
    pub bytes: u64,
    pub sha256: String,
}

/// Column order of every file. Kept here (not in the feature-gated writer) so
/// the manifest and the Parquet schema cannot drift: the writer parses a schema
/// string and a test asserts these names come back in this order.
pub const DAILY_COLUMNS: &[&str] = &[
    "entity",
    "trade_day",
    "open",
    "high",
    "low",
    "close",
    "price_scale",
    "volume",
    "amount",
    "r7_raw",
    "volume_quantum",
];
pub const FACTOR_COLUMNS: &[&str] = &[
    "entity",
    "ordinal",
    "effective_day",
    "known_at",
    "category",
    "c1",
    "c2",
    "c3",
    "c4",
    "factor_a",
    "factor_b",
];
pub const CALENDAR_COLUMNS: &[&str] = &["trade_day", "sources"];
pub const INDEX_COLUMNS: &[&str] = &[
    "entity",
    "aliases",
    "trade_day",
    "open",
    "high",
    "low",
    "close",
    "price_scale",
    "volume",
    "amount",
    "aggregate_scope",
    "member_count",
];

pub const DAILY_RAW_FILE: &str = "daily_raw.parquet";
pub const DAILY_QFQ_FILE: &str = "daily_qfq.parquet";
pub const FACTORS_FILE: &str = "factors.parquet";
pub const CALENDAR_FILE: &str = "calendar.parquet";
pub const INDEX_FILE: &str = "index_daily.parquet";

/// The files a request produces, in a fixed order (§7.2).
pub fn file_plans(request: &ExportRequest) -> Vec<FilePlan> {
    let mut files = Vec::new();
    if request.adjustment.includes(Adjustment::Raw) && request.dataset.includes_daily() {
        files.push(FilePlan {
            path: DAILY_RAW_FILE,
            columns: DAILY_COLUMNS,
        });
    }
    if request.adjustment.includes(Adjustment::Forward) && request.dataset.includes_daily() {
        files.push(FilePlan {
            path: DAILY_QFQ_FILE,
            columns: DAILY_COLUMNS,
        });
    }
    if request.dataset.includes_factors() {
        files.push(FilePlan {
            path: FACTORS_FILE,
            columns: FACTOR_COLUMNS,
        });
    }
    if request.dataset.includes_calendar() {
        files.push(FilePlan {
            path: CALENDAR_FILE,
            columns: CALENDAR_COLUMNS,
        });
    }
    if request.dataset.includes_index() {
        files.push(FilePlan {
            path: INDEX_FILE,
            columns: INDEX_COLUMNS,
        });
    }
    files
}

/// The planned snapshot: the identity inputs, the rows to write, and the id
/// derived from them.
#[derive(Debug, Clone)]
pub struct SnapshotPlan {
    pub request: ExportRequest,
    pub snapshot_id: String,
    pub catalog: CatalogBinding,
    pub generation: GenerationBinding,
    pub series: Vec<SeriesEntry>,
    pub derived: Vec<DerivedEntry>,
    pub files: Vec<FilePlan>,
    /// The canonical identity document the `snapshot_id` hashes. Printed by
    /// `--dry-run`; also the base of `manifest.json`.
    pub identity: Value,
}

/// Open the export database read-only (§7.1: forced `mode=ro` + `query_only`).
///
/// The connection cannot write: `SQLITE_OPEN_READ_ONLY` is enforced by the file
/// layer, and `query_only` makes SQLite reject a write statement even if a
/// future caller passes the connection to a write path (AC-M1-9). The schema is
/// checked here so every later query runs against a shape this build knows.
pub fn open_read_only(path: &Path) -> Result<Connection, Error> {
    read::open_read_only(path)
}

/// Build the snapshot plan: a pure read, no side effects (§11 separation 5).
///
/// Rejections, in order: invalid request → missing catalog/entity/dataset →
/// empty selection → mixed source generation. The returned `snapshot_id` is a
/// function of the bound inputs only (§7.3), so two runs over the same state
/// produce the same id (AC-M1-7).
pub fn plan(c: &Connection, request: &ExportRequest) -> Result<SnapshotPlan, Error> {
    let request = request.clone().validate()?;
    let catalog = read::catalog(c, &request.entities)?;

    let mut series = Vec::new();
    if request.dataset.includes_daily() {
        let stocks = read::stock_series(c, &request, catalog.members.as_ref())?;
        if stocks.is_empty() {
            return Err(Error::Empty(
                "the entity selection has no published stock daily series".into(),
            ));
        }
        series.extend(stocks);
    }
    if request.dataset.includes_index() {
        let indexes = read::index_series(c)?;
        if indexes.is_empty() {
            return Err(Error::Empty(
                "no registered index series is published".into(),
            ));
        }
        series.extend(indexes);
    }

    let mut derived = Vec::new();
    if request.dataset.includes_factors() {
        let sets = read::factor_sets(c, &request, catalog.members.as_ref())?;
        if sets.is_empty() {
            return Err(Error::Empty(
                "the entity selection has no published factor set".into(),
            ));
        }
        derived.extend(sets);
    }
    if request.dataset.includes_calendar() {
        derived.push(read::calendar_version(c)?);
    }

    let generation = read::unique_generation(c, &series, &derived)?;
    let identity = manifest::identity(&request, &catalog.binding, &generation, &series, &derived);
    let snapshot_id = manifest::snapshot_id(&identity)?;
    Ok(SnapshotPlan {
        request: request.clone(),
        snapshot_id,
        catalog: catalog.binding,
        generation,
        series,
        derived,
        files: file_plans(&request),
        identity,
    })
}

/// Re-plan on a fresh read-only connection and require the same identity.
///
/// This is the AC-M1-8 guard: the export transaction gives a consistent view of
/// the rows it wrote, but a daily run may have committed while the files were
/// being written. Comparing the freshly planned id with the exported one
/// detects that (the manifest is written only after this succeeds), so a
/// snapshot is never half old / half new.
pub fn verify_unchanged(path: &Path, exported: &SnapshotPlan) -> Result<(), Error> {
    let c = open_read_only(path)?;
    let current = plan(&c, &exported.request)?;
    if current.snapshot_id != exported.snapshot_id {
        return Err(Error::Drift(format!(
            "snapshot inputs changed during the export: planned {} but the database now binds {}",
            exported.snapshot_id, current.snapshot_id
        )));
    }
    Ok(())
}

/// The default output root (§7.1 owner ruling).
pub const DEFAULT_OUTPUT_ROOT: &str = "/root/x-data-exports";

/// Fixed limitations carried by every manifest (§7.3). They travel with the
/// data instead of living in a document only.
pub const LIMITATIONS: &[&str] = &[
    "qfq is anchored at the snapshot's last event and includes events whose \
     effective day is later than the last exported bar; for point-in-time \
     adjustment use raw + factors",
    "adjustment_events.known_at is always NULL: the source carries no \
     announcement date, so the 知晓时间 is unknown and never guessed (BR-M1-9)",
    "trading_calendar is derived data: a row means at least one calendar input \
     index had a bar that day; a missing row inside the span is a non-trading \
     day; a row does NOT mean a security was tradable (BR-M1-5)",
    "amount originates from an f32 source field and is exported as \
     DECIMAL(20,2), i.e. rounded from the stored decimal string",
    "volume_quantum is the per-bar volume precision (100 = a hand-recorded bar \
     precise to 100 shares, NULL = exact to the share); M1 stores no quantum yet",
];
