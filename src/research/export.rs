//! Snapshot export orchestration (feature `export`, §7.2/§7.3).
//!
//! The writer is deliberately split into three steps so the AC-M1-8 guard is
//! testable without a hook: [`SnapshotWriter::create`] plans inside one
//! read-only transaction, [`SnapshotWriter::write_files`] writes the Parquet
//! files into a temporary directory, and [`SnapshotWriter::finalize`] re-plans
//! on a *fresh* connection, writes `manifest.json` and only then renames the
//! directory into place. A daily run that commits between the steps changes the
//! freshly planned identity, so `finalize` fails and the temporary directory is
//! removed — a half-new / half-old snapshot cannot exist on disk.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::Connection;
use serde_json::{Value, json};

use super::{
    DEFAULT_OUTPUT_ROOT, Error, ExportRequest, FileStat, SnapshotPlan, manifest, open_read_only,
    parquet, verify_unchanged,
};

/// `--compression` (§7.1). Snappy is pure Rust, so the export feature adds no C
/// toolchain dependency (§7.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    Snappy,
    None,
}

impl Compression {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Snappy => "snappy",
            Self::None => "none",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "snappy" => Self::Snappy,
            "none" => Self::None,
            _ => return None,
        })
    }
}

/// Output options. They never enter the snapshot identity (§7.3): the same
/// inputs with a different compression produce the same `snapshot_id` and
/// different file bytes.
#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub out_root: PathBuf,
    pub compression: Compression,
    /// Rows per Parquet row group (§7.1 `--row-group-rows`).
    pub row_group_rows: usize,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            out_root: PathBuf::from(DEFAULT_OUTPUT_ROOT),
            compression: Compression::Snappy,
            row_group_rows: 65_536,
        }
    }
}

/// The result of a successful export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportOutcome {
    pub snapshot_id: String,
    pub directory: PathBuf,
    pub files: Vec<FileStat>,
    pub created_at: String,
    pub started_at: String,
    pub finished_at: String,
    /// WAL growth while the read transaction was open (§7.3). Informational:
    /// a long export makes the daily writer's WAL grow, and the number is
    /// reported instead of blocking.
    pub wal_bytes_delta: i64,
}

/// The temporary `<out>/.tmp-<pid>` directory, removed unless disarmed.
struct TempDir {
    path: PathBuf,
    armed: bool,
}

