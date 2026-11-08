//! ETF factor-check records in the source store (`#607` PR-B, D-92 Q2 (c)).
//!
//! Layer position: this is a **source-store evidence scope**, not a market
//! table. `docs/tasks/issue-607-etf-qfq.md` §3.2.2 chose option (c) for the
//! per-event verification result: it is registered as content-addressed
//! evidence instead of a new `market` column or table, because a schema bump in
//! this repository costs a full fresh-init rebuild on every deployment
//! (§3.2.1, `docs/new-host-runbook.md:282-284`) while `switch_binary.sh` stops
//! being usable at all (`src/research/read.rs:73`,
//! `src/sqlite_market/generation_facts.rs:225`).
//!
//! ```text
//! one check record  =  (entity, ordinal) + check_status + basis + evidence
//! record id         =  sha256 of the canonical JSON of that record
//! store line        =  source_id etf_factor_checks, kind archive_import,
//!                      parser_version etf-factor-checks-v1, origin
//!                      etf-factor-checks:<record sha256>
//! ```
//!
//! The record's **content** carries every field a reviewer needs to reproduce
//! the verdict (§3.2.2): the status, the basis (official `qss` or a fund
//! announcement), the evidence URL plus its sha256, the announced/official
//! value and the gbbq value, the forward-price difference with the D-92 tick
//! verdict, the check date, and `evidence_digest` — the digest of the stored
//! evidence bundle of the event the record checks
//! (`adjustment_events.evidence_digest`,
//! `migrations/tasks/027_etf_factor_events.sql:32`).
//!
//! Records are append-only: a later verdict is a **new record with a new
//! sha256**, never a rewrite of an old one. The blocking decision
//! ([`super::etf_factor_gate`]) therefore reads the whole set and picks the
//! newest record per `(entity, ordinal, evidence_digest)` by `checked_at`.
//!
//! This module is **not** a production writer: the D-92 ruling keeps the real
//! import a separate operations task after deployment. `import` here is the
//! operator entry (`source_store import --format etf-factor-checks`) and is
//! idempotent, dry-runnable and fail-closed per row.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::import::{ImportFormat, ImportOptions, ImportReport, ImportedMember, SkippedMember};
use super::replay::Scope;
use super::{Registration, SourceStore, StoreError, StoreKind};

/// Parser/business version of a check record (registered in
/// `docs/conventions`-style algorithm lists as `etf-factor-checks-v1`).
pub const CHECKS_VERSION: &str = "etf-factor-checks-v1";
/// `manifest.jsonl` `source_id` of a frozen check record.
pub const CHECKS_SOURCE_ID: &str = "etf_factor_checks";
/// Origin prefix of one record registration:
/// `etf-factor-checks:<record sha256>`.
pub const ORIGIN_PREFIX: &str = "etf-factor-checks:";

/// Canonical JSON media type of a stored record (one compact line, keys in the
/// [`EtfFactorCheck`] field order, no trailing newline).
pub const RECORD_FORMAT: &str = "etf-factor-checks-v1+json";

/// Verdict of one `(entity, ordinal)` check (design §3.1/§3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// Checked against the X-day official `qss` and within one tick.
    VerifiedOfficial,
    /// Checked against the fund announcement (amount, registration day R and
    /// ex-dividend day X).
    VerifiedAnnouncement,
    /// The event is real but provably has no price effect (D-91 §3: an event
    /// before the first bar, or a conversion that changes nothing). Such a
    /// record never blocks and never contributes to the chain.
    NoEffect,
    /// Neither basis could verify the event: **blocks the whole entity**
    /// (D-92 Q1 (a)).
    Unverified,
}

impl CheckStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VerifiedOfficial => "verified_official",
            Self::VerifiedAnnouncement => "verified_announcement",
            Self::NoEffect => "no_effect",
            Self::Unverified => "unverified",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "verified_official" => Self::VerifiedOfficial,
            "verified_announcement" => Self::VerifiedAnnouncement,
            "no_effect" => Self::NoEffect,
            "unverified" => Self::Unverified,
            _ => return None,
        })
    }

    /// Whether the verdict lets the event into the forward chain.
    pub fn admits(self) -> bool {
        match self {
            Self::VerifiedOfficial | Self::VerifiedAnnouncement | Self::NoEffect => true,
            Self::Unverified => false,
        }
    }

    /// Whether the verdict excludes the event from the chain although it was
    /// looked at.
    pub fn is_no_effect(self) -> bool {
        self == Self::NoEffect
    }
}

