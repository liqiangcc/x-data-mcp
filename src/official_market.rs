//! Inspected official-file snapshot source for QFQ daily market data
//! (issue #3 slice). Existence of this source does NOT lift the
//! `official_files::PRODUCTION_BLOCKERS` gates: construction requires an
//! `inspect()`-passed snapshot supplied by the host, and nothing in the
//! production service configures one. Published records keep the
//! NOT_PRODUCTION_READY marking in their evidence reasons.
//!
//! Verified domain only: `.day` member raw OHLC plus `category == 1` GBBQ
//! records through `qfq::forward_adjust`. Other GBBQ categories are never
//! interpreted or applied (the frozen oracle reproduces official client
//! output from category-1 alone); their exclusion is counted into the
//! published evidence reason. `.day` amount/volume units are anchored by
//! #239 TdxQuant oracle evidence (shares / yuan) and promoted into published
//! bars verbatim — prices only are QFQ-adjusted; amount/volume are never
//! adjusted. The eighth `.day` field (`reserved`) stays unpromoted: its
//! A-share ex-basis semantics are only evidenced for 1999–2010 (#240).
use crate::{
    day_file, gbbq,
    market_data::{
        Adjustment, AdjustmentEvidence, Bar, CoverageEvidence, EvidenceValue, MarketDataBatch,
        MarketDataPipeline, Period, Provenance, SnapshotInputBinding, SourceError,
        UnitCurrencyEvidence,
    },
    market_storage::{
        Error, MarketRepository, Publication, PublishIntent, Publisher, Receipt, Scope,
        compat::CheckedRecord,
    },
    market_update::{
        DateRange, Evidence,
        publication::CombinedEvidence,
        runner::{Acquisition, CombinedCandidate, ItemExecution, RangeProvider},
    },
    publish::{self, EffectiveVersion},
    qfq,
    snapshot::{self, FileInput, FileRole},
    task_control::{Change, ItemOutcome, Stage, Status, TaskSnapshot},
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::Value;
use std::{collections::BTreeMap, io::Read, path::PathBuf, str::FromStr};

pub const SOURCE_NAME: &str = "tdx-official-file-snapshot";
pub const ADAPTER_VERSION: &str = "tdx-market-source-v1";
pub const NOT_PRODUCTION_READY: &str =
    "NOT_PRODUCTION_READY: official_files::PRODUCTION_BLOCKERS gates unchanged";
const GBBQ_MEMBER: &str = "gbbq";
/// Largest manifest byte count the installer will read; the pinned snapshot
/// schema carries exactly three file entries and stays far below this.
const MANIFEST_LIMIT: u64 = 1 << 20;

#[derive(Debug, Clone)]
pub struct TdxMarketSource {
    root: PathBuf,
    manifest_sha256: String,
    day: FileInput,
    gbbq: FileInput,
}

impl TdxMarketSource {
    /// Construction is the gate: only an `inspect()`-passed manifest may form
    /// a source. Any inspection failure is `NotVerified`, never a fallback.
    pub fn inspect(
        root: impl Into<PathBuf>,
        manifest: &[u8],
        now: DateTime<Utc>,
    ) -> Result<Self, SourceError> {
        let root = root.into();
        let inspection = snapshot::inspect(manifest, &root, now)
            .map_err(|e| SourceError::NotVerified(format!("snapshot inspection failed: {e}")))?;
        if inspection.production_ready {
            return Err(SourceError::NotVerified(
                "inspection must not approve production readiness".into(),
            ));
        }
        let take = |role| {
            inspection
                .files
                .iter()
                .find(|f| f.role == role)
                .cloned()
                .ok_or_else(|| SourceError::NotVerified("inspected role missing".into()))
        };
        Ok(Self {
            root,
            manifest_sha256: inspection.manifest_sha256,
            day: take(FileRole::Day)?,
            gbbq: take(FileRole::Gbbq)?,
        })
    }

    /// Service install hook: only when `X_DATA_MCP_OFFICIAL_MANIFEST`
    /// (manifest file) and `X_DATA_MCP_OFFICIAL_DIR` (snapshot root) are both
    /// configured AND the manifest passes `inspect()` does a source exist.
    /// Absent or uninspectable configuration yields `None`, leaving the
    /// market.daily item on the `BlockedSource` refusal path.
    pub fn from_env() -> Option<Self> {
        let manifest = std::env::var_os("X_DATA_MCP_OFFICIAL_MANIFEST")?;
        let root = std::env::var_os("X_DATA_MCP_OFFICIAL_DIR")?;
        let mut bytes = Vec::new();
        std::fs::File::open(manifest)
            .ok()?
            .take(MANIFEST_LIMIT + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > MANIFEST_LIMIT {
            return None;
        }
        Self::inspect(root, &bytes, Utc::now()).ok()
    }

    /// Byte-verified snapshot binding carried into version provenance.
    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }

    /// Read exactly one archive member. Absent or ambiguous members and any
    /// decompression/CRC failure are source failures; the manifest-pinned
    /// archive digest is what `inspect()` already verified.
    fn member_bytes(
        &self,
        input: &FileInput,
        candidates: &[String],
    ) -> Result<(String, Vec<u8>), SourceError> {
        let archive = self
            .root
            .join(&input.relative_path)
            .canonicalize()
            .map_err(|e| SourceError::Invocation(format!("archive unavailable: {e}")))?;
        if !archive.starts_with(&self.root) {
            return Err(SourceError::Invocation(
                "archive resolves outside inspected root".into(),
            ));
        }
        let file = std::fs::File::open(&archive)
            .map_err(|e| SourceError::Invocation(format!("archive open failed: {e}")))?;
        let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
            .map_err(|e| SourceError::Invocation(format!("archive is not a valid zip: {e}")))?;
        let found: Vec<String> = zip
            .file_names()
            .filter(|name| candidates.iter().any(|c| c == name))
            .map(str::to_owned)
            .collect();
        let [member] = found.as_slice() else {
            return Err(SourceError::Invocation(format!(
                "member absent or ambiguous: expected exactly one of {candidates:?}, found {}",
                found.len()
            )));
        };
        let mut entry = zip
            .by_name(member)
            .map_err(|e| SourceError::Invocation(format!("member open failed: {e}")))?;
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut bytes)
            .map_err(|e| SourceError::Invocation(format!("member bytes failed CRC: {e}")))?;
        Ok((member.clone(), bytes))
    }

    /// Open a shared-decode session over this source: the gbbq member is
    /// read, CRC-verified and decoded at most once (lazily, on the first
    /// materialization), so a multi-symbol worker run pays one decode
    /// instead of one per symbol. The session borrows the source — there
    /// is no global or cross-process cache, and a changed snapshot can
    /// only enter through a newly inspected source instance, which yields
    /// a new session.
    pub fn materialize_session(&self) -> MaterializeSession<'_> {
        MaterializeSession {
            source: self,
            gbbq: std::cell::OnceCell::new(),
        }
    }

    /// Materialize the snapshot's complete `.day` history for one security
    /// as forward-adjusted (QFQ) bars. Identical to running the materialize
    /// on a fresh `materialize_session()` — the single-symbol path simply
    /// pays the one gbbq decode itself.
    pub fn materialize_daily(&self, symbol: &str) -> Result<MarketDataBatch, SourceError> {
        self.materialize_session().materialize_daily(symbol)
    }
}

