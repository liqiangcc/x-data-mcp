use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive as _;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::market_data::{
    Adjustment, AdjustmentEvidence, Bar, CoverageStatus, EvidenceStatus, Period,
    UnitCurrencyEvidence,
};

pub const MAX_SEGMENT_BARS: usize = 512;
pub type Digest = [u8; 32];

/// `market_series.price_scale` ceiling (schema 22 DDL, issue-383 §3.2): an
/// integer price is `value / 10^price_scale` in 元 (点 for indexes).
pub const MAX_PRICE_SCALE: u32 = 6;
/// A-share raw prices in 分: `price_scale=2` (`.day` 原值, issue-383 §4.1).
pub const PRICE_SCALE_CENTS: u32 = 2;
/// Forward-adjusted series precision: `price_scale=6` (1e-6 元), written by
/// `qfq-exact-v1` (issue-383 §4.2/§4.3). The forward chain is composed over
/// exact rationals and rounded once, so the stored integer *is* the rounded
/// value — there is no further rounding a reader could apply.
pub const PRICE_SCALE_QFQ: u32 = 6;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("compact market data invalid: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactBar {
    pub trade_day: i64,
    pub open_units: i64,
    pub high_units: i64,
    pub low_units: i64,
    pub close_units: i64,
    pub volume: Option<i64>,
    pub amount: Option<String>,
    pub r7_raw: Option<u32>,
}

impl CompactBar {
    /// Encode a domain bar at the series' `price_scale` (issue-383 §3.3):
    /// the integers carry no unit, the owning `market_series` row does.
    pub fn from_bar(bar: &Bar, period: Period, price_scale: u32) -> Result<Self, Error> {
        if period == Period::Year && bar.r7_raw.is_some() {
            return Err(invalid("yearly bar must not carry r7_raw"));
        }
        let compact = Self {
            trade_day: epoch_day(bar.date),
            open_units: units(bar.open, price_scale)?,
            high_units: units(bar.high, price_scale)?,
            low_units: units(bar.low, price_scale)?,
            close_units: units(bar.close, price_scale)?,
            volume: bar.volume.map(volume).transpose()?,
            amount: bar.amount.map(amount).transpose()?,
            r7_raw: bar.r7_raw,
        };
        compact.check(period)?;
        Ok(compact)
    }

    /// Decode with the scale stored on the owning series row — a reader that
    /// assumes 2dp would misprice every 6dp series (issue-383 §3.10).
    ///
    /// The returned values are exact at the row's scale but not padded to it:
    /// trailing zeros carry no information, and the project rule is not to fix
    /// an output width (`docs/sqlite-storage-design.md`: 不擅自固定两位或六位
    /// 小数). A 6dp series therefore renders `11.75` for an exact cent value
    /// and `7.526667` where the chain actually has six decimals — "最多 6 位"
    /// (issue-383 §8.2), not "总是 6 位".
    pub fn to_bar(&self, period: Period, price_scale: u32) -> Result<Bar, Error> {
        let (date, amount) = self.decode_checked(period)?;
        let scale = checked_scale(price_scale)?;
        Ok(Bar {
            date,
            open: Decimal::new(self.open_units, scale).normalize(),
            high: Decimal::new(self.high_units, scale).normalize(),
            low: Decimal::new(self.low_units, scale).normalize(),
            close: Decimal::new(self.close_units, scale).normalize(),
            volume: self.volume.map(Decimal::from),
            amount,
            r7_raw: self.r7_raw,
        })
    }

    /// Single-pass checked decode (XP-018): the exact `check` validations
    /// in their current error order — epoch date, OHLC ordering, volume
    /// sign, amount parse/sign/canonical lexical form, yearly-r7
    /// rejection — returning the already-checked date and parsed amount
    /// so callers consume them instead of re-parsing.
    fn decode_checked(&self, period: Period) -> Result<(NaiveDate, Option<Decimal>), Error> {
        let date = epoch_date(self.trade_day)?;
        if self.high_units < self.open_units
            || self.high_units < self.close_units
            || self.high_units < self.low_units
            || self.low_units > self.open_units
            || self.low_units > self.close_units
        {
            return Err(invalid("OHLC ordering violated"));
        }
        if let Some(volume) = self.volume
            && volume < 0
        {
            return Err(invalid("negative volume"));
        }
        let amount = match &self.amount {
            Some(amount) => {
                let parsed = amount
                    .parse::<Decimal>()
                    .map_err(|_| invalid("stored amount is not a decimal"))?;
                if parsed < Decimal::ZERO {
                    return Err(invalid("negative amount"));
                }
                if !canonical_decimal(amount) {
                    return Err(invalid("stored amount is not canonical"));
                }
                Some(parsed)
            }
            None => None,
        };
        if period == Period::Year && self.r7_raw.is_some() {
            return Err(invalid("yearly bar must not carry r7_raw"));
        }
        Ok((date, amount))
    }