impl fmt::Display for CheckStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which evidence the verdict came from (design §3.1 "判定口径").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckBasis {
    /// The exchange's official daily quote (`qss`), used on the event's
    /// effective day X for Shenzhen ETFs (D-91 §3).
    OfficialQss,
    /// A fund announcement (dividend/distribution or share conversion).
    Announcement,
    /// No basis could verify the event; `basis_ref`/`evidence_url` then carry
    /// the attempted source, or are empty for "nothing was tried".
    None,
}

impl CheckBasis {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OfficialQss => "official_qss",
            Self::Announcement => "announcement",
            Self::None => "none",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "official_qss" => Self::OfficialQss,
            "announcement" => Self::Announcement,
            "none" => Self::None,
            _ => return None,
        })
    }
}

impl fmt::Display for CheckBasis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One `(entity, ordinal)` check record. Field order **is** the canonical JSON
/// order used to compute [`EtfFactorCheck::digest`], so inserting a field in
/// the middle would silently change every record id — append new optional
/// fields at the end with `serde(default)` instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EtfFactorCheck {
    /// Manifest version of the record shape (`etf-factor-checks-v1`).
    pub record_version: String,
    /// `sh.510180`-style entity.
    pub entity: String,
    /// `adjustment_events.ordinal` of the event this record checks.
    pub ordinal: i64,
    /// The event's effective day (design `R`→`X`; the gbbq event day).
    pub effective_day: String,
    /// Category of the checked event (1 = 每 10 份派现, 11 = 份额折算).
    pub category: u8,
    /// Verdict (§3.1/§3.2).
    pub check_status: CheckStatus,
    /// Evidence class the verdict came from.
    pub basis: CheckBasis,
    /// Human/machine label of the exact evidence, e.g.
    /// `szse_1815_stock:2024-12-02` or `sse_ann:2013-04-17`.
    pub basis_ref: String,
    /// URL of the official row or the announcement.
    pub evidence_url: String,
    /// sha256 of the bytes behind `evidence_url` (document or frozen blob).
    pub evidence_sha256: String,
    /// The value the evidence states, as the source's own decimal text: `c1`
    /// (元/10 份) for category 1, the conversion ratio for category 11. Empty
    /// when the basis carries no value (`no_effect` without a document).
    pub announced_value: String,
    /// The gbbq factor value for the same event (the fact under check).
    pub gbbq_value: String,
    /// `official_qss - computed forward price` in 元, as a decimal text; empty
    /// when no price could be compared.
    pub price_delta: String,
    /// D-92's one-tick verdict on that difference.
    #[serde(default)]
    pub within_one_tick: Option<bool>,
    /// When the check was made (`YYYY-MM-DD`).
    pub checked_at: String,
    /// Hex of the 32-byte `adjustment_events.evidence_digest` of the checked
    /// event. A changed event digest is a different key, so an old record
    /// stops matching and the entity blocks until it is re-checked (D-92 Q10).
    pub evidence_digest: String,
}

impl EtfFactorCheck {
    /// The check key: which stored fact this record is about.
    pub fn key(&self) -> (String, i64, String) {
        (
            self.entity.clone(),
            self.ordinal,
            self.evidence_digest.clone(),
        )
    }

    /// Canonical JSON bytes of the record: compact serialization in declaration
    /// order, no trailing newline. This is what is stored and what the id
    /// digests.
    pub fn canonical_json(&self) -> Result<String, CheckError> {
        serde_json::to_string(self).map_err(|error| CheckError::invalid(error.to_string()))
    }

    /// Content-addressed id: sha256 hex of [`Self::canonical_json`].
    pub fn digest(&self) -> Result<String, CheckError> {
        Ok(crate::hash::sha256(self.canonical_json()?.as_bytes()))
    }

    /// Parse one record line and run every per-field check. Fail-closed: an
    /// unrecognized `check_status`, a missing sha256 or a non-record version is
    /// an error, never a default.
    pub fn parse_line(line: &str) -> Result<Self, CheckError> {
        let record: Self = serde_json::from_str(line)
            .map_err(|error| CheckError::invalid(format!("not a check record: {error}")))?;
        record.validate()?;
        Ok(record)
    }