/// The one-time gbbq member decode shared by a `MaterializeSession`:
/// member name and digest are kept for per-batch provenance binding.
struct DecodedGbbq {
    member: String,
    sha256: String,
    events: Vec<gbbq::Event>,
}

/// Per-source materialization channel; see `TdxMarketSource::materialize_session`.
pub struct MaterializeSession<'a> {
    source: &'a TdxMarketSource,
    gbbq: std::cell::OnceCell<DecodedGbbq>,
}

impl MaterializeSession<'_> {
    /// The inspected source this session decodes against.
    pub fn source(&self) -> &TdxMarketSource {
        self.source
    }

    /// Same output as `TdxMarketSource::materialize_daily`: the `.day`
    /// member is re-read per symbol while the verified gbbq member bytes
    /// are decoded once and shared.
    pub fn materialize_daily(&self, symbol: &str) -> Result<MarketDataBatch, SourceError> {
        let source = self.source;
        let Some((market, code)) = symbol.split_once('.').filter(|(m, c)| {
            matches!(*m, "sh" | "sz") && c.len() == 6 && c.bytes().all(|b| b.is_ascii_digit())
        }) else {
            return Err(SourceError::Unsupported(format!(
                "unsupported symbol {symbol:?}; expected sh|sz + six digits"
            )));
        };
        let day_names = [
            format!("{market}\\lday\\{market}{code}.day"),
            format!("{market}/lday/{market}{code}.day"),
        ];
        let (day_member, day_bytes) = source.member_bytes(&source.day, &day_names)?;
        let raw_bars = day_file::parse(&day_bytes)
            .map_err(|e| SourceError::InvalidResponse(format!(".day member malformed: {e}")))?;
        if raw_bars.is_empty() {
            return Err(SourceError::InvalidResponse(
                ".day member carries no records".into(),
            ));
        }
        if self.gbbq.get().is_none() {
            let (member, bytes) = source.member_bytes(&source.gbbq, &[GBBQ_MEMBER.into()])?;
            let raw = gbbq::parse(&bytes)
                .map_err(|e| SourceError::InvalidResponse(format!("gbbq member malformed: {e}")))?;
            let events = gbbq::decode_events(&raw).map_err(|e| {
                SourceError::InvalidResponse(format!("gbbq record decode failed: {e}"))
            })?;
            let _ = self.gbbq.set(DecodedGbbq {
                member,
                sha256: crate::hash::sha256(&bytes),
                events,
            });
        }
        let gbbq = self.gbbq.get().expect("initialized above");
        let market_byte: u8 = if market == "sh" { 1 } else { 0 };
        let mut events = Vec::new();
        let mut excluded = 0usize;
        for event in &gbbq.events {
            if event.market != market_byte || event.code != code {
                continue;
            }
            // Only category 1 has verified QFQ semantics; other categories are
            // never interpreted or applied to prices.
            if event.category == 1 {
                events.push(event.clone());
            } else {
                excluded += 1;
            }
        }
        let adjusted = qfq::forward_adjust(&raw_bars, &events)
            .map_err(|e| SourceError::InvalidResponse(format!("qfq chain failed: {e}")))?;
        let bars: Vec<Bar> = adjusted
            .iter()
            .map(|b| Bar {
                date: b.date,
                open: b.open,
                high: b.high,
                low: b.low,
                close: b.close,
                volume: Some(Decimal::from(b.volume_raw)),
                amount: Decimal::from_str(
                    &f64::from(f32::from_bits(b.amount_float32_bits)).to_string(),
                )
                .ok(),
                r7_raw: Some(b.r7_raw),
            })
            .collect();
        let acquired_at = source
            .day
            .acquisition_completed_at
            .max(source.gbbq.acquisition_completed_at);
        let day_binding = SnapshotInputBinding {
            role: serde_json::to_value(source.day.role)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string(),
            member: day_member,
            archive_sha256: source.day.sha256.clone(),
            member_sha256: crate::hash::sha256(&day_bytes),
        };
        let gbbq_binding = SnapshotInputBinding {
            role: serde_json::to_value(source.gbbq.role)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string(),
            member: gbbq.member.clone(),
            archive_sha256: source.gbbq.sha256.clone(),
            member_sha256: gbbq.sha256.clone(),
        };
        Ok(MarketDataBatch {
            symbol: symbol.into(),
            period: Period::Day,
            adjustment: Adjustment::Forward,
            bars,
            provenance: Provenance {
                source: SOURCE_NAME.into(),
                source_period: "day".into(),
                source_adjustment: "qfq".into(),
                symbol: symbol.into(),
                acquired_at,
                adapter_version: ADAPTER_VERSION.into(),
                yearly_derivation: None,
                snapshot_inputs: Some(vec![day_binding, gbbq_binding]),
            },
            coverage: CoverageEvidence::complete(format!(
                "complete .day member history from inspected snapshot manifest {}; {NOT_PRODUCTION_READY}",
                source.manifest_sha256
            )),
            unit_currency: UnitCurrencyEvidence {
                price_currency: EvidenceValue::confirmed(
                    "CNY",
                    ".day raw OHLC are fen integers of yuan-denominated prices (#166/#167 anchored)",
                ),
                price_unit: EvidenceValue::confirmed(
                    "yuan",
                    ".day raw OHLC are fen integers of yuan-denominated prices (#166/#167 anchored)",
                ),
                volume_unit: EvidenceValue::confirmed(
                    "share",
                    ".day volume is u32 shares; anchored by #239 TdxQuant oracle (37,345-bar full-history match, PR #242); not adjustment-adjusted",
                ),
                amount_currency: EvidenceValue::confirmed(
                    "CNY",
                    ".day amount is f32 yuan; anchored by #239 TdxQuant oracle (37,288/37,345 within tolerance, PR #242); not adjustment-adjusted",
                ),
                amount_unit: EvidenceValue::confirmed(
                    "yuan",
                    ".day amount is f32 yuan; anchored by #239 TdxQuant oracle (37,288/37,345 within tolerance, PR #242); not adjustment-adjusted",
                ),
            },
            adjustment_evidence: AdjustmentEvidence::confirmed(
                "qfq",
                format!(
                    "qfq::forward_adjust over category==1 GBBQ affine chain ({} events, {excluded} non-category-1 records excluded uninterpreted); {NOT_PRODUCTION_READY}",
                    events.len()
                ),
            ),
        })
    }
}