impl TempDir {
    fn new(root: &Path) -> Result<Self, Error> {
        let path = root.join(format!(".tmp-{}", std::process::id()));
        fs::create_dir(&path).map_err(|e| {
            Error::Io(format!(
                "cannot create the temporary directory {}: {e}",
                path.display()
            ))
        })?;
        Ok(Self { path, armed: true })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn wal_bytes(db: &Path) -> u64 {
    let mut path = db.as_os_str().to_owned();
    path.push("-wal");
    fs::metadata(Path::new(&path))
        .map(|meta| meta.len())
        .unwrap_or(0)
}

/// An export in progress. See the module docs for the three steps.
pub struct SnapshotWriter {
    db: PathBuf,
    options: ExportOptions,
    plan: SnapshotPlan,
    temp: TempDir,
    files: Vec<FileStat>,
    connection: Option<Connection>,
    started_at: DateTime<Utc>,
    wal_before: u64,
}

impl SnapshotWriter {
    /// Plan the snapshot and reserve the temporary directory.
    ///
    /// The read transaction starts here, so every file written by
    /// [`Self::write_files`] sees one committed state (§7.3). The final
    /// directory must not exist yet: the same `snapshot_id` means the same
    /// content, so an existing one is either already correct or the digest is
    /// broken — never silently overwritten (§7.2).
    pub fn create(
        db: &Path,
        request: &ExportRequest,
        options: &ExportOptions,
    ) -> Result<Self, Error> {
        if options.row_group_rows == 0 {
            return Err(Error::Invalid("--row-group-rows must be positive".into()));
        }
        let started_at = Utc::now();
        let wal_before = wal_bytes(db);
        let connection = open_read_only(db)?;
        connection
            .execute_batch("BEGIN")
            .map_err(|e| Error::Db(e.to_string()))?;
        let plan = super::plan(&connection, request)?;
        let directory = options.out_root.join(&plan.snapshot_id);
        if directory.exists() {
            return Err(Error::OutputExists(format!(
                "{} already exists; a snapshot is never overwritten",
                directory.display()
            )));
        }
        fs::create_dir_all(&options.out_root)?;
        let temp = TempDir::new(&options.out_root)?;
        Ok(Self {
            db: db.to_path_buf(),
            options: options.clone(),
            plan,
            temp,
            files: Vec::new(),
            connection: Some(connection),
            started_at,
            wal_before,
        })
    }

    pub fn plan(&self) -> &SnapshotPlan {
        &self.plan
    }

    /// Write every planned Parquet file into the temporary directory.
    pub fn write_files(&mut self) -> Result<(), Error> {
        let connection = self
            .connection
            .as_ref()
            .ok_or_else(|| Error::Invalid("the writer is already finalized".into()))?;
        let files = parquet::write_all(
            self.temp.path(),
            connection,
            &self.plan,
            self.options.compression,
            self.options.row_group_rows,
        )?;
        let rows: i64 = files.iter().map(|file| file.rows).sum();
        if rows == 0 {
            return Err(Error::Empty(
                "the request selected no row in this database".into(),
            ));
        }
        self.files = files;
        Ok(())
    }

    /// Re-verify the inputs, write `manifest.json` and rename into place.
    pub fn finalize(mut self) -> Result<ExportOutcome, Error> {
        if self.files.is_empty() {
            return Err(Error::Invalid(
                "write_files must run before finalize".into(),
            ));
        }
        // AC-M1-8: the bound inputs must still be the ones the files were
        // written from. This runs before the manifest exists, so a drift
        // leaves no valid snapshot behind.
        verify_unchanged(&self.db, &self.plan)?;
        let finished_at = Utc::now();
        let created_at = finished_at.to_rfc3339_opts(SecondsFormat::Secs, true);
        let started_at = self.started_at.to_rfc3339_opts(SecondsFormat::Secs, true);
        let wal_bytes_delta = wal_bytes(&self.db) as i64 - self.wal_before as i64;
        let files = manifest::files_value(
            &self.plan.files,
            Some(&self.files),
            self.options.compression.as_str(),
        );
        let export = json!({
            "started_at": started_at.clone(),
            "finished_at": created_at.clone(),
            "wal_bytes_delta": wal_bytes_delta,
            "row_group_rows": self.options.row_group_rows,
            "compression": self.options.compression.as_str(),
        });
        let document = manifest::document(
            &self.plan.identity,
            &self.plan.snapshot_id,
            &created_at,
            files,
            export,
        );
        let text = serde_json::to_string_pretty(&document)?;
        fs::write(self.temp.path().join("manifest.json"), text.as_bytes())?;
        let directory = self.options.out_root.join(&self.plan.snapshot_id);
        if directory.exists() {
            return Err(Error::OutputExists(format!(
                "{} appeared while the snapshot was being written",
                directory.display()
            )));
        }
        fs::rename(self.temp.path(), &directory)?;
        self.temp.disarm();
        // Close the read transaction before reporting success.
        drop(self.connection.take());
        Ok(ExportOutcome {
            snapshot_id: self.plan.snapshot_id.clone(),
            directory,
            files: self.files,
            created_at,
            started_at,
            finished_at: finished_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            wal_bytes_delta,
        })
    }
}

/// One-shot export: plan, write, verify, publish.
pub fn export(
    db: &Path,
    request: &ExportRequest,
    options: &ExportOptions,
) -> Result<ExportOutcome, Error> {
    let mut writer = SnapshotWriter::create(db, request, options)?;
    writer.write_files()?;
    writer.finalize()
}

/// `--dry-run` (§7.1): plan and render the manifest without writing anything.
///
/// `files[].rows`/`bytes`/`sha256` are `null` here: they are only known after
/// the file is written, and they are excluded from the snapshot identity, so
/// the id printed by a dry run is the id a real run would produce.
pub fn dry_run(
    db: &Path,
    request: &ExportRequest,
    options: &ExportOptions,
) -> Result<(SnapshotPlan, Value), Error> {
    let connection = open_read_only(db)?;
    // §7.3: `--dry-run` plans inside one read transaction, like a real export.
    connection
        .execute_batch("BEGIN")
        .map_err(|e| Error::Db(e.to_string()))?;
    let plan = super::plan(&connection, request)?;
    let files = manifest::files_value(&plan.files, None, options.compression.as_str());
    let export = json!({
        "dry_run": true,
        "row_group_rows": options.row_group_rows,
        "compression": options.compression.as_str(),
    });
    let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let document = manifest::document(
        &plan.identity,
        &plan.snapshot_id,
        &created_at,
        files,
        export,
    );
    Ok((plan, document))
}