    /// Field checks shared by the CSV/JSONL importer and by the replay reader.
    pub fn validate(&self) -> Result<(), CheckError> {
        if self.record_version != CHECKS_VERSION {
            return Err(CheckError::invalid(format!(
                "record_version {:?} is not {CHECKS_VERSION}",
                self.record_version
            )));
        }
        if !is_entity(&self.entity) {
            return Err(CheckError::invalid(format!(
                "entity {:?} is not <sh|sz|bj>.<6 digits>",
                self.entity
            )));
        }
        if self.ordinal < 0 {
            return Err(CheckError::invalid("ordinal must be >= 0"));
        }
        if !matches!(self.category, 1 | 11) {
            return Err(CheckError::invalid(format!(
                "category {} is not a stored ETF factor category (1|11)",
                self.category
            )));
        }
        if !is_day(&self.effective_day) {
            return Err(CheckError::invalid(format!(
                "effective_day {:?} is not YYYY-MM-DD",
                self.effective_day
            )));
        }
        if !is_day(&self.checked_at) {
            return Err(CheckError::invalid(format!(
                "checked_at {:?} is not YYYY-MM-DD",
                self.checked_at
            )));
        }
        // D-91 §3 evidence pair: a verified verdict names its bytes; an
        // `unverified` one may name the attempted (unusable) evidence or none.
        if matches!(
            self.check_status,
            CheckStatus::VerifiedOfficial | CheckStatus::VerifiedAnnouncement
        ) && self.basis == CheckBasis::None
        {
            return Err(CheckError::invalid(
                "a verified record needs a basis (official_qss|announcement)",
            ));
        }
        if self.basis != CheckBasis::None {
            if self.basis_ref.trim().is_empty() {
                return Err(CheckError::invalid("basis_ref is required with a basis"));
            }
            if self.evidence_url.trim().is_empty() {
                return Err(CheckError::invalid("evidence_url is required with a basis"));
            }
            validate_sha(&self.evidence_sha256)?;
        } else if !self.evidence_sha256.trim().is_empty() {
            return Err(CheckError::invalid(
                "basis none must leave evidence_sha256 empty",
            ));
        }
        match (self.check_status, self.basis) {
            (CheckStatus::VerifiedOfficial, CheckBasis::OfficialQss)
            | (CheckStatus::VerifiedAnnouncement, CheckBasis::Announcement)
            | (CheckStatus::Unverified, _)
            | (CheckStatus::NoEffect, _) => {}
            (status, basis) => {
                return Err(CheckError::invalid(format!(
                    "{status} cannot use basis {basis}"
                )));
            }
        }
        if !self.gbbq_value.trim().is_empty() && !is_decimal(&self.gbbq_value) {
            return Err(CheckError::invalid("gbbq_value is not a decimal"));
        }
        if !self.announced_value.trim().is_empty() && !is_decimal(&self.announced_value) {
            return Err(CheckError::invalid("announced_value is not a decimal"));
        }
        if !self.price_delta.trim().is_empty() && !is_decimal(&self.price_delta) {
            return Err(CheckError::invalid("price_delta is not a decimal"));
        }
        if self.within_one_tick.is_some() && self.price_delta.trim().is_empty() {
            return Err(CheckError::invalid(
                "within_one_tick needs a price_delta to describe",
            ));
        }
        validate_sha(&self.evidence_digest)?;
        Ok(())
    }
}

/// Why a record line was rejected. Reported per row by the importer; row
/// rejections never abort the batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckError {
    pub reason: &'static str,
    pub detail: String,
}

impl CheckError {
    pub fn invalid(detail: impl Into<String>) -> Self {
        Self {
            reason: "invalid_record",
            detail: detail.into(),
        }
    }
}

impl fmt::Display for CheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.reason, self.detail)
    }
}

impl From<StoreError> for CheckError {
    fn from(error: StoreError) -> Self {
        Self {
            reason: "source_store",
            detail: error.to_string(),
        }
    }
}