impl MarketDataPipeline for TdxMarketSource {
    fn materialize_daily(
        &self,
        symbol: &str,
        adjustment: Adjustment,
    ) -> Result<MarketDataBatch, SourceError> {
        match adjustment {
            Adjustment::Forward => Self::materialize_daily(self, symbol),
        }
    }

    fn derive_yearly(&self, _daily: &EffectiveVersion) -> Result<MarketDataBatch, SourceError> {
        Err(SourceError::Unsupported(
            "yearly is derived from published daily by the yearly path, not a source fetch".into(),
        ))
    }
}

/// Range-update provider for `update_data`/`market.daily` (#183): re-reads
/// the fixed security's complete QFQ history from the installed inspected
/// snapshot and delivers the accepted range as the verified segment.
///
/// `combined` stays `None`: with a single snapshot generation there is no
/// independent whole-range coverage evidence, so any changed result fails
/// closed at `NEEDS_VERIFICATION` — an honest limit, not a gap. The
/// materialized coverage evidence is kept verbatim; the accepted range and
/// the snapshot binding are carried by the `Evidence` instead (a differing
/// reason would mark every update `metadata_changed` and force
/// `NEEDS_VERIFICATION` even for an identical segment).
///
/// `backfill_data` never reaches this provider: its frozen gap-only
/// semantics differ from refresh, so the worker keeps it on the blocked
/// provider rather than letting it borrow the refresh path.
pub struct TdxRangeProvider<'a> {
    pub source: &'a TdxMarketSource,
}