    fn check(&self, period: Period) -> Result<(), Error> {
        self.decode_checked(period).map(|_| ())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentClip {
    pub segment_digest: Digest,
    pub clip_start_day: i64,
    pub clip_end_day: i64,
    pub row_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeltaKind {
    Upsert(SegmentClip),
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeltaOp {
    pub range_start_day: i64,
    pub range_end_day: i64,
    pub kind: DeltaKind,
}

pub fn segment_digest(entity: &str, period: Period, bars: &[CompactBar]) -> Result<Digest, Error> {
    canonical_entity(entity)?;
    if bars.is_empty() || bars.len() > MAX_SEGMENT_BARS {
        return Err(invalid("segment row_count out of bounds"));
    }
    for pair in bars.windows(2) {
        if pair[1].trade_day <= pair[0].trade_day {
            return Err(invalid("segment bars not strictly increasing"));
        }
    }
    let mut payload = Vec::new();
    payload.extend_from_slice(b"x-data-segment-v1\0");
    push_text(&mut payload, entity);
    payload.push(period_code(period));
    payload.extend_from_slice(&(bars.len() as u32).to_be_bytes());
    for bar in bars {
        bar.check(period)?;
        bar_encoding(&mut payload, bar);
    }
    Ok(Sha256::digest(&payload).into())
}

pub fn checkpoint_projection_digest(
    entity: &str,
    period: Period,
    clips: &[SegmentClip],
) -> Result<Digest, Error> {
    canonical_entity(entity)?;
    if clips.is_empty() {
        return Err(invalid("checkpoint requires at least one clip"));
    }
    let mut previous_end: Option<i64> = None;
    for clip in clips {
        if clip.clip_start_day > clip.clip_end_day {
            return Err(invalid("clip bounds inverted"));
        }
        if clip.row_count == 0 {
            return Err(invalid("clip row_count must be positive"));
        }
        if let Some(end) = previous_end
            && clip.clip_start_day <= end
        {
            return Err(invalid("checkpoint clips overlap or are unordered"));
        }
        previous_end = Some(clip.clip_end_day);
    }
    let mut payload = Vec::new();
    payload.extend_from_slice(b"x-data-projection-checkpoint-v1\0");
    push_text(&mut payload, entity);
    payload.push(period_code(period));
    payload.extend_from_slice(&(clips.len() as u32).to_be_bytes());
    for clip in clips {
        payload.extend_from_slice(&clip.segment_digest);
        payload.extend_from_slice(&clip.clip_start_day.to_be_bytes());
        payload.extend_from_slice(&clip.clip_end_day.to_be_bytes());
        payload.extend_from_slice(&clip.row_count.to_be_bytes());
    }
    Ok(Sha256::digest(&payload).into())
}

pub fn delta_digest(ops: &[DeltaOp]) -> Result<Digest, Error> {
    check_delta_ops(ops)?;
    let mut payload = Vec::new();
    payload.extend_from_slice(b"x-data-version-delta-v1\0");
    payload.extend_from_slice(&(ops.len() as u32).to_be_bytes());
    for op in ops {
        delta_op_encoding(&mut payload, op);
    }
    Ok(Sha256::digest(&payload).into())
}

pub fn delta_projection_digest(parent: Digest, ops: &[DeltaOp]) -> Result<(Digest, Digest), Error> {
    let delta = delta_digest(ops)?;
    let mut payload = Vec::new();
    payload.extend_from_slice(b"x-data-projection-delta-v1\0");
    payload.extend_from_slice(&parent);
    payload.extend_from_slice(&delta);
    Ok((delta, Sha256::digest(&payload).into()))
}

pub fn digest_hex(digest: &Digest) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceKind {
    Generation,
    BatchSelection,
    Version,
    Continuity,
    Catalog,
    Yearly,
    /// Derived-data evidence: trading calendar input list, factor-set
    /// provenance, index unit/scope evidence (issue-383 §3.9, §6.3). Carries
    /// the "how it was proven" note, never bar arrays.
    Derived,
    /// Storage-internal sub-document: a generation-common field of a larger
    /// evidence document, stored once and referenced by `$evref` from the
    /// parent's stored form (#259 P0-D). Never produced over the wire; a
    /// shared row is only reachable through a parent bundle's marker.
    Shared,
}

impl EvidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Generation => "generation",
            Self::BatchSelection => "batch_selection",
            Self::Version => "version",
            Self::Continuity => "continuity",
            Self::Catalog => "catalog",
            Self::Yearly => "yearly",
            Self::Derived => "derived",
            Self::Shared => "shared",
        }
    }

    fn byte_limit(self) -> usize {
        match self {
            Self::Continuity => 4_096,
            Self::Version | Self::Yearly => 16_384,
            Self::Derived | Self::Shared => 65_536,
            Self::Generation | Self::Catalog => 262_144,
            Self::BatchSelection => 4_194_304,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceBundle {
    pub kind: EvidenceKind,
    pub canonical_json: String,
    pub digest: Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionEvidence {
    pub source: String,
    pub coverage_reason: String,
    pub unit_currency: UnitCurrencyEvidence,
    pub adjustment_evidence: AdjustmentEvidence,
    /// Index-only: the bars dropped because their own OHLC ordering was
    /// impossible (#383 owner ruling 2026-10-27). `None` for every stock
    /// series and for an index whose source had no such bar, so adding this
    /// field does not change an existing evidence digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded_bars: Option<Vec<ExcludedBar>>,
}

/// One index bar excluded from the stored series, recorded verbatim (the
/// decimal values the adapter handed over) so the decision is reproducible
/// from the evidence alone.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExcludedBar {
    pub date: NaiveDate,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBinding {
    pub generation_id: String,
    pub generation_manifest_sha256: Digest,
    pub day_member_sha256: Digest,
    pub security_event_sha256: Digest,
    pub evidence_digest: Digest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestMode {
    Checkpoint,
    Delta,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YearlyLineageIdentity {
    pub base_daily_version_id: String,
    pub algorithm_version: String,
    pub first_trade_day: i64,
    pub last_trade_day: i64,
    pub as_of_day: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionIdentity {
    pub version_id: String,
    pub entity: String,
    pub period: Period,
    pub source_binding_digest: Digest,
    pub parent_version_id: Option<String>,
    pub manifest_mode: ManifestMode,
    pub delta_depth: u32,
    pub projection_digest: Digest,
    pub record_count: u64,
    pub first_day: i64,
    pub last_day: i64,
    pub coverage_status: CoverageStatus,
    pub evidence_digest: Digest,
    pub published_at: String,
    pub yearly_lineage: Option<YearlyLineageIdentity>,
}

pub fn canonical_evidence(kind: EvidenceKind, value: &Value) -> Result<EvidenceBundle, Error> {
    check_evidence_semantics(kind, value)?;
    let canonical_json = serde_json_canonicalizer::to_string(value)
        .map_err(|_| invalid("evidence is not canonicalizable JSON"))?;
    check_evidence_size(kind, &canonical_json)?;
    let digest = evidence_digest(kind, &canonical_json);
    Ok(EvidenceBundle {
        kind,
        canonical_json,
        digest,
    })
}

pub fn verify_evidence(
    kind: EvidenceKind,
    canonical_json: &str,
    expected: Digest,
) -> Result<Value, Error> {
    let value: Value =
        serde_json::from_str(canonical_json).map_err(|_| invalid("evidence is not JSON"))?;
    let recanonicalized = serde_json_canonicalizer::to_string(&value)
        .map_err(|_| invalid("evidence is not canonicalizable JSON"))?;
    if recanonicalized != canonical_json {
        return Err(invalid("stored evidence is not canonical JCS"));
    }
    if evidence_digest(kind, canonical_json) != expected {
        return Err(invalid("evidence digest mismatch"));
    }
    check_evidence_semantics(kind, &value)?;
    check_evidence_size(kind, canonical_json)?;
    Ok(value)
}

pub fn source_binding_digest(binding: &SourceBinding) -> Result<Digest, Error> {
    if binding.generation_id.is_empty() {
        return Err(invalid("source binding generation_id is empty"));
    }
    let mut payload = Vec::new();
    payload.extend_from_slice(b"x-data-source-binding-v1\0");
    push_text(&mut payload, &binding.generation_id);
    payload.extend_from_slice(&binding.generation_manifest_sha256);
    payload.extend_from_slice(&binding.day_member_sha256);
    payload.extend_from_slice(&binding.security_event_sha256);
    payload.extend_from_slice(&binding.evidence_digest);
    Ok(Sha256::digest(&payload).into())
}

pub fn version_integrity_digest(identity: &VersionIdentity) -> Result<Digest, Error> {
    if identity.version_id.is_empty() {
        return Err(invalid("version_id is empty"));
    }
    canonical_entity(&identity.entity)?;
    if identity.record_count == 0 {
        return Err(invalid("record_count must be positive"));
    }
    if identity.first_day > identity.last_day {
        return Err(invalid("version day bounds inverted"));
    }
    epoch_date(identity.first_day)?;
    epoch_date(identity.last_day)?;
    let published = DateTime::parse_from_rfc3339(&identity.published_at)
        .map_err(|_| invalid("published_at is not RFC 3339"))?;
    if published.offset().local_minus_utc() != 0
        || published.with_timezone(&Utc).to_rfc3339() != identity.published_at
    {
        return Err(invalid("published_at is not canonical UTC RFC 3339"));
    }
    match identity.manifest_mode {
        ManifestMode::Checkpoint => {
            if identity.parent_version_id.is_some() || identity.delta_depth != 0 {
                return Err(invalid("checkpoint version carries parent/depth"));
            }
        }
        ManifestMode::Delta => {
            if identity
                .parent_version_id
                .as_deref()
                .is_none_or(str::is_empty)
                || !(1..=256).contains(&identity.delta_depth)
            {
                return Err(invalid("delta version lacks parent or depth 1..=256"));
            }
        }
    }
    match (&identity.period, &identity.yearly_lineage) {
        (Period::Day, None) => {}
        (Period::Year, Some(lineage)) => {
            if lineage.base_daily_version_id.is_empty() || lineage.algorithm_version.is_empty() {
                return Err(invalid("yearly lineage identity is empty"));
            }
            if lineage.first_trade_day > lineage.last_trade_day
                || lineage.last_trade_day > lineage.as_of_day
            {
                return Err(invalid("yearly lineage bounds inverted"));
            }
            epoch_date(lineage.first_trade_day)?;
            epoch_date(lineage.last_trade_day)?;
            epoch_date(lineage.as_of_day)?;
        }
        _ => return Err(invalid("period/lineage mismatch")),
    }
    let mut payload = Vec::new();
    payload.extend_from_slice(b"x-data-market-version-v1\0");
    push_text(&mut payload, &identity.version_id);
    push_text(&mut payload, &identity.entity);
    payload.push(period_code(identity.period));
    payload.push(1);
    payload.extend_from_slice(&identity.source_binding_digest);
    match &identity.parent_version_id {
        Some(parent) => {
            payload.push(1);
            push_text(&mut payload, parent);
        }
        None => payload.push(0),
    }
    payload.push(match identity.manifest_mode {
        ManifestMode::Checkpoint => 1,
        ManifestMode::Delta => 2,
    });
    payload.extend_from_slice(&identity.delta_depth.to_be_bytes());
    payload.extend_from_slice(&identity.projection_digest);
    payload.extend_from_slice(&identity.record_count.to_be_bytes());
    payload.extend_from_slice(&identity.first_day.to_be_bytes());
    payload.extend_from_slice(&identity.last_day.to_be_bytes());
    payload.push(match identity.coverage_status {
        CoverageStatus::Complete => 1,
        CoverageStatus::Incomplete => 2,
        CoverageStatus::Unknown => 3,
    });
    payload.extend_from_slice(&identity.evidence_digest);
    push_text(&mut payload, &identity.published_at);
    match &identity.yearly_lineage {
        Some(lineage) => {
            payload.push(1);
            push_text(&mut payload, &lineage.base_daily_version_id);
            push_text(&mut payload, &lineage.algorithm_version);
            payload.extend_from_slice(&lineage.first_trade_day.to_be_bytes());
            payload.extend_from_slice(&lineage.last_trade_day.to_be_bytes());
            payload.extend_from_slice(&lineage.as_of_day.to_be_bytes());
        }
        None => payload.push(0),
    }
    Ok(Sha256::digest(&payload).into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateOutcome {
    NoChange,
    Published,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuityMode {
    Unchanged,
    Append,
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketUpdateResultIdentity {
    pub result_id: String,
    pub task_id: String,
    pub item_id: String,
    pub base_version_id: String,
    pub result_version_id: String,
    pub outcome: UpdateOutcome,
    pub range_start_day: i64,
    pub range_end_day: i64,
    pub algorithm_version: String,
    pub source_binding_digest: Digest,
    pub continuity_mode: ContinuityMode,
    pub prefix_bytes: Option<u64>,
    pub base_projection_digest: Digest,
    pub result_projection_digest: Digest,
    pub delta_digest: Option<Digest>,
    pub evidence_digest: Digest,
    pub task_revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationKind {
    Collection,
    Yearly,
    Republish,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationResultIdentity {
    pub publication_id: String,
    pub task_id: String,
    pub item_id: String,
    pub publication_kind: PublicationKind,
    pub base_version_id: Option<String>,
    pub result_version_id: String,
    pub outcome: UpdateOutcome,
    pub algorithm_version: String,
    pub source_binding_digest: Digest,
    pub evidence_digest: Digest,
    pub task_revision: u64,
}

pub fn market_update_result_digest(identity: &MarketUpdateResultIdentity) -> Result<Digest, Error> {
    for id in [
        &identity.result_id,
        &identity.task_id,
        &identity.item_id,
        &identity.base_version_id,
        &identity.result_version_id,
        &identity.algorithm_version,
    ] {
        if id.is_empty() {
            return Err(invalid("update result identity field is empty"));
        }
    }
    if identity.task_revision == 0 {
        return Err(invalid("task_revision must be positive"));
    }
    epoch_date(identity.range_start_day)?;
    epoch_date(identity.range_end_day)?;
    if identity.range_start_day > identity.range_end_day {
        return Err(invalid("update result range inverted"));
    }
    match identity.outcome {
        UpdateOutcome::NoChange => {
            if identity.continuity_mode != ContinuityMode::Unchanged
                || identity.base_version_id != identity.result_version_id
                || identity.prefix_bytes.is_none()
                || identity.base_projection_digest != identity.result_projection_digest
                || identity.delta_digest.is_some()
            {
                return Err(invalid("no-change update result semantics violated"));
            }
        }
        UpdateOutcome::Published => {
            if identity.base_version_id == identity.result_version_id
                || identity.delta_digest.is_none()
                || identity.continuity_mode == ContinuityMode::Unchanged
            {
                return Err(invalid("published update result semantics violated"));
            }
            match identity.continuity_mode {
                ContinuityMode::Append if identity.prefix_bytes.is_none() => {
                    return Err(invalid("append update result lacks prefix_bytes"));
                }
                ContinuityMode::Full if identity.prefix_bytes.is_some() => {
                    return Err(invalid("full update result carries prefix_bytes"));
                }
                _ => {}
            }
        }
    }
    let mut payload = Vec::new();
    payload.extend_from_slice(b"x-data-market-update-result-v1\0");
    push_text(&mut payload, &identity.result_id);
    push_text(&mut payload, &identity.task_id);
    push_text(&mut payload, &identity.item_id);
    push_text(&mut payload, &identity.base_version_id);
    push_text(&mut payload, &identity.result_version_id);
    payload.push(match identity.outcome {
        UpdateOutcome::NoChange => 1,
        UpdateOutcome::Published => 2,
    });
    payload.extend_from_slice(&identity.range_start_day.to_be_bytes());
    payload.extend_from_slice(&identity.range_end_day.to_be_bytes());
    push_text(&mut payload, &identity.algorithm_version);
    payload.extend_from_slice(&identity.source_binding_digest);
    payload.push(match identity.continuity_mode {
        ContinuityMode::Unchanged => 1,
        ContinuityMode::Append => 2,
        ContinuityMode::Full => 3,
    });
    match identity.prefix_bytes {
        Some(prefix) => {
            payload.push(1);
            payload.extend_from_slice(&prefix.to_be_bytes());
        }
        None => payload.push(0),
    }
    payload.extend_from_slice(&identity.base_projection_digest);
    payload.extend_from_slice(&identity.result_projection_digest);
    match identity.delta_digest {
        Some(delta) => {
            payload.push(1);
            payload.extend_from_slice(&delta);
        }
        None => payload.push(0),
    }
    payload.extend_from_slice(&identity.evidence_digest);
    payload.extend_from_slice(&identity.task_revision.to_be_bytes());
    Ok(Sha256::digest(&payload).into())
}

pub fn publication_result_digest(identity: &PublicationResultIdentity) -> Result<Digest, Error> {
    for id in [
        &identity.publication_id,
        &identity.task_id,
        &identity.item_id,
        &identity.result_version_id,
        &identity.algorithm_version,
    ] {
        if id.is_empty() {
            return Err(invalid("publication result identity field is empty"));
        }
    }
    if identity.task_revision == 0 {
        return Err(invalid("task_revision must be positive"));
    }
    if let Some(base) = &identity.base_version_id
        && base.is_empty()
    {
        return Err(invalid("base_version_id is empty"));
    }
    match identity.outcome {
        UpdateOutcome::NoChange => {
            if identity.base_version_id.as_deref() != Some(identity.result_version_id.as_str()) {
                return Err(invalid("no-change publication result requires base=result"));
            }
        }
        UpdateOutcome::Published => {
            if identity.base_version_id.as_deref() == Some(identity.result_version_id.as_str()) {
                return Err(invalid("published publication result reuses base version"));
            }
        }
    }
    let mut payload = Vec::new();
    payload.extend_from_slice(b"x-data-publication-result-v1\0");
    push_text(&mut payload, &identity.publication_id);
    push_text(&mut payload, &identity.task_id);
    push_text(&mut payload, &identity.item_id);
    payload.push(match identity.publication_kind {
        PublicationKind::Collection => 1,
        PublicationKind::Yearly => 2,
        PublicationKind::Republish => 3,
    });
    match &identity.base_version_id {
        Some(base) => {
            payload.push(1);
            push_text(&mut payload, base);
        }
        None => payload.push(0),
    }
    push_text(&mut payload, &identity.result_version_id);
    payload.push(match identity.outcome {
        UpdateOutcome::NoChange => 1,
        UpdateOutcome::Published => 2,
    });
    push_text(&mut payload, &identity.algorithm_version);
    payload.extend_from_slice(&identity.source_binding_digest);
    payload.extend_from_slice(&identity.evidence_digest);
    payload.extend_from_slice(&identity.task_revision.to_be_bytes());
    Ok(Sha256::digest(&payload).into())
}

pub fn full_revision_digest(
    base_projection_digest: Digest,
    result_projection_digest: Digest,
) -> Digest {
    let mut payload = Vec::new();
    payload.extend_from_slice(b"x-data-full-revision-v1\0");
    payload.extend_from_slice(&base_projection_digest);
    payload.extend_from_slice(&result_projection_digest);
    Sha256::digest(&payload).into()
}

fn evidence_digest(kind: EvidenceKind, canonical_json: &str) -> Digest {
    let mut payload = Vec::new();
    payload.extend_from_slice(b"x-data-evidence-v1\0");
    push_text(&mut payload, kind.as_str());
    push_bytes(&mut payload, canonical_json.as_bytes());
    Sha256::digest(&payload).into()
}

fn check_evidence_size(kind: EvidenceKind, canonical_json: &str) -> Result<(), Error> {
    if canonical_json.len() > kind.byte_limit() {
        return Err(invalid("evidence exceeds kind byte limit"));
    }
    Ok(())
}

fn check_evidence_semantics(kind: EvidenceKind, value: &Value) -> Result<(), Error> {
    if !value.is_object() {
        return Err(invalid("evidence is not a JSON object"));
    }
    reject_bar_arrays(value)?;
    if kind == EvidenceKind::Version {
        check_version_evidence(value)?;
    }
    Ok(())
}

fn reject_bar_arrays(value: &Value) -> Result<(), Error> {
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                if (key == "bars" || key == "records") && item.is_array() {
                    return Err(invalid("evidence embeds a bars/records array"));
                }
                reject_bar_arrays(item)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                reject_bar_arrays(item)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn check_version_evidence(value: &Value) -> Result<(), Error> {
    let evidence: VersionEvidence = serde_json::from_value(value.clone())
        .map_err(|_| invalid("version evidence shape mismatch"))?;
    if evidence.source.is_empty() || evidence.coverage_reason.is_empty() {
        return Err(invalid("version evidence source/coverage_reason empty"));
    }
    let unit = &evidence.unit_currency;
    for field in [
        &unit.price_currency,
        &unit.price_unit,
        &unit.volume_unit,
        &unit.amount_currency,
        &unit.amount_unit,
    ] {
        check_evidence_fields(&field.status, &field.value, &field.reason)?;
    }
    check_evidence_fields(
        &evidence.adjustment_evidence.status,
        &evidence.adjustment_evidence.value,
        &evidence.adjustment_evidence.reason,
    )?;
    // Schema 22 stores one version evidence row per 口径 (raw / qfq); both are
    // valid here, an unrecognized value is not (issue-383 §2.3 item 3).
    if evidence.adjustment_evidence.status != EvidenceStatus::Confirmed
        || !evidence
            .adjustment_evidence
            .value
            .as_deref()
            .is_some_and(|value| Adjustment::from_str_exact(value).is_some())
    {
        return Err(invalid("adjustment evidence is not a confirmed 口径"));
    }
    Ok(())
}

fn check_evidence_fields(
    status: &EvidenceStatus,
    value: &Option<String>,
    reason: &str,
) -> Result<(), Error> {
    if reason.is_empty() {
        return Err(invalid("evidence reason is empty"));
    }
    match status {
        EvidenceStatus::Confirmed => {
            if value.as_deref().is_none_or(str::is_empty) {
                return Err(invalid("confirmed evidence lacks a value"));
            }
        }
        EvidenceStatus::Unknown => {
            if value.is_some() {
                return Err(invalid("unknown evidence carries a value"));
            }
        }
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

pub(crate) fn period_code(period: Period) -> u8 {
    match period {
        Period::Day => 1,
        Period::Year => 2,
    }
}

pub(crate) fn canonical_entity(entity: &str) -> Result<(), Error> {
    let canonical = entity.len() == 9
        && (entity.starts_with("sh.") || entity.starts_with("sz."))
        && entity.as_bytes()[3..].iter().all(u8::is_ascii_digit);
    if canonical {
        Ok(())
    } else {
        Err(invalid(format!("non-canonical entity {entity}")))
    }
}

pub(crate) fn epoch_day(date: NaiveDate) -> i64 {
    date.signed_duration_since(NaiveDate::from_ymd_opt(1970, 1, 1).unwrap())
        .num_days()
}

pub(crate) fn epoch_date(day: i64) -> Result<NaiveDate, Error> {
    let delta =
        chrono::TimeDelta::try_days(day).ok_or_else(|| invalid("trade_day out of range"))?;
    NaiveDate::from_ymd_opt(1970, 1, 1)
        .unwrap()
        .checked_add_signed(delta)
        .ok_or_else(|| invalid("trade_day out of date range"))
}

/// Validate a series `price_scale` against the DDL ceiling; keeps
/// `Decimal::new`/`10^scale` inside their representable range.
fn checked_scale(price_scale: u32) -> Result<u32, Error> {
    if price_scale > MAX_PRICE_SCALE {
        return Err(invalid("price_scale exceeds 6"));
    }
    Ok(price_scale)
}

/// Integer price at `price_scale`: `value * 10^scale`, rejecting any value
/// that does not round-trip exactly (more decimals than the series allows).
fn units(value: Decimal, price_scale: u32) -> Result<i64, Error> {
    let scale = checked_scale(price_scale)?;
    let denominator = Decimal::from(10u32.pow(scale));
    let scaled = value
        .checked_mul(denominator)
        .ok_or_else(|| invalid("price scale overflow"))?;
    let units = scaled
        .to_i64()
        .ok_or_else(|| invalid("price exceeds i64 units range"))?;
    if Decimal::from(units).checked_div(denominator) != Some(value) {
        return Err(invalid("price has more decimals than price_scale"));
    }
    Ok(units)
}

fn volume(value: Decimal) -> Result<i64, Error> {
    let volume = value
        .to_i64()
        .ok_or_else(|| invalid("volume exceeds i64 range"))?;
    if Decimal::from(volume) != value {
        return Err(invalid("volume is not integral"));
    }
    if volume < 0 {
        return Err(invalid("negative volume"));
    }
    Ok(volume)
}

fn amount(value: Decimal) -> Result<String, Error> {
    if value < Decimal::ZERO {
        return Err(invalid("negative amount"));
    }
    let text = value.normalize().to_string();
    if text.parse::<Decimal>() != Ok(value) {
        return Err(invalid("canonical amount does not roundtrip"));
    }
    Ok(text)
}

fn push_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn push_text(out: &mut Vec<u8>, value: &str) {
    push_bytes(out, value.as_bytes());
}

/// Lexical equivalent of `parsed.normalize().to_string() == text` for a
/// value that already parsed as `Decimal`: optional '-', a non-empty digit
/// integer part without leading zeros ("0" alone is fine), and an optional
/// non-empty digit fraction that does not end in '0'. No '+', exponent, or
/// bare '.' ever survives normalization, and "-0…" normalizes to "0".
fn canonical_decimal(text: &str) -> bool {
    let body = text.strip_prefix('-').unwrap_or(text);
    let (int, frac) = match body.split_once('.') {
        Some((i, f)) => (i, f),
        None => (body, ""),
    };
    if int.is_empty()
        || !int.bytes().all(|b| b.is_ascii_digit())
        || (int.len() > 1 && int.starts_with('0'))
    {
        return false;
    }
    if body.contains('.')
        && (frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit()) || frac.ends_with('0'))
    {
        return false;
    }
    if text.starts_with('-') && body.bytes().all(|b| b == b'0' || b == b'.') {
        return false;
    }
    true
}

pub(crate) fn bar_encoding(out: &mut Vec<u8>, bar: &CompactBar) {
    out.extend_from_slice(b"x-data-bar-v1\0");
    out.extend_from_slice(&bar.trade_day.to_be_bytes());
    for value in [
        bar.open_units,
        bar.high_units,
        bar.low_units,
        bar.close_units,
    ] {
        out.extend_from_slice(&value.to_be_bytes());
    }
    match bar.volume {
        Some(value) => {
            out.push(1);
            out.extend_from_slice(&value.to_be_bytes());
        }
        None => out.push(0),
    }
    match &bar.amount {
        Some(text) => {
            out.push(1);
            push_text(out, text);
        }
        None => out.push(0),
    }
    match bar.r7_raw {
        Some(value) => {
            out.push(1);
            out.extend_from_slice(&value.to_be_bytes());
        }
        None => out.push(0),
    }
}

fn check_delta_ops(ops: &[DeltaOp]) -> Result<(), Error> {
    if ops.is_empty() {
        return Err(invalid("delta requires at least one op"));
    }
    let mut previous_end: Option<i64> = None;
    for op in ops {
        if op.range_start_day > op.range_end_day {
            return Err(invalid("delta range bounds inverted"));
        }
        if let Some(end) = previous_end
            && op.range_start_day <= end
        {
            return Err(invalid("delta ranges overlap or are unordered"));
        }
        previous_end = Some(op.range_end_day);
        if let DeltaKind::Upsert(clip) = &op.kind {
            if clip.clip_start_day > clip.clip_end_day {
                return Err(invalid("clip bounds inverted"));
            }
            if clip.row_count == 0 {
                return Err(invalid("clip row_count must be positive"));
            }
            if clip.clip_start_day < op.range_start_day || clip.clip_end_day > op.range_end_day {
                return Err(invalid("upsert clip outside op range"));
            }
        }
    }
    Ok(())
}

fn delta_op_encoding(out: &mut Vec<u8>, op: &DeltaOp) {
    match &op.kind {
        DeltaKind::Upsert(clip) => {
            out.push(1);
            out.extend_from_slice(&op.range_start_day.to_be_bytes());
            out.extend_from_slice(&op.range_end_day.to_be_bytes());
            out.push(1);
            out.extend_from_slice(&clip.segment_digest);
            out.push(1);
            out.extend_from_slice(&clip.clip_start_day.to_be_bytes());
            out.push(1);
            out.extend_from_slice(&clip.clip_end_day.to_be_bytes());
            out.extend_from_slice(&clip.row_count.to_be_bytes());
        }
        DeltaKind::Remove => {
            out.push(2);
            out.extend_from_slice(&op.range_start_day.to_be_bytes());
            out.extend_from_slice(&op.range_end_day.to_be_bytes());
            out.push(0);
            out.push(0);
            out.push(0);
            out.extend_from_slice(&0u32.to_be_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn bar1() -> CompactBar {
        CompactBar {
            trade_day: 0,
            open_units: -123,
            high_units: -100,
            low_units: -200,
            close_units: -150,
            volume: Some(10),
            amount: Some("12.34".into()),
            r7_raw: Some(7),
        }
    }

    fn bar2() -> CompactBar {
        CompactBar {
            trade_day: 1,
            open_units: 100,
            high_units: 200,
            low_units: 50,
            close_units: 150,
            volume: None,
            amount: None,
            r7_raw: None,
        }
    }

    fn clip(digest: Digest) -> SegmentClip {
        SegmentClip {
            segment_digest: digest,
            clip_start_day: 0,
            clip_end_day: 1,
            row_count: 2,
        }
    }

    fn source_bar(day: i64, price: &str) -> Bar {
        let price = Decimal::from_str(price).unwrap();
        Bar {
            date: epoch_date(day).unwrap(),
            open: price,
            high: price,
            low: price,
            close: price,
            volume: None,
            amount: None,
            r7_raw: None,
        }
    }

    #[test]
    fn fixed_framing_digests() {
        let bars = [bar1(), bar2()];
        let segment = segment_digest("sh.600006", Period::Day, &bars).unwrap();
        assert_eq!(
            digest_hex(&segment),
            "198e18b303e0caeb56076b2356971cd929c100c736e9ccd58cb945103f9ab275"
        );
        let checkpoint =
            checkpoint_projection_digest("sh.600006", Period::Day, &[clip(segment)]).unwrap();
        assert_eq!(
            digest_hex(&checkpoint),
            "10a347ad4f3ba1d413e91ae865ee481ca342d4b13ca77e19951b60e0e16947f7"
        );
        let ops = [DeltaOp {
            range_start_day: 0,
            range_end_day: 1,
            kind: DeltaKind::Upsert(clip(segment)),
        }];
        let (delta, projection) = delta_projection_digest([0x11; 32], &ops).unwrap();
        assert_eq!(
            digest_hex(&delta),
            "e59db36dc0c0558640e04a9543de474424f282e9c970a2db52815b8770441716"
        );
        assert_eq!(
            digest_hex(&projection),
            "d5d2bf47e163bc3f87eb2f4d07f726371d875cadc023240fa1557caa9a4549e3"
        );
    }

    #[test]
    fn negative_price_roundtrip() {
        let original = source_bar(0, "-1.23");
        let compact = CompactBar::from_bar(&original, Period::Day, PRICE_SCALE_CENTS).unwrap();
        assert_eq!(compact.open_units, -123);
        assert_eq!(
            compact.to_bar(Period::Day, PRICE_SCALE_CENTS).unwrap(),
            original
        );
        let mut stored = compact.clone();
        stored.amount = Some("12.34".into());
        stored.volume = Some(10);
        stored.r7_raw = Some(7);
        assert_eq!(
            stored
                .to_bar(Period::Day, PRICE_SCALE_CENTS)
                .unwrap()
                .amount,
            Decimal::new(1234, 2).into()
        );
    }

    #[test]
    fn rejects_sub_2dp_price() {
        let bar = source_bar(0, "1.234");
        assert!(CompactBar::from_bar(&bar, Period::Day, PRICE_SCALE_CENTS).is_err());
        let bar = source_bar(0, "1.235");
        assert!(CompactBar::from_bar(&bar, Period::Day, PRICE_SCALE_CENTS).is_err());
    }

    /// Schema 22 (issue-383 §3.3): the integer price is interpreted through
    /// the series' `price_scale`, so 3dp/6dp values must round-trip and a
    /// value with more decimals than the series allows must be rejected
    /// rather than silently truncated.
    #[test]
    fn price_scale_accepts_exact_precision_and_rejects_extra_decimals() {
        for (text, scale) in [("4.178", 3), ("1.234567", 6), ("8.445", 3), ("0.000001", 6)] {
            let original = source_bar(0, text);
            let compact = CompactBar::from_bar(&original, Period::Day, scale).unwrap();
            assert_eq!(
                compact.to_bar(Period::Day, scale).unwrap(),
                original,
                "{text} at scale {scale}"
            );
        }
        // 4dp under a 3dp series: rejected, not rounded.
        let bar = source_bar(0, "1.2345");
        assert!(CompactBar::from_bar(&bar, Period::Day, 3).is_err());
        // Above the DDL ceiling: refused in both directions.
        let bar = source_bar(0, "1.0");
        assert!(CompactBar::from_bar(&bar, Period::Day, MAX_PRICE_SCALE + 1).is_err());
        assert!(bar1().to_bar(Period::Day, MAX_PRICE_SCALE + 1).is_err());
    }

    #[test]
    fn rejects_decimal_overflow() {
        let bar = source_bar(0, "79228162514264337593543950335");
        assert!(CompactBar::from_bar(&bar, Period::Day, PRICE_SCALE_CENTS).is_err());
        let bar = source_bar(0, "92233720368547758.08");
        assert!(CompactBar::from_bar(&bar, Period::Day, PRICE_SCALE_CENTS).is_err());
        let bar = source_bar(0, "92233720368547758.07");
        assert!(CompactBar::from_bar(&bar, Period::Day, PRICE_SCALE_CENTS).is_ok());
    }

    #[test]
    fn rejects_bad_volume() {
        let mut bar = source_bar(0, "1.00");
        bar.volume = Some(Decimal::from_str("-5").unwrap());
        assert!(CompactBar::from_bar(&bar, Period::Day, PRICE_SCALE_CENTS).is_err());
        bar.volume = Some(Decimal::from_str("1.5").unwrap());
        assert!(CompactBar::from_bar(&bar, Period::Day, PRICE_SCALE_CENTS).is_err());
        bar.volume = Some(Decimal::from_str("9223372036854775808").unwrap());
        assert!(CompactBar::from_bar(&bar, Period::Day, PRICE_SCALE_CENTS).is_err());
    }

    #[test]
    fn canonical_decimal_matches_normalize_stringify() {
        // Equivalence oracle: `check()` parses first, then applies the
        // lexical check — so the composed predicate must equal the old
        // `parse ok && normalize().to_string() == original` on every input.
        let cases = [
            "0",
            "-0",
            "0.0",
            "-0.0",
            "0.00",
            "1.",
            "5.",
            ".5",
            "-.5",
            "+5",
            "1e3",
            "1E3",
            "1e-3",
            "007",
            "0.50",
            "0.05",
            "1.230",
            "-1.230",
            "10.0",
            "0.001",
            "-0.5",
            "0.000",
            "1",
            "-1",
            "12.34",
            "-12.34",
            "100.01",
            "0.10",
            "00.5",
            "1.2.3",
            "",
            "-",
            ".",
            "1 0",
            " 5",
            "5 ",
            "1_0",
            "79228162514264337593543950335",
            "99999999999999999999999999999",
            "1234567890123456789012345678.9",
            "0.0000000000000000000000000001",
            "-0.0000000000000000000000000001",
            "9007199254740993.99",
        ];
        for case in cases {
            let parsed = case.parse::<Decimal>();
            let composed = parsed.is_ok() && canonical_decimal(case);
            let oracle = parsed
                .map(|d| d.normalize().to_string() == case)
                .unwrap_or(false);
            assert_eq!(composed, oracle, "case {case:?}");
        }
        // Round-trip: every produced canonical amount stays canonical.
        for v in [
            "0",
            "1",
            "0.1",
            "123456.78",
            "92233720368547758.07",
            "0.0000000000000000000000000001",
        ] {
            let d = Decimal::from_str(v).unwrap().normalize();
            let s = d.to_string();
            assert!(canonical_decimal(&s), "normalized {s:?} must be canonical");
        }
    }

    /// XP-018 differential oracle: the ORIGINAL two-pass implementation —
    /// `check` validations followed by a second amount parse and a
    /// separate epoch-date conversion inside `to_bar` — copied verbatim.
    /// `decode_checked` must produce identical `Bar`/`Error` outcomes.
    fn reference_check(bar: &CompactBar, period: Period) -> Result<(), Error> {
        epoch_date(bar.trade_day)?;
        if bar.high_units < bar.open_units
            || bar.high_units < bar.close_units
            || bar.high_units < bar.low_units
            || bar.low_units > bar.open_units
            || bar.low_units > bar.close_units
        {
            return Err(invalid("OHLC ordering violated"));
        }
        if let Some(volume) = bar.volume
            && volume < 0
        {
            return Err(invalid("negative volume"));
        }
        if let Some(amount) = &bar.amount {
            let parsed = amount
                .parse::<Decimal>()
                .map_err(|_| invalid("stored amount is not a decimal"))?;
            if parsed < Decimal::ZERO {
                return Err(invalid("negative amount"));
            }
            if !canonical_decimal(amount) {
                return Err(invalid("stored amount is not canonical"));
            }
        }
        if period == Period::Year && bar.r7_raw.is_some() {
            return Err(invalid("yearly bar must not carry r7_raw"));
        }
        Ok(())
    }

    fn reference_to_bar(bar: &CompactBar, period: Period, price_scale: u32) -> Result<Bar, Error> {
        reference_check(bar, period)?;
        let amount = bar
            .amount
            .as_deref()
            .map(|text| text.parse::<Decimal>())
            .transpose()
            .map_err(|_| invalid("stored amount is not a decimal"))?;
        Ok(Bar {
            date: epoch_date(bar.trade_day)?,
            open: Decimal::new(bar.open_units, price_scale),
            high: Decimal::new(bar.high_units, price_scale),
            low: Decimal::new(bar.low_units, price_scale),
            close: Decimal::new(bar.close_units, price_scale),
            volume: bar.volume.map(Decimal::from),
            amount,
            r7_raw: bar.r7_raw,
        })
    }

    /// XP-018 codec equivalence: `check`/`to_bar` (delegating to
    /// `decode_checked`) vs the verbatim two-pass oracle — same `Bar` on
    /// success, identical `Error` on failure, across both periods and
    /// malformed inputs including multi-defect bars that pin the
    /// original error order.
    #[test]
    fn decode_checked_matches_original_two_pass_oracle() {
        let mut variants = vec![bar1(), bar2()];
        // None/Some volume/amount/r7 combinations and canonical amounts.
        for (volume, amount, r7) in [
            (None, None, None),
            (Some(0), Some("0"), Some(0)),
            (Some(5), Some("0.05"), None),
            (Some(10), Some("12.34"), Some(7)),
        ] {
            let mut bar = bar2();
            bar.volume = volume;
            bar.amount = amount.map(str::to_string);
            bar.r7_raw = r7;
            variants.push(bar);
        }
        // Amount lexical rejections, kept in check() error order:
        // parse failure, negative, non-canonical.
        for amount in [
            "-0",
            "007",
            "1.230",
            "+1",
            "1e3",
            "",
            "abc",
            "-1",
            "79228162514264337593543950336",
        ] {
            let mut bar = bar2();
            bar.amount = Some(amount.into());
            variants.push(bar);
        }
        // Invalid epoch day.
        let mut bar = bar2();
        bar.trade_day = i64::MAX;
        variants.push(bar);
        // OHLC inversion.
        let mut bar = bar2();
        bar.high_units = 49;
        variants.push(bar);
        // Negative volume.
        let mut bar = bar2();
        bar.volume = Some(-1);
        variants.push(bar);
        // Yearly r7 rejection (valid under Period::Day).
        let mut bar = bar2();
        bar.r7_raw = Some(1);
        variants.push(bar);
        // Multi-defect bars pin the original error order: day first,
        // then OHLC, then volume, then amount, then yearly r7.
        let mut bar = bar2();
        bar.trade_day = i64::MAX;
        bar.high_units = 49;
        bar.volume = Some(-1);
        bar.amount = Some("abc".into());
        bar.r7_raw = Some(1);
        variants.push(bar);
        let mut bar = bar2();
        bar.high_units = 49;
        bar.volume = Some(-1);
        bar.amount = Some("-1".into());
        bar.r7_raw = Some(1);
        variants.push(bar);
        for bar in &variants {
            for period in [Period::Day, Period::Year] {
                assert_eq!(
                    bar.check(period),
                    reference_check(bar, period),
                    "check diverged for {bar:?} {period:?}"
                );
                assert_eq!(
                    bar.to_bar(period, PRICE_SCALE_CENTS),
                    reference_to_bar(bar, period, PRICE_SCALE_CENTS),
                    "to_bar diverged for {bar:?} {period:?}"
                );
            }
        }
    }

    #[test]
    fn rejects_negative_amount() {
        let mut bar = source_bar(0, "1.00");
        bar.amount = Some(Decimal::from_str("-0.01").unwrap());
        assert!(CompactBar::from_bar(&bar, Period::Day, PRICE_SCALE_CENTS).is_err());
    }

    #[test]
    fn rejects_yearly_r7() {
        let mut bar = source_bar(0, "1.00");
        bar.r7_raw = Some(1);
        assert!(CompactBar::from_bar(&bar, Period::Year, PRICE_SCALE_CENTS).is_err());
        assert!(CompactBar::from_bar(&bar, Period::Day, PRICE_SCALE_CENTS).is_ok());
        assert!(bar1().to_bar(Period::Year, PRICE_SCALE_CENTS).is_err());
    }

    #[test]
    fn rejects_invalid_ohlc() {
        let mut bar = source_bar(0, "1.00");
        bar.low = Decimal::from_str("1.01").unwrap();
        assert!(CompactBar::from_bar(&bar, Period::Day, PRICE_SCALE_CENTS).is_err());
        let mut bar = source_bar(0, "1.00");
        bar.high = Decimal::from_str("0.99").unwrap();
        assert!(CompactBar::from_bar(&bar, Period::Day, PRICE_SCALE_CENTS).is_err());
    }

    #[test]
    fn rejects_bad_segments() {
        let bars = [bar1(), bar2()];
        assert!(segment_digest("SH.600006", Period::Day, &bars).is_err());
        assert!(segment_digest("sh.60006", Period::Day, &bars).is_err());
        assert!(segment_digest("sz.000002", Period::Day, &bars).is_ok());
        assert!(segment_digest("sh.600006", Period::Day, &[]).is_err());
        let oversize = vec![bar2(); MAX_SEGMENT_BARS + 1];
        assert!(segment_digest("sh.600006", Period::Day, &oversize).is_err());
        let unordered = [bar2(), bar1()];
        assert!(segment_digest("sh.600006", Period::Day, &unordered).is_err());
        let duplicated = [bar1(), bar1()];
        assert!(segment_digest("sh.600006", Period::Day, &duplicated).is_err());
    }

    #[test]
    fn rejects_bad_clips_and_deltas() {
        let segment = segment_digest("sh.600006", Period::Day, &[bar1(), bar2()]).unwrap();
        let good = clip(segment);
        assert!(
            checkpoint_projection_digest("SH.600006", Period::Day, std::slice::from_ref(&good))
                .is_err()
        );
        assert!(
            checkpoint_projection_digest("sh.60006", Period::Day, std::slice::from_ref(&good))
                .is_err()
        );
        assert!(checkpoint_projection_digest("sh.600006", Period::Day, &[]).is_err());
        let mut zero = good.clone();
        zero.row_count = 0;
        assert!(checkpoint_projection_digest("sh.600006", Period::Day, &[zero]).is_err());
        let mut inverted = good.clone();
        inverted.clip_start_day = 2;
        assert!(checkpoint_projection_digest("sh.600006", Period::Day, &[inverted]).is_err());
        let mut second = good.clone();
        second.clip_start_day = 1;
        second.clip_end_day = 3;
        assert!(
            checkpoint_projection_digest("sh.600006", Period::Day, &[good.clone(), second])
                .is_err()
        );
        let mut reversed = good.clone();
        reversed.clip_start_day = 4;
        reversed.clip_end_day = 5;
        assert!(
            checkpoint_projection_digest("sh.600006", Period::Day, &[reversed, good.clone()])
                .is_err()
        );
        assert!(delta_digest(&[]).is_err());
        let upsert = DeltaOp {
            range_start_day: 0,
            range_end_day: 1,
            kind: DeltaKind::Upsert(good.clone()),
        };
        assert!(delta_digest(std::slice::from_ref(&upsert)).is_ok());
        let outside = DeltaOp {
            range_start_day: 0,
            range_end_day: 0,
            kind: DeltaKind::Upsert(good.clone()),
        };
        assert!(delta_digest(&[outside]).is_err());
        let remove = DeltaOp {
            range_start_day: 1,
            range_end_day: 1,
            kind: DeltaKind::Remove,
        };
        assert!(delta_digest(&[upsert.clone(), remove]).is_err());
        let remove_later = DeltaOp {
            range_start_day: 2,
            range_end_day: 3,
            kind: DeltaKind::Remove,
        };
        assert!(delta_digest(&[upsert, remove_later]).is_ok());
    }

    use serde_json::json;

    fn version_evidence_json() -> Value {
        let field = |status: &str, value: Value| json!({"status": status, "value": value, "reason": "evidence bound"});
        json!({
            "source": "official",
            "coverage_reason": "full history verified",
            "unit_currency": {
                "price_currency": field("Confirmed", json!("CNY")),
                "price_unit": field("Confirmed", json!("share")),
                "volume_unit": field("Unknown", Value::Null),
                "amount_currency": field("Confirmed", json!("CNY")),
                "amount_unit": field("Unknown", Value::Null)
            },
            "adjustment_evidence": field("Confirmed", json!("qfq"))
        })
    }

    fn object_of_canonical_len(len: usize) -> Value {
        let pad = len - "{\"pad\":\"\"}".len();
        let value = json!({"pad": "x".repeat(pad)});
        assert_eq!(
            serde_json_canonicalizer::to_string(&value).unwrap().len(),
            len
        );
        value
    }

    fn daily_identity() -> VersionIdentity {
        VersionIdentity {
            version_id: "version-1".into(),
            entity: "sh.600006".into(),
            period: Period::Day,
            source_binding_digest: [0x11; 32],
            parent_version_id: None,
            manifest_mode: ManifestMode::Checkpoint,
            delta_depth: 0,
            projection_digest: [0x22; 32],
            record_count: 2,
            first_day: 0,
            last_day: 1,
            coverage_status: CoverageStatus::Complete,
            evidence_digest: [0x33; 32],
            published_at: "2026-09-21T00:00:00+00:00".into(),
            yearly_lineage: None,
        }
    }

    fn yearly_lineage() -> YearlyLineageIdentity {
        YearlyLineageIdentity {
            base_daily_version_id: "daily-1".into(),
            algorithm_version: "local-yearly-v1".into(),
            first_trade_day: 0,
            last_trade_day: 365,
            as_of_day: 365,
        }
    }

    fn yearly_identity() -> VersionIdentity {
        VersionIdentity {
            version_id: "yearly-1".into(),
            entity: "sh.600006".into(),
            period: Period::Year,
            source_binding_digest: [0x11; 32],
            parent_version_id: None,
            manifest_mode: ManifestMode::Checkpoint,
            delta_depth: 0,
            projection_digest: [0x44; 32],
            record_count: 1,
            first_day: 365,
            last_day: 365,
            coverage_status: CoverageStatus::Complete,
            evidence_digest: [0x55; 32],
            published_at: "2026-09-21T00:00:00+00:00".into(),
            yearly_lineage: Some(yearly_lineage()),
        }
    }

    #[test]
    fn evidence_fixed_vector() {
        let bundle =
            canonical_evidence(EvidenceKind::Continuity, &json!({"b": 1, "a": 2})).unwrap();
        assert_eq!(bundle.canonical_json, "{\"a\":2,\"b\":1}");
        assert_eq!(
            digest_hex(&bundle.digest),
            "2b95996080eb7b5d5785e1178859a1c6f93b4257e192ef810b3ac1c753910916"
        );
    }

    #[test]
    fn verify_evidence_roundtrip_and_rejections() {
        let bundle =
            canonical_evidence(EvidenceKind::Continuity, &json!({"b": 1, "a": 2})).unwrap();
        let value = verify_evidence(
            EvidenceKind::Continuity,
            &bundle.canonical_json,
            bundle.digest,
        )
        .unwrap();
        assert_eq!(value, json!({"a": 2, "b": 1}));
        assert!(
            verify_evidence(
                EvidenceKind::Continuity,
                "{ \"a\" : 2, \"b\" : 1 }",
                bundle.digest
            )
            .is_err()
        );
        assert!(verify_evidence(EvidenceKind::Continuity, "not json", bundle.digest).is_err());
        let mut wrong = bundle.digest;
        wrong[0] ^= 1;
        assert!(verify_evidence(EvidenceKind::Continuity, &bundle.canonical_json, wrong).is_err());
        let other = canonical_evidence(EvidenceKind::Generation, &json!({"a": 2, "b": 1})).unwrap();
        assert!(
            verify_evidence(
                EvidenceKind::Continuity,
                &bundle.canonical_json,
                other.digest
            )
            .is_err()
        );
    }

    #[test]
    fn evidence_rejects_embedded_bar_arrays() {
        assert!(canonical_evidence(EvidenceKind::Generation, &json!({"bars": [1, 2]})).is_err());
        assert!(
            canonical_evidence(
                EvidenceKind::Generation,
                &json!({"outer": {"records": [1]}})
            )
            .is_err()
        );
        assert!(
            canonical_evidence(EvidenceKind::Generation, &json!({"list": [{"bars": []}]})).is_err()
        );
        assert!(
            canonical_evidence(
                EvidenceKind::Generation,
                &json!({"bars": "text", "records": {"x": 1}})
            )
            .is_ok()
        );
        assert!(canonical_evidence(EvidenceKind::Generation, &json!([1, 2])).is_err());
        assert!(canonical_evidence(EvidenceKind::Generation, &json!("text")).is_err());
    }

    #[test]
    fn evidence_size_boundaries() {
        for (kind, limit) in [
            (EvidenceKind::Continuity, 4_096usize),
            (EvidenceKind::Yearly, 16_384),
            (EvidenceKind::Generation, 262_144),
            (EvidenceKind::Catalog, 262_144),
            (EvidenceKind::BatchSelection, 4_194_304),
            (EvidenceKind::Derived, 65_536),
        ] {
            assert!(canonical_evidence(kind, &object_of_canonical_len(limit)).is_ok());
            assert!(canonical_evidence(kind, &object_of_canonical_len(limit + 1)).is_err());
        }
        let mut evidence = version_evidence_json();
        evidence["coverage_reason"] = json!("x".repeat(16_384));
        assert!(canonical_evidence(EvidenceKind::Version, &evidence).is_err());
    }

    #[test]
    fn version_evidence_malformed_rejected() {
        let valid = version_evidence_json();
        assert!(canonical_evidence(EvidenceKind::Version, &valid).is_ok());
        let mut extra = valid.clone();
        extra["surprise"] = json!(1);
        assert!(canonical_evidence(EvidenceKind::Version, &extra).is_err());
        let mut missing = valid.clone();
        missing["unit_currency"]
            .as_object_mut()
            .unwrap()
            .remove("price_unit");
        assert!(canonical_evidence(EvidenceKind::Version, &missing).is_err());
        let mut mismatch = valid.clone();
        mismatch["unit_currency"]["price_unit"]["status"] = json!("Unknown");
        assert!(canonical_evidence(EvidenceKind::Version, &mismatch).is_err());
        let mut no_value = valid.clone();
        no_value["unit_currency"]["price_unit"]["value"] = Value::Null;
        assert!(canonical_evidence(EvidenceKind::Version, &no_value).is_err());
        let mut unknown_value = valid.clone();
        unknown_value["unit_currency"]["volume_unit"]["value"] = json!("leak");
        assert!(canonical_evidence(EvidenceKind::Version, &unknown_value).is_err());
        let mut no_reason = valid.clone();
        no_reason["unit_currency"]["volume_unit"]["reason"] = json!("");
        assert!(canonical_evidence(EvidenceKind::Version, &no_reason).is_err());
        let mut hfq = valid.clone();
        hfq["adjustment_evidence"]["value"] = json!("hfq");
        assert!(canonical_evidence(EvidenceKind::Version, &hfq).is_err());
        let mut unconfirmed = valid.clone();
        unconfirmed["adjustment_evidence"]["status"] = json!("Unknown");
        assert!(canonical_evidence(EvidenceKind::Version, &unconfirmed).is_err());
        let mut no_source = valid.clone();
        no_source["source"] = json!("");
        assert!(canonical_evidence(EvidenceKind::Version, &no_source).is_err());
        let mut no_reason2 = valid.clone();
        no_reason2["coverage_reason"] = json!("");
        assert!(canonical_evidence(EvidenceKind::Version, &no_reason2).is_err());
    }

    #[test]
    fn source_binding_fixed_vector() {
        let binding = SourceBinding {
            generation_id: "gen-2".into(),
            generation_manifest_sha256: [1; 32],
            day_member_sha256: [2; 32],
            security_event_sha256: [3; 32],
            evidence_digest: [4; 32],
        };
        assert_eq!(
            digest_hex(&source_binding_digest(&binding).unwrap()),
            "2268d65f32921a455ef412312467dca8cd12945b5fa39295866aeb0627c688c0"
        );
        let mut empty = binding.clone();
        empty.generation_id = String::new();
        assert!(source_binding_digest(&empty).is_err());
    }

    #[test]
    fn version_identity_fixed_vectors() {
        assert_eq!(
            digest_hex(&version_integrity_digest(&daily_identity()).unwrap()),
            "d8cf6f98195bd579c690060a28b27a1c4c79c53772b9142373a4f4d58fab62cc"
        );
        assert_eq!(
            digest_hex(&version_integrity_digest(&yearly_identity()).unwrap()),
            "14969aa54d66f79edb6e71ee68c2b355d4861b1618bec4a983e1582382b50807"
        );
    }

    #[test]
    fn version_identity_rejections() {
        let good = daily_identity();
        let mut zulu = good.clone();
        zulu.published_at = "2026-09-21T00:00:00Z".into();
        assert!(version_integrity_digest(&zulu).is_err());
        let mut offset = good.clone();
        offset.published_at = "2026-09-21T01:00:00+01:00".into();
        assert!(version_integrity_digest(&offset).is_err());
        let mut garbage = good.clone();
        garbage.published_at = "not-a-date".into();
        assert!(version_integrity_digest(&garbage).is_err());
        let mut fractional = good.clone();
        fractional.published_at = "2026-09-21T00:00:00.000+00:00".into();
        assert!(version_integrity_digest(&fractional).is_err());
        let mut checkpoint_parent = good.clone();
        checkpoint_parent.parent_version_id = Some("p".into());
        assert!(version_integrity_digest(&checkpoint_parent).is_err());
        let mut checkpoint_depth = good.clone();
        checkpoint_depth.delta_depth = 1;
        assert!(version_integrity_digest(&checkpoint_depth).is_err());
        let mut delta_no_parent = good.clone();
        delta_no_parent.manifest_mode = ManifestMode::Delta;
        delta_no_parent.delta_depth = 1;
        assert!(version_integrity_digest(&delta_no_parent).is_err());
        let mut delta = good.clone();
        delta.manifest_mode = ManifestMode::Delta;
        delta.parent_version_id = Some("parent-1".into());
        delta.delta_depth = 1;
        assert!(version_integrity_digest(&delta).is_ok());
        let mut delta_zero = delta.clone();
        delta_zero.delta_depth = 0;
        assert!(version_integrity_digest(&delta_zero).is_err());
        let mut delta_deep = delta.clone();
        delta_deep.delta_depth = 257;
        assert!(version_integrity_digest(&delta_deep).is_err());
        let mut delta_empty_parent = delta.clone();
        delta_empty_parent.parent_version_id = Some(String::new());
        assert!(version_integrity_digest(&delta_empty_parent).is_err());
        let mut no_id = good.clone();
        no_id.version_id = String::new();
        assert!(version_integrity_digest(&no_id).is_err());
        let mut bad_entity = good.clone();
        bad_entity.entity = "SH.600006".into();
        assert!(version_integrity_digest(&bad_entity).is_err());
        let mut zero_count = good.clone();
        zero_count.record_count = 0;
        assert!(version_integrity_digest(&zero_count).is_err());
        let mut inverted = good.clone();
        inverted.first_day = 2;
        inverted.last_day = 1;
        assert!(version_integrity_digest(&inverted).is_err());
        let mut daily_lineage = good.clone();
        daily_lineage.yearly_lineage = Some(yearly_lineage());
        assert!(version_integrity_digest(&daily_lineage).is_err());
        let mut year_no_lineage = yearly_identity();
        year_no_lineage.yearly_lineage = None;
        assert!(version_integrity_digest(&year_no_lineage).is_err());
        let mut inverted_lineage = yearly_identity();
        inverted_lineage
            .yearly_lineage
            .as_mut()
            .unwrap()
            .first_trade_day = 400;
        assert!(version_integrity_digest(&inverted_lineage).is_err());
        let mut future_lineage = yearly_identity();
        future_lineage
            .yearly_lineage
            .as_mut()
            .unwrap()
            .last_trade_day = 400;
        assert!(version_integrity_digest(&future_lineage).is_err());
        let mut huge_day = yearly_identity();
        huge_day.yearly_lineage.as_mut().unwrap().as_of_day = i64::MAX;
        assert!(version_integrity_digest(&huge_day).is_err());
        let mut empty_lineage = yearly_identity();
        empty_lineage
            .yearly_lineage
            .as_mut()
            .unwrap()
            .base_daily_version_id = String::new();
        assert!(version_integrity_digest(&empty_lineage).is_err());
        let mut empty_algorithm = yearly_identity();
        empty_algorithm
            .yearly_lineage
            .as_mut()
            .unwrap()
            .algorithm_version = String::new();
        assert!(version_integrity_digest(&empty_algorithm).is_err());
    }

    fn market_identity() -> MarketUpdateResultIdentity {
        MarketUpdateResultIdentity {
            result_id: "result-1".into(),
            task_id: "task-1".into(),
            item_id: "item-1".into(),
            base_version_id: "base-v1".into(),
            result_version_id: "base-v1".into(),
            outcome: UpdateOutcome::NoChange,
            range_start_day: 0,
            range_end_day: 1,
            algorithm_version: "market-update-v1".into(),
            source_binding_digest: [0x11; 32],
            continuity_mode: ContinuityMode::Unchanged,
            prefix_bytes: Some(64),
            base_projection_digest: [0x22; 32],
            result_projection_digest: [0x22; 32],
            delta_digest: None,
            evidence_digest: [0x33; 32],
            task_revision: 7,
        }
    }

    fn publication_identity() -> PublicationResultIdentity {
        PublicationResultIdentity {
            publication_id: "pub-1".into(),
            task_id: "task-1".into(),
            item_id: "item-1".into(),
            publication_kind: PublicationKind::Collection,
            base_version_id: None,
            result_version_id: "result-v1".into(),
            outcome: UpdateOutcome::Published,
            algorithm_version: "collection-v1".into(),
            source_binding_digest: [0x11; 32],
            evidence_digest: [0x33; 32],
            task_revision: 7,
        }
    }

    #[test]
    fn market_update_result_fixed_vector() {
        assert_eq!(
            digest_hex(&market_update_result_digest(&market_identity()).unwrap()),
            "e1f4d956900bd17779ef65615c25ceb5f98613b4a0a58a07d5a15aa3018bd8d9"
        );
    }

    #[test]
    fn publication_result_fixed_vector() {
        assert_eq!(
            digest_hex(&publication_result_digest(&publication_identity()).unwrap()),
            "2261074fdedc5b6b470a2684dbffd4cc7b2ce6cfe2d15fe98b85fc611df0626f"
        );
    }

    #[test]
    fn market_update_result_semantics() {
        let mut append = market_identity();
        append.outcome = UpdateOutcome::Published;
        append.result_version_id = "result-v2".into();
        append.result_projection_digest = [0x44; 32];
        append.delta_digest = Some([0x55; 32]);
        append.continuity_mode = ContinuityMode::Append;
        assert!(market_update_result_digest(&append).is_ok());
        let mut full = append.clone();
        full.continuity_mode = ContinuityMode::Full;
        full.prefix_bytes = None;
        assert!(market_update_result_digest(&full).is_ok());

        let good = market_identity();
        let mut append_no_change = good.clone();
        append_no_change.continuity_mode = ContinuityMode::Append;
        assert!(market_update_result_digest(&append_no_change).is_err());
        let mut full_no_change = good.clone();
        full_no_change.continuity_mode = ContinuityMode::Full;
        assert!(market_update_result_digest(&full_no_change).is_err());
        let mut different_ids = good.clone();
        different_ids.result_version_id = "other".into();
        assert!(market_update_result_digest(&different_ids).is_err());
        let mut no_prefix = good.clone();
        no_prefix.prefix_bytes = None;
        assert!(market_update_result_digest(&no_prefix).is_err());
        let mut different_projection = good.clone();
        different_projection.result_projection_digest = [0x44; 32];
        assert!(market_update_result_digest(&different_projection).is_err());
        let mut with_delta = good.clone();
        with_delta.delta_digest = Some([0x55; 32]);
        assert!(market_update_result_digest(&with_delta).is_err());

        let mut unchanged_published = append.clone();
        unchanged_published.continuity_mode = ContinuityMode::Unchanged;
        assert!(market_update_result_digest(&unchanged_published).is_err());
        let mut same_ids = append.clone();
        same_ids.result_version_id = "base-v1".into();
        assert!(market_update_result_digest(&same_ids).is_err());
        let mut no_delta = append.clone();
        no_delta.delta_digest = None;
        assert!(market_update_result_digest(&no_delta).is_err());
        let mut append_no_prefix = append.clone();
        append_no_prefix.prefix_bytes = None;
        assert!(market_update_result_digest(&append_no_prefix).is_err());
        let mut full_with_prefix = append.clone();
        full_with_prefix.continuity_mode = ContinuityMode::Full;
        assert!(market_update_result_digest(&full_with_prefix).is_err());

        let mut empty_id = good.clone();
        empty_id.result_id = String::new();
        assert!(market_update_result_digest(&empty_id).is_err());
        let mut empty_algorithm = good.clone();
        empty_algorithm.algorithm_version = String::new();
        assert!(market_update_result_digest(&empty_algorithm).is_err());
        let mut zero_revision = good.clone();
        zero_revision.task_revision = 0;
        assert!(market_update_result_digest(&zero_revision).is_err());
        let mut inverted = good.clone();
        inverted.range_start_day = 5;
        inverted.range_end_day = 1;
        assert!(market_update_result_digest(&inverted).is_err());
        let mut huge_day = good.clone();
        huge_day.range_end_day = i64::MAX;
        assert!(market_update_result_digest(&huge_day).is_err());
    }

    #[test]
    fn publication_result_semantics() {
        let mut no_change = publication_identity();
        no_change.outcome = UpdateOutcome::NoChange;
        no_change.base_version_id = Some("result-v1".into());
        assert!(publication_result_digest(&no_change).is_ok());
        let mut published_with_base = publication_identity();
        published_with_base.base_version_id = Some("base-v1".into());
        assert!(publication_result_digest(&published_with_base).is_ok());

        let mut no_base = no_change.clone();
        no_base.base_version_id = None;
        assert!(publication_result_digest(&no_base).is_err());
        let mut different_base = no_change.clone();
        different_base.base_version_id = Some("other".into());
        assert!(publication_result_digest(&different_base).is_err());
        let mut published_same = publication_identity();
        published_same.base_version_id = Some("result-v1".into());
        assert!(publication_result_digest(&published_same).is_err());

        let good = publication_identity();
        let mut empty_id = good.clone();
        empty_id.publication_id = String::new();
        assert!(publication_result_digest(&empty_id).is_err());
        let mut empty_base = good.clone();
        empty_base.base_version_id = Some(String::new());
        assert!(publication_result_digest(&empty_base).is_err());
        let mut zero_revision = good;
        zero_revision.task_revision = 0;
        assert!(publication_result_digest(&zero_revision).is_err());
    }

    #[test]
    fn full_revision_fixed_vector() {
        assert_eq!(
            digest_hex(&full_revision_digest([0x11; 32], [0x22; 32])),
            "c03ce395eaedb1f2c92f7c7197fca6193f46cb09a4c2f78806e23ca11b7a9f3d"
        );
        let digest = full_revision_digest([0x11; 32], [0x22; 32]);
        assert_ne!(full_revision_digest([0x12; 32], [0x22; 32]), digest);
        assert_ne!(full_revision_digest([0x11; 32], [0x23; 32]), digest);
        assert_ne!(full_revision_digest([0x22; 32], [0x11; 32]), digest);
    }
}