/// The whole check set read out of the store, plus the summary a reader needs
/// to prove what it judged: the `etf_checks_digest` and the record count.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CheckSet {
    /// Records sorted by `(entity, ordinal, evidence_digest, checked_at, sha)`.
    pub records: Vec<EtfFactorCheck>,
    /// sha256 of each stored record blob, in the same order as `records`.
    pub shas: Vec<String>,
}

impl CheckSet {
    /// `etf_checks_digest`: sha256 over the sorted record ids (the set of
    /// records that took part in a decision), so a response can name the exact
    /// evidence behind its verdict (design §3.2.2 "复现").
    pub fn digest(&self) -> String {
        let mut hasher = Vec::new();
        hasher.extend_from_slice(b"etf-checks-v1\0");
        let mut ids: Vec<&str> = self.shas.iter().map(String::as_str).collect();
        ids.sort_unstable();
        for id in ids {
            hasher.extend_from_slice(id.as_bytes());
            hasher.push(0);
        }
        crate::hash::sha256(&hasher)
    }

    /// Every record of `entity`, in store order (see [`Self::records`]).
    pub fn for_entity<'a>(&'a self, entity: &'a str) -> impl Iterator<Item = &'a EtfFactorCheck> {
        self.records
            .iter()
            .filter(move |record| record.entity == entity)
    }

    /// Newest record per check key: later `checked_at` wins, and on an equal
    /// day the bigger sha wins (a deterministic, arbitrary tie-break so two
    /// imports of the same day are reproducible).
    pub fn latest_by_key(&self) -> BTreeMap<(String, i64, String), &EtfFactorCheck> {
        let mut out: BTreeMap<(String, i64, String), (&EtfFactorCheck, &String)> = BTreeMap::new();
        for (record, sha) in self.records.iter().zip(&self.shas) {
            let key = record.key();
            match out.get(&key) {
                Some((existing, existing_sha))
                    if (&existing.checked_at, *existing_sha) >= (&record.checked_at, sha) => {}
                _ => {
                    out.insert(key, (record, sha));
                }
            }
        }
        out.into_iter()
            .map(|(key, (record, _))| (key, record))
            .collect()
    }
}

/// Read and verify every frozen check record in `store`. Blob digests are
/// verified by [`SourceStore::read_verified`]; a record whose bytes do not hash
/// to its manifest sha is `source_store_corrupt`.
pub fn read(store: &SourceStore) -> Result<CheckSet, StoreError> {
    let mut seen = BTreeSet::new();
    let mut rows: Vec<(EtfFactorCheck, String)> = Vec::new();
    for record in store.records()? {
        if record.source_id != CHECKS_SOURCE_ID {
            continue;
        }
        if !seen.insert(record.sha256.clone()) {
            continue;
        }
        let bytes = store.read_verified(&record.sha256)?;
        let text = std::str::from_utf8(&bytes).map_err(|error| {
            StoreError::Manifest(format!(
                "check record {} is not UTF-8: {error}",
                record.sha256
            ))
        })?;
        let parsed = EtfFactorCheck::parse_line(text.trim_end_matches('\n')).map_err(|error| {
            StoreError::Manifest(format!("check record {}: {error}", record.sha256))
        })?;
        let digest = parsed.digest().map_err(|error| {
            StoreError::Manifest(format!("check record {}: {error}", record.sha256))
        })?;
        if digest != record.sha256 {
            return Err(StoreError::Manifest(format!(
                "check record {} does not hash its own content",
                record.sha256
            )));
        }
        rows.push((parsed, record.sha256));
    }
    rows.sort_by(|a, b| {
        (
            &a.0.entity,
            a.0.ordinal,
            &a.0.evidence_digest,
            &a.0.checked_at,
            &a.1,
        )
            .cmp(&(
                &b.0.entity,
                b.0.ordinal,
                &b.0.evidence_digest,
                &b.0.checked_at,
                &b.1,
            ))
    });
    let (records, shas) = rows.into_iter().unzip();
    Ok(CheckSet { records, shas })
}

/// `replay --scope etf_factor_checks` / `--verify-only` body: the selection is
/// read-only evidence, so the scope computes one deterministic summary from the
/// blobs and never writes the market database (design §3.2.2).
pub struct ReplayOutcome {
    pub status: &'static str,
    pub rebuilt: &'static str,
    pub summary: Option<String>,
    pub database: Option<&'static str>,
    pub records: Option<usize>,
    pub etf_checks_digest: Option<String>,
    pub problems: Vec<super::replay::BlobProblem>,
}