impl RangeProvider for TdxRangeProvider<'_> {
    fn acquire(
        &self,
        base: &CheckedRecord,
        range: &DateRange,
        _candidate_id: &str,
    ) -> Result<Acquisition, SourceError> {
        let mut batch = self.source.materialize_daily(&base.version().symbol)?;
        batch
            .bars
            .retain(|b| range.start <= b.date && b.date <= range.end);
        if batch.bars.is_empty() {
            return Err(SourceError::InvalidResponse(
                "snapshot carries no bars inside the accepted range".into(),
            ));
        }
        let gbbq = batch
            .provenance
            .snapshot_inputs
            .as_ref()
            .and_then(|inputs| inputs.iter().find(|i| i.role == "gbbq"))
            .ok_or_else(|| {
                SourceError::InvalidResponse("snapshot binding lacks the gbbq member".into())
            })?;
        Ok(Acquisition {
            evidence: Evidence {
                base_version_id: base.version().version_id.clone(),
                base_integrity_sha256: base.raw()["integrity_sha256"]
                    .as_str()
                    .ok_or_else(|| {
                        SourceError::NotVerified("fixed base carries no integrity digest".into())
                    })?
                    .into(),
                candidate_sha256: crate::market_update::candidate_digest(&batch)
                    .map_err(|e| SourceError::InvalidResponse(e.to_string()))?,
                range: range.clone(),
                // Same-QFQ-basis proof: both the base version and this
                // segment derive from the same manifest-pinned snapshot and
                // the same gbbq member bytes.
                same_qfq_basis_ref: format!(
                    "inspected snapshot manifest {} + gbbq member {}",
                    self.source.manifest_sha256(),
                    gbbq.member_sha256
                ),
                revision_ref: None,
            },
            batch,
            combined: None,
        })
    }
}

/// Shared acquisition for providers that build a trusted combined
/// candidate: the fixed base merged with real source bars inside the
/// accepted range, outside-range base values preserved verbatim — the
/// same merge `market_update::prepare` approves before `prepare_range`
/// re-checks the frozen inputs.
///
/// The base must provably derive from the installed snapshot generation
/// (same pinned member/archive digests = same QFQ basis). An overlapping
/// in-range bar the source contradicts on any known value is a basis
/// conflict, not a fill — the merge fails closed instead of overwriting;
/// filling an unknown optional field is enrichment, not a correction
/// (#4 AC-71). The combined record claims `Complete` coverage only when
/// the merged records equal the full materialized source history and
/// otherwise carries the base coverage forward.
///
/// `revision_ref` is the caller's durable revision-authorization text:
/// the backfill provider binds the frozen proven-gap contract; the
/// republish provider passes `None` so `prepare` independently rejects
/// any revised known value — republish expects pure enrichment only.
fn acquire_merged_segment(
    source: &TdxMarketSource,
    base: &CheckedRecord,
    range: &DateRange,
    candidate_id: &str,
    published_at: DateTime<Utc>,
    revision_ref: Option<String>,
) -> Result<Acquisition, SourceError> {
    let batch = source.materialize_daily(&base.version().symbol)?;
    // Same pinned snapshot = same QFQ basis: the base's declared
    // snapshot inputs must equal the member/archive digests this
    // installed source actually read.
    if base.batch.provenance.source != SOURCE_NAME
        || base.batch.provenance.source_adjustment != "qfq"
        || base.batch.provenance.snapshot_inputs != batch.provenance.snapshot_inputs
    {
        return Err(SourceError::NotVerified(
            "base does not provably derive from the installed snapshot".into(),
        ));
    }
    let inputs = batch.provenance.snapshot_inputs.as_deref().unwrap_or(&[]);
    let day = inputs.iter().find(|i| i.role == "day").ok_or_else(|| {
        SourceError::InvalidResponse("snapshot binding lacks the day member".into())
    })?;
    let gbbq = inputs.iter().find(|i| i.role == "gbbq").ok_or_else(|| {
        SourceError::InvalidResponse("snapshot binding lacks the gbbq member".into())
    })?;
    // The segment is the real source bars inside the accepted union of
    // frozen proven-gap ranges; an empty segment cannot fill anything.
    let mut segment = batch.clone();
    segment
        .bars
        .retain(|b| range.start <= b.date && b.date <= range.end);
    if segment.bars.is_empty() {
        return Err(SourceError::InvalidResponse(
            "snapshot carries no bars inside the frozen gap range".into(),
        ));
    }
    let base_integrity: String = base.raw()["integrity_sha256"]
        .as_str()
        .ok_or_else(|| SourceError::NotVerified("fixed base carries no integrity digest".into()))?
        .into();
    // Mirror the merge `market_update::prepare` approves: base records
    // overlaid by in-range source bars; everything outside the range is
    // preserved verbatim. A proven gap only authorizes filling dates the
    // base lacks — an overlapping bar the source contradicts is a basis
    // conflict, not a gap, so fail closed instead of overwriting it.
    let mut merged: BTreeMap<_, _> = base
        .version()
        .records
        .iter()
        .map(|b| (b.date, b.clone()))
        .collect();
    for bar in &segment.bars {
        if let Some(existing) = merged.get(&bar.date) {
            // Same rule as `market_update::prepare` (#4 AC-71): filling an
            // unknown optional field is enrichment, not a correction — a
            // conflict exists only when a known value differs.
            let changed_known = existing.open != bar.open
                || existing.high != bar.high
                || existing.low != bar.low
                || existing.close != bar.close
                || existing.volume.zip(bar.volume).is_some_and(|(a, b)| a != b)
                || existing.amount.zip(bar.amount).is_some_and(|(a, b)| a != b)
                || existing.r7_raw.zip(bar.r7_raw).is_some_and(|(a, b)| a != b);
            let lost_known = (existing.volume.is_some() && bar.volume.is_none())
                || (existing.amount.is_some() && bar.amount.is_none())
                || (existing.r7_raw.is_some() && bar.r7_raw.is_none());
            if changed_known || lost_known {
                return Err(SourceError::NotVerified(format!(
                    "source bar {} conflicts with the fixed base",
                    bar.date
                )));
            }
        }
        merged.insert(bar.date, bar.clone());
    }
    let mut combined_batch = batch.clone();
    combined_batch.bars = merged.into_values().collect();
    combined_batch.coverage = if combined_batch.bars == batch.bars {
        CoverageEvidence::complete(format!(
            "merged record equals the full .day member history of inspected snapshot manifest {}; {NOT_PRODUCTION_READY}",
            source.manifest_sha256()
        ))
    } else {
        base.batch.coverage.clone()
    };
    let (records, validation) = publish::validate_and_deduplicate(&combined_batch)
        .map_err(|e| SourceError::InvalidResponse(e.to_string()))?;
    let mut raw = serde_json::to_value(EffectiveVersion {
        version_id: candidate_id.into(),
        symbol: combined_batch.symbol.clone(),
        period: combined_batch.period,
        adjustment: combined_batch.adjustment,
        source: combined_batch.provenance.source.clone(),
        records,
        published_at,
        validation,
        unit_currency: combined_batch.unit_currency.clone(),
        adjustment_evidence: combined_batch.adjustment_evidence.clone(),
        yearly_derivation: None,
    })
    .map_err(|e| SourceError::InvalidResponse(e.to_string()))?;
    raw["batch"] = serde_json::to_value(&combined_batch)
        .map_err(|e| SourceError::InvalidResponse(e.to_string()))?;
    raw["integrity_sha256"] = Value::String(
        crate::market_storage::compat::legacy_digest(&raw)
            .map_err(|e| SourceError::InvalidResponse(e.to_string()))?,
    );
    let segment_sha256 = crate::market_update::candidate_digest(&segment)
        .map_err(|e| SourceError::InvalidResponse(e.to_string()))?;
    Ok(Acquisition {
        batch: segment,
        evidence: Evidence {
            base_version_id: base.version().version_id.clone(),
            base_integrity_sha256: base_integrity.clone(),
            candidate_sha256: segment_sha256.clone(),
            range: range.clone(),
            same_qfq_basis_ref: format!(
                "inspected snapshot manifest {} + gbbq member {}",
                source.manifest_sha256(),
                gbbq.member_sha256
            ),
            revision_ref,
        },
        combined: Some(CombinedCandidate {
            evidence: CombinedEvidence {
                base_integrity_sha256: base_integrity,
                segment_sha256,
                combined_integrity_sha256: raw["integrity_sha256"]
                    .as_str()
                    .expect("just set")
                    .into(),
                complete_coverage_ref: format!(
                    "inspected snapshot manifest {} + day member {}:{} + gbbq member {}:{}",
                    source.manifest_sha256(),
                    day.member,
                    day.member_sha256,
                    gbbq.member,
                    gbbq.member_sha256
                ),
            },
            record: raw,
        }),
    })
}