/// Canonical summary line of one record for the scope digest.
fn summary_line(record: &EtfFactorCheck, sha: &str) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        record.entity,
        record.ordinal,
        record.check_status,
        record.basis,
        record.checked_at,
        record.evidence_digest,
        sha
    )
}

/// Rebuild the scope's summary from the store. `records` is the manifest
/// selection `Scope::etf_factor_checks` owns; a bad record is reported as a
/// problem and fails the scope (`record_invalid`) instead of being skipped.
pub fn replay_scope(
    store: &SourceStore,
    records: &[&super::ManifestRecord],
) -> Result<ReplayOutcome, String> {
    let empty = records.is_empty();
    let set = match read(store) {
        Ok(set) => set,
        Err(StoreError::Manifest(detail)) => {
            let _ = &records;
            return Ok(ReplayOutcome {
                status: "record_invalid",
                rebuilt: super::replay::NOTHING_SELECTED,
                summary: None,
                database: None,
                records: None,
                etf_checks_digest: None,
                problems: vec![super::replay::BlobProblem {
                    sha256: String::new(),
                    source_id: CHECKS_SOURCE_ID.to_string(),
                    reason: "record_invalid",
                    expected: format!("a {CHECKS_VERSION} record"),
                    actual: detail,
                }],
            });
        }
        Err(error) => return Err(error.to_string()),
    };
    if empty {
        return Ok(ReplayOutcome {
            status: "ok",
            rebuilt: super::replay::NOTHING_SELECTED,
            summary: None,
            database: None,
            records: Some(0),
            etf_checks_digest: None,
            problems: Vec::new(),
        });
    }
    let mut payload = String::from("etf-factor-checks-v1\0");
    for (record, sha) in set.records.iter().zip(&set.shas) {
        payload.push_str(&summary_line(record, sha));
        payload.push('\n');
    }
    Ok(ReplayOutcome {
        status: "ok",
        rebuilt: super::replay::COMPUTED_ETF_FACTOR_CHECKS,
        summary: Some(crate::hash::sha256(payload.as_bytes())),
        database: Some("not_compared"),
        records: Some(set.records.len()),
        etf_checks_digest: Some(set.digest()),
        problems: Vec::new(),
    })
}

/// One parsed input row of the importer (CSV row or JSONL line).
fn record_from_map(
    fields: &BTreeMap<String, String>,
    line: usize,
) -> Result<EtfFactorCheck, CheckError> {
    let field = |name: &str| -> String {
        fields
            .get(name)
            .map(|value| value.trim().to_string())
            .unwrap_or_default()
    };
    let required = |name: &str| -> Result<String, CheckError> {
        let value = field(name);
        if value.is_empty() {
            return Err(CheckError::invalid(format!(
                "line {line}: {name} is required"
            )));
        }
        Ok(value)
    };
    let check_status = CheckStatus::parse(&required("check_status")?).ok_or_else(|| {
        CheckError::invalid(format!(
            "line {line}: check_status {:?} is not \
             verified_official|verified_announcement|no_effect|unverified",
            field("check_status")
        ))
    })?;
    let basis_text = fields
        .get("basis")
        .cloned()
        .unwrap_or_else(|| "none".into());
    let basis = CheckBasis::parse(basis_text.trim()).ok_or_else(|| {
        CheckError::invalid(format!(
            "line {line}: basis {basis_text:?} is not official_qss|announcement|none"
        ))
    })?;
    let ordinal: i64 = required("ordinal")?
        .parse()
        .map_err(|_| CheckError::invalid(format!("line {line}: ordinal is not an integer")))?;
    let category: u8 = required("category")?
        .parse()
        .map_err(|_| CheckError::invalid(format!("line {line}: category is not an integer")))?;
    Ok(EtfFactorCheck {
        record_version: fields
            .get("record_version")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| CHECKS_VERSION.to_string()),
        entity: required("entity")?,
        ordinal,
        effective_day: required("effective_day")?,
        category,
        check_status,
        basis,
        basis_ref: field("basis_ref"),
        evidence_url: field("evidence_url"),
        evidence_sha256: field("evidence_sha256"),
        announced_value: field("announced_value"),
        gbbq_value: field("gbbq_value"),
        price_delta: field("price_delta"),
        within_one_tick: match field("within_one_tick").as_str() {
            "" => None,
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            other => {
                return Err(CheckError::invalid(format!(
                    "line {line}: within_one_tick {other:?} is not a boolean"
                )));
            }
        },
        checked_at: required("checked_at")?,
        evidence_digest: required("evidence_digest")?,
    })
}

fn csv_rows(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if quoted {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(ch);
            }
            continue;
        }
        match ch {
            '"' if field.is_empty() => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            '\r' => {}
            _ => field.push(ch),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

/// Import one `etf-factor-checks` file (CSV with a header row, or JSONL) into
/// `store`.
///
/// - **Idempotent**: a record already registered under its own sha is
///   `already_present` and writes no blob and no line.
/// - **`--dry-run`**: every check runs, nothing is written and the store is
///   only read (the shared lock `SourceStore::registered_keys` takes).
/// - **fail-closed per row**: a row with a missing field, an unregistered
///   status, a malformed sha256 or a `check_status`/`basis` mismatch is
///   rejected and counted in `skipped_unverified`; a rejected row registers
///   nothing, so it cannot become evidence by accident.
pub fn import(
    store: &SourceStore,
    path: &Path,
    options: &ImportOptions,
) -> Result<ImportReport, StoreError> {
    let bytes = std::fs::read(path)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| StoreError::Manifest(format!("{}: not UTF-8: {error}", path.display())))?;
    let is_jsonl = options.format == ImportFormat::EtfFactorChecksJsonl;
    let batch = options
        .batch
        .clone()
        .unwrap_or_else(|| format!("etf-factor-checks-{}", crate::hash::sha256(&bytes)));
    let mut report = ImportReport {
        status: "ok",
        store: store.root().display().to_string(),
        index: path.display().to_string(),
        blobs: String::new(),
        batches: vec![batch.clone()],
        tiers: options.tiers.clone(),
        kind: CHECKS_SOURCE_ID.to_string(),
        dry_run: options.dry_run,
        index_lines: 0,
        imported: Vec::new(),
        already_present: Vec::new(),
        skipped_unverified: Vec::new(),
        blobs_added: 0,
        manifest_lines_added: 0,
    };
    let mut existing = store.registered_keys()?;
    let window = if is_jsonl {
        jsonl_rows(text)
    } else {
        csv_field_maps(text)
    };
    let rejected = window.rejected;
    report.index_lines = rejected.len() + window.rows.len();
    for (line, error) in rejected {
        report.skipped_unverified.push(SkippedMember {
            index_line: line,
            source_path: path.display().to_string(),
            member: "etf_factor_check",
            reason: error.reason,
            detail: error.detail,
        });
    }
    for (line, fields) in window.rows {
        let record = match record_from_map(&fields, line) {
            Ok(record) => record,
            Err(error) => {
                report.skipped_unverified.push(SkippedMember {
                    index_line: line,
                    source_path: path.display().to_string(),
                    member: "etf_factor_check",
                    reason: error.reason,
                    detail: error.detail,
                });
                continue;
            }
        };
        if let Err(error) = record.validate() {
            report.skipped_unverified.push(SkippedMember {
                index_line: line,
                source_path: path.display().to_string(),
                member: "etf_factor_check",
                reason: error.reason,
                detail: format!("line {line}: {}", error.detail),
            });
            continue;
        }
        let canonical = match record.canonical_json() {
            Ok(text) => text,
            Err(error) => {
                report.skipped_unverified.push(SkippedMember {
                    index_line: line,
                    source_path: path.display().to_string(),
                    member: "etf_factor_check",
                    reason: error.reason,
                    detail: error.detail,
                });
                continue;
            }
        };
        let sha = crate::hash::sha256(canonical.as_bytes());
        let origin = format!("{ORIGIN_PREFIX}{sha}");
        let member = ImportedMember {
            member: "etf_factor_check",
            source_id: CHECKS_SOURCE_ID,
            sha256: sha.clone(),
            origin: origin.clone(),
        };
        let key = (sha.clone(), CHECKS_SOURCE_ID.to_string(), origin.clone());
        if existing.contains(&key) {
            report.already_present.push(member);
            continue;
        }
        let registration = Registration {
            source_id: CHECKS_SOURCE_ID.to_string(),
            store_kind: StoreKind::ArchiveImport,
            origin,
            acquired_at: format!("{}T00:00:00Z", record.checked_at),
            publisher_last_modified: None,
            evidence_grade: None,
            basis_status: Some(record.check_status.as_str().to_string()),
            parser_version: Some(CHECKS_VERSION.to_string()),
            container_sha256: None,
            import_batch: Some(batch.clone()),
            imported_at: Some(now_utc()),
            http_etag: None,
        };
        if options.dry_run {
            if !store.blob_exists(&sha) {
                report.blobs_added += 1;
            }
            report.manifest_lines_added += 1;
            report.imported.push(member);
            continue;
        }
        let outcome = store.register(canonical.as_bytes(), &registration)?;
        existing.insert(key);
        if outcome.appended_line {
            report.manifest_lines_added += 1;
            if outcome.written_blob {
                report.blobs_added += 1;
            }
            report.imported.push(member);
        } else {
            report.already_present.push(member);
        }
    }
    Ok(report)
}