/// #187 Phase C provider for `backfill_data` under the default frozen-gap
/// contract only — never for explicit ranges and never for `update_data`
/// refresh. Unlike `TdxRangeProvider`, it builds the trusted combined
/// candidate a changed publication requires through
/// `acquire_merged_segment`. `revision_ref` binds what `acquire` can see
/// (base identity + accepted range + candidate id); the frozen proven-gap
/// ids that authorize in-range value changes live in the task's frozen
/// inputs, are re-checked by `prepare_range`'s frozen-inputs match, and
/// are not visible to this layer — the reference says so rather than
/// naming ids it cannot read.
pub struct TdxBackfillProvider<'a> {
    pub source: &'a TdxMarketSource,
    pub published_at: DateTime<Utc>,
}

impl RangeProvider for TdxBackfillProvider<'_> {
    fn acquire(
        &self,
        base: &CheckedRecord,
        range: &DateRange,
        candidate_id: &str,
    ) -> Result<Acquisition, SourceError> {
        acquire_merged_segment(
            self.source,
            base,
            range,
            candidate_id,
            self.published_at,
            Some(format!(
                "frozen proven-gap authorization checked by the default-contract frozen inputs; base {} range {}..{} candidate {}",
                base.version().version_id,
                range.start,
                range.end,
                candidate_id
            )),
        )
    }
}

/// #247 provider for `republish_data` under the explicit republish
/// contract only — never for `update_data` refresh and never under the
/// default frozen-gap contract. Same trusted combined candidate as the
/// backfill path (fixed base merged with real in-range source bars), but
/// the authorization is the explicit `republish_data` command itself:
/// `revision_ref` stays `None`, so `market_update::prepare`
/// independently rejects any revised known value — a republish expects
/// pure enrichment (filling previously unpromoted optional fields) or
/// new dates only, never a correction.
pub struct TdxRepublishProvider<'a> {
    pub source: &'a TdxMarketSource,
    pub published_at: DateTime<Utc>,
}