struct ParsedRows {
    rows: Vec<(usize, BTreeMap<String, String>)>,
    rejected: Vec<(usize, CheckError)>,
}

/// CSV input: a header row names the columns, unknown columns are ignored and
/// an absent column reads as empty (so `validate` reports it by name).
fn csv_field_maps(text: &str) -> ParsedRows {
    let mut rows = Vec::new();
    let mut rejected = Vec::new();
    let mut lines = csv_rows(text).into_iter();
    let Some(header) = lines.next() else {
        rejected.push((1, CheckError::invalid("the CSV is empty")));
        return ParsedRows { rows, rejected };
    };
    let names: Vec<String> = header.iter().map(|name| name.trim().to_string()).collect();
    for (offset, row) in lines.enumerate() {
        let line = offset + 2;
        if row.iter().all(|value| value.trim().is_empty()) {
            continue;
        }
        if row.len() != names.len() {
            rejected.push((
                line,
                CheckError::invalid(format!(
                    "line {line} has {} column(s), the header has {}",
                    row.len(),
                    names.len()
                )),
            ));
            continue;
        }
        let map: BTreeMap<String, String> = names
            .iter()
            .cloned()
            .zip(row.iter().cloned())
            .filter(|(name, _)| !name.is_empty())
            .collect();
        rows.push((line, map));
    }
    ParsedRows { rows, rejected }
}

/// JSONL input: one object per line, blank lines and `#` comments skipped.
fn jsonl_rows(text: &str) -> ParsedRows {
    let mut rows = Vec::new();
    let mut rejected = Vec::new();
    for (offset, line) in text.lines().enumerate() {
        let number = offset + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(serde_json::Value::Object(object)) => {
                let map: BTreeMap<String, String> = object
                    .into_iter()
                    .map(|(key, value)| {
                        let text = match value {
                            serde_json::Value::String(text) => text,
                            serde_json::Value::Null => String::new(),
                            other => other.to_string(),
                        };
                        (key, text)
                    })
                    .collect();
                rows.push((number, map));
            }
            Ok(_) => rejected.push((number, CheckError::invalid("line is not a JSON object"))),
            Err(error) => rejected.push((
                number,
                CheckError::invalid(format!("line is not JSON: {error}")),
            )),
        }
    }
    ParsedRows { rows, rejected }
}

fn validate_sha(value: &str) -> Result<(), CheckError> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(());
    }
    Err(CheckError::invalid(format!(
        "{value:?} is not a sha256 hex digest"
    )))
}

fn is_entity(value: &str) -> bool {
    let Some((exchange, code)) = value.split_once('.') else {
        return false;
    };
    matches!(exchange, "sh" | "sz" | "bj")
        && code.len() == 6
        && code.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_day(value: &str) -> bool {
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
}

fn is_decimal(value: &str) -> bool {
    value.parse::<rust_decimal::Decimal>().is_ok()
}

fn now_utc() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The scope of the same name, so the CLI and replay never spell it twice.
pub const SCOPE: Scope = Scope::EtfFactorChecks;

#[cfg(test)]
mod tests;