impl RangeProvider for TdxRepublishProvider<'_> {
    fn acquire(
        &self,
        base: &CheckedRecord,
        range: &DateRange,
        candidate_id: &str,
    ) -> Result<Acquisition, SourceError> {
        acquire_merged_segment(
            self.source,
            base,
            range,
            candidate_id,
            self.published_at,
            None,
        )
    }
}

/// Repository surface for collection publication: generic commit plus the
/// reads the runner needs for replay, head preconditions and task transitions.
pub trait DailyCollectionRepository: MarketRepository {
    /// Receipt of a committed publication for one item, if any.
    fn collection_receipt(&self, task: &str, item: &str) -> Result<Option<Receipt>, Error>;
    /// Current effective head for the scope (None before first publish).
    fn effective_head(&self, scope: &Scope) -> Result<Option<String>, Error>;
    fn transition_task(
        &self,
        task: &str,
        revision: u64,
        run: &str,
        change: Change,
    ) -> Result<TaskSnapshot, Error>;
}

/// Item runner for `start_collection`/`market.daily`/`v1` — the only item
/// kind this source replaces `BlockedSource` for. Lifecycle mirrors
/// `RangeRunner`: claim, start, materialize, stage, publish via
/// `Publisher::prepare`/`commit` (which finishes the item atomically), then
/// finish execution. No validation step is bypassed.
pub struct TdxDailyRunner<'a, R> {
    pub source: &'a TdxMarketSource,
    /// Shared-decode channel for a multi-item run: one gbbq member decode
    /// serves every item. `None` materializes each item through the plain
    /// single-symbol path; the two produce identical batches.
    pub session: Option<&'a MaterializeSession<'a>>,
    pub repository: R,
    pub run_id: String,
    pub published_at: DateTime<Utc>,
}
impl<'a, R: DailyCollectionRepository + Clone> TdxDailyRunner<'a, R> {
    pub fn run_item(&self, task_id: &str, item_id: &str) -> Result<ItemExecution, Error> {
        let mut task = self.repository.task(task_id)?;
        if task.task.key.operation != "start_collection"
            || task.task.key.dataset != "market.daily"
            || task.task.key.contract_version != "v1"
        {
            return Err(Error::Invalid("unsupported collection contract".into()));
        }
        let item = task
            .items
            .iter()
            .find(|i| i.spec.item_id == item_id)
            .cloned()
            .ok_or_else(|| Error::Invalid("collection item absent from accepted task".into()))?;
        if let Some(receipt) = self.repository.collection_receipt(task_id, item_id)? {
            return Ok(ItemExecution::Published(receipt));
        }
        if task.visible_status(&self.run_id) == Status::NeedsVerification {
            return Err(Error::Task(crate::task_control::Error::NeedsVerification(
                "old run or unresolved execution".into(),
            )));
        }
        if item.stage == Stage::Running {
            return Err(Error::OutcomeUnknown(
                "running item has no verified result; inspect before any retry".into(),
            ));
        }
        if task.task.status.terminal() || item.stage == Stage::Finished {
            if item.outcome.as_ref().is_some_and(|o| o.success) {
                return Err(Error::Integrity(
                    "successful collection item has no receipt".into(),
                ));
            }
            return Ok(ItemExecution::NotPublished(Box::new(task)));
        }
        if task.task.status == Status::CancelRequested {
            let next = self.repository.transition_task(
                task_id,
                task.task.state_revision,
                &self.run_id,
                Change::FinishExecution,
            )?;
            return Ok(ItemExecution::NotPublished(Box::new(next)));
        }
        if task.task.status == Status::Pending {
            task = self.repository.transition_task(
                task_id,
                task.task.state_revision,
                &self.run_id,
                Change::Claim,
            )?;
        }
        task = self.repository.transition_task(
            task_id,
            task.task.state_revision,
            &self.run_id,
            Change::StartItem(item_id.into()),
        )?;
        let outcome = self.materialize_and_commit(&task, item_id, &item.spec.target);
        match outcome {
            Ok(receipt) => Ok(ItemExecution::Published(receipt)),
            Err(error) => {
                // A response can be lost after commit. Never overwrite success.
                if let Some(receipt) = self.repository.collection_receipt(task_id, item_id)? {
                    return Ok(ItemExecution::Published(receipt));
                }
                if matches!(
                    error,
                    Error::OutcomeUnknown(_)
                        | Error::Storage(_)
                        | Error::Task(
                            crate::task_control::Error::StorageUnavailable(_)
                                | crate::task_control::Error::OutcomeUnknown(_)
                        )
                ) {
                    let current = self.repository.task(task_id)?;
                    if let Err(recording) = self.repository.transition_task(
                        task_id, current.task.state_revision, &self.run_id,
                        Change::RequireVerification {
                            item_id: item_id.into(),
                            reason: "item execution result not confirmed after materialization/publication; committed receipt not yet established".into(),
                        },
                    ) {
                        if let Some(receipt) =
                            self.repository.collection_receipt(task_id, item_id)?
                        {
                            return Ok(ItemExecution::Published(receipt));
                        }
                        return Err(recording);
                    }
                    return Err(error);
                }
                let current = self.repository.task(task_id)?;
                let cancelled_without_publication = matches!(error, Error::Conflict(_))
                    && current.task.status == Status::CancelRequested;
                let next = self.repository.transition_task(
                    task_id,
                    current.task.state_revision,
                    &self.run_id,
                    Change::FinishItem(
                        item_id.into(),
                        ItemOutcome {
                            success: false,
                            error: Some(if cancelled_without_publication {
                                format!("cancelled before publication: {error}")
                            } else {
                                error.to_string()
                            }),
                            evidence: None,
                        },
                    ),
                )?;
                let next = if next.items.iter().all(|i| i.stage == Stage::Finished)
                    || next.task.status == Status::CancelRequested
                {
                    self.repository.transition_task(
                        task_id,
                        next.task.state_revision,
                        &self.run_id,
                        if cancelled_without_publication {
                            Change::FinishCancellation
                        } else {
                            Change::FinishExecution
                        },
                    )?
                } else {
                    next
                };
                Ok(ItemExecution::NotPublished(Box::new(next)))
            }
        }
    }
    /// Source failure preserves the old version: nothing is staged or
    /// committed on any materialization/validation error.
    fn materialize_and_commit(
        &self,
        task: &TaskSnapshot,
        item_id: &str,
        target: &Value,
    ) -> Result<Receipt, Error> {
        let symbol = target["symbol"]
            .as_str()
            .ok_or_else(|| Error::Integrity("collection item target missing symbol".into()))?;
        if target["period"] != Value::String("day".into()) {
            return Err(Error::Invalid("collection target is not daily".into()));
        }
        let scope = Scope {
            symbol: symbol.into(),
            period: Period::Day,
        };
        if self.repository.effective_head(&scope)?.is_some() {
            return Err(Error::Invalid(
                "existing daily head requires the update_data range path".into(),
            ));
        }
        let batch = match self.session {
            Some(session) => session.materialize_daily(symbol),
            None => self.source.materialize_daily(symbol),
        }
        .map_err(|e| Error::Invalid(e.to_string()))?;
        let current = self.repository.task(&task.task.task_id)?;
        if current.task.status != Status::Running
            || current.task.state_revision != task.task.state_revision
        {
            return Err(Error::Conflict(
                "task changed or cancelled during materialization".into(),
            ));
        }
        let id = crate::hash::sha256(
            serde_json::json!([task.task.task_id, item_id])
                .to_string()
                .as_bytes(),
        );
        let candidate_id = format!("collection-version-{id}");
        let (records, validation) =
            publish::validate_and_deduplicate(&batch).map_err(|e| Error::Invalid(e.to_string()))?;
        let mut raw = serde_json::to_value(EffectiveVersion {
            version_id: candidate_id.clone(),
            symbol: batch.symbol.clone(),
            period: batch.period,
            adjustment: batch.adjustment,
            source: batch.provenance.source.clone(),
            records,
            published_at: self.published_at,
            validation,
            unit_currency: batch.unit_currency.clone(),
            adjustment_evidence: batch.adjustment_evidence.clone(),
            yearly_derivation: batch.provenance.yearly_derivation.clone(),
        })
        .map_err(|e| Error::Integrity(e.to_string()))?;
        raw["batch"] = serde_json::to_value(&batch).map_err(|e| Error::Integrity(e.to_string()))?;
        raw["integrity_sha256"] = Value::String(
            crate::market_storage::compat::legacy_digest(&raw)
                .map_err(|e| Error::Integrity(e.to_string()))?,
        );
        self.repository.stage(&raw)?;
        let plan = (Publisher {
            repository: self.repository.clone(),
        })
        .prepare(Publication {
            publication_id: format!("collection-result-{id}"),
            version_id: candidate_id,
            task_id: task.task.task_id.clone(),
            item_id: item_id.into(),
            run_id: self.run_id.clone(),
            expected_revision: task.task.state_revision,
            intent: PublishIntent::Daily {
                expected_head: None,
            },
        })?;
        self.repository.commit(&plan)
    }
}

/// Decision-only conclusion: the pinned `.day` member exists but carries no
/// bar for the target day — the source cannot prove a local gap.
const CONCLUSION_SOURCE_LIMITATION: &str = "source_limitation";
/// Decision-only conclusion: the frozen subject already carries the target
/// day, so there is no local gap to prove.
const CONCLUSION_NO_LOCAL_GAP: &str = "no_local_gap";
/// #187 Phase B trusted gap verifier bound to an installed official-file
/// snapshot; implements the sealed #155 `SourceGapVerifier` port. The
/// emitted `GapBasis` is Serialize-only durable evidence, never caller
/// input, and binds the .day member digest plus (inside
/// `artifact_identity`) the manifest digest and gbbq member identity so the
/// conclusion replays only against the pinned snapshot generation.
///
/// `confirmed_missing_range` is emitted only when every binding holds:
/// - the frozen subject is a fully readable published official-file QFQ
///   daily version whose declared snapshot inputs (manifest-pinned archive
///   digests + .day/gbbq member digests) equal the members this installed
///   source actually read — a subject minted from another source or
///   another snapshot generation fails closed;
/// - the source `.day` member contains an exact bar for the target day;
/// - the subject carries no record for the target day;
/// - every date present in both subject and source agrees field-for-field
///   — an overlapping value conflict is an integrity doubt, never a gap.
///
/// A present member lacking the target day produces a `source_limitation`
/// decision (no proven gap); a subject already carrying the day produces
/// `no_local_gap` (no proven gap). Any identity, digest or basis doubt
/// returns `BasisNotVerified` and writes nothing.
pub struct TdxGapVerifier<'a, R> {
    pub source: &'a TdxMarketSource,
    pub repository: R,
    /// Observation instant supplied by the caller's run clock.
    pub observed_at: DateTime<Utc>,
}
impl<R: MarketRepository> crate::gap_verification::SourceGapVerifier for TdxGapVerifier<'_, R> {
    fn verifier_id(&self) -> &str {
        "tdx-official-gap-verifier-v1"
    }
    fn verify(
        &self,
        target: &crate::gap_verification::GapTarget,
        subject: &crate::gap_verification::SubjectBinding,
    ) -> Result<crate::gap_verification::GapBasis, Error> {
        // Bounded slice: one frozen day only — wider targets are refused,
        // never silently reinterpreted as ranges.
        if target.start_date != target.end_date {
            return Err(Error::BasisNotVerified(
                "bounded gap verifier supports single-day targets only".into(),
            ));
        }
        let batch = self.source.materialize_daily(&target.symbol).map_err(|e| {
            Error::BasisNotVerified(format!(
                "official-file source cannot materialize the target symbol: {e}"
            ))
        })?;
        // Re-read the frozen subject: a fully verified published record
        // whose stored digest must equal the frozen binding.
        let record = self.repository.fixed(&subject.version_id)?;
        if record.raw()["integrity_sha256"].as_str() != Some(subject.integrity_sha256.as_str()) {
            return Err(Error::BasisNotVerified(
                "subject digest does not match frozen binding".into(),
            ));
        }
        let version = record.version();
        if version.symbol != target.symbol
            || version.period != Period::Day
            || version.source != SOURCE_NAME
            || version.adjustment != Adjustment::Forward
            || !version.adjustment_evidence.is_confirmed()
            || version.adjustment_evidence.value.as_deref() != Some("qfq")
        {
            return Err(Error::BasisNotVerified(
                "subject is not a pinned official-file QFQ daily version".into(),
            ));
        }
        // Same pinned snapshot = same QFQ basis: the subject's declared
        // snapshot inputs must equal the member/archive digests this
        // installed source actually read.
        if record.batch.provenance.source != SOURCE_NAME
            || record.batch.provenance.source_adjustment != "qfq"
            || record.batch.provenance.snapshot_inputs != batch.provenance.snapshot_inputs
        {
            return Err(Error::BasisNotVerified(
                "subject does not provably derive from the installed snapshot".into(),
            ));
        }
        // Comparable overlap must agree field-for-field; a conflict is an
        // integrity doubt, never a gap.
        let source_bars: BTreeMap<_, &Bar> = batch.bars.iter().map(|b| (b.date, b)).collect();
        for local in &record.batch.bars {
            if let Some(remote) = source_bars.get(&local.date)
                && local != *remote
            {
                return Err(Error::BasisNotVerified(format!(
                    "subject/source value conflict on {}",
                    local.date
                )));
            }
        }
        let conclusion = if !source_bars.contains_key(&target.start_date) {
            CONCLUSION_SOURCE_LIMITATION
        } else if record
            .batch
            .bars
            .iter()
            .any(|b| b.date == target.start_date)
        {
            CONCLUSION_NO_LOCAL_GAP
        } else {
            crate::market_storage::CONFIRMED_MISSING_RANGE
        };
        let inputs = batch.provenance.snapshot_inputs.as_deref().unwrap_or(&[]);
        let day = inputs
            .iter()
            .find(|i| i.role == "day")
            .ok_or_else(|| Error::Integrity("materialized batch missing day binding".into()))?;
        let gbbq = inputs
            .iter()
            .find(|i| i.role == "gbbq")
            .ok_or_else(|| Error::Integrity("materialized batch missing gbbq binding".into()))?;
        let mut basis = crate::gap_verification::GapBasis {
            source_identity: format!("{SOURCE_NAME} manifest {}", self.source.manifest_sha256()),
            source_contract: ADAPTER_VERSION.into(),
            symbol: target.symbol.clone(),
            start_date: target.start_date,
            end_date: target.end_date,
            subject_version_id: subject.version_id.clone(),
            subject_version_digest: subject.integrity_sha256.clone(),
            artifact_identity: format!(
                "{}:{} manifest {} gbbq member {}:{}",
                day.member,
                day.member_sha256,
                self.source.manifest_sha256(),
                gbbq.member,
                gbbq.member_sha256
            ),
            artifact_sha256: day.member_sha256.clone(),
            conclusion: conclusion.into(),
            observed_at: self.observed_at,
            verifier_id: self.verifier_id().into(),
            basis_sha256: String::new(),
        };
        basis.basis_sha256 = basis.content_digest();
        Ok(basis)
    }
}
