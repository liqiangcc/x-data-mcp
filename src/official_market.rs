//! Inspected official-file snapshot source for QFQ daily market data
//! (issue #3 slice). Construction requires an `inspect()`-passed snapshot
//! supplied by the host. All `official_files::PRODUCTION_BLOCKERS` gates are
//! resolved by Final Gate rulings (#54, #131, #71); published records carry
//! the DECLARED_SCOPE note in their evidence reasons — the #71 acceptance is
//! scoped to owner personal research, local-only, no external exposure, and
//! external distribution is not authorized by that scoped acceptance.
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
        Adjustment, AdjustmentEvidence, Bar, CoverageEvidence, CoverageStatus, EvidenceValue,
        MarketDataBatch, MarketDataPipeline, Period, Provenance, SnapshotInputBinding, SourceError,
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
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::{collections::BTreeMap, io::Read, path::PathBuf, str::FromStr};

pub const SOURCE_NAME: &str = "tdx-official-file-snapshot";
pub const ADAPTER_VERSION: &str = "tdx-market-source-v1";
pub const DECLARED_SCOPE: &str = "DECLARED_SCOPE: owner personal research, \
    local-only, no external exposure (#71 comment-5749934485); external \
    distribution is not authorized";
const GBBQ_MEMBER: &str = "gbbq";
/// Snapshot-binding role for the per-security decoded event set: digests
/// the category-1 events actually applied plus the excluded count, so an
/// update can prove event continuity for one symbol even when other
/// securities' records changed the gbbq member as a whole.
pub(crate) const SECURITY_EVENTS_ROLE: &str = "gbbq-security-events";
/// Largest manifest byte count the installer will read; the pinned snapshot
/// schema carries exactly three file entries and stays far below this.
const MANIFEST_LIMIT: u64 = 1 << 20;

/// Canonical digest of one security's interpreted event set: every
/// category-1 event's stored `(date_raw, c_raw)` fields in decode order,
/// followed by the excluded non-category-1 record count. Byte-exact over
/// the decoded records — identical inputs digest identically, and any
/// added/dropped/reordered record fails the continuity check.
fn security_events_digest(events: &[gbbq::Event], excluded: usize) -> String {
    let mut bytes = Vec::with_capacity(events.len() * 20 + 8);
    for event in events {
        bytes.extend_from_slice(&event.date_raw.to_le_bytes());
        for c in event.c_raw {
            bytes.extend_from_slice(&c.to_bits().to_le_bytes());
        }
    }
    bytes.extend_from_slice(&(excluded as u64).to_le_bytes());
    crate::hash::sha256(&bytes)
}

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

    /// Build a source view from an already byte-verified installed
    /// generation. `verify_installed` ran the same `snapshot::inspect`
    /// `inspect()` would repeat on the identical manifest bytes — the same
    /// role/path/digest validation and the same (always false)
    /// `production_ready` verdict — so re-hashing the artifacts here would
    /// be pure duplicate work with no additional gate.
    pub(crate) fn from_installed(
        installed: &crate::generation::Installed,
    ) -> Result<Self, SourceError> {
        let take = |role| {
            installed
                .manifest
                .files
                .iter()
                .find(|f| f.role == role)
                .cloned()
                .ok_or_else(|| SourceError::NotVerified("inspected role missing".into()))
        };
        Ok(Self {
            root: installed.dir.clone(),
            manifest_sha256: installed.manifest_sha256.clone(),
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

    /// Resolve an inspected input to its canonical archive path under the
    /// snapshot root. The containment check is unchanged from the original
    /// per-call `member_bytes` path.
    fn archive_path(&self, input: &FileInput) -> Result<PathBuf, SourceError> {
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
        Ok(archive)
    }

    /// Open a shared-decode session over this source: each archive's
    /// central directory is parsed at most once (lazily, on first member
    /// read) and the gbbq member is read, CRC-verified and decoded at most
    /// once, so a multi-symbol worker run pays one index build and one
    /// decode instead of one per symbol. The session borrows the source —
    /// there is no global or cross-process cache, and a changed snapshot
    /// can only enter through a newly inspected source instance, which
    /// yields a new session.
    pub fn materialize_session(&self) -> MaterializeSession<'_> {
        MaterializeSession {
            source: self,
            gbbq: std::cell::OnceCell::new(),
            archives: std::cell::RefCell::new(std::collections::HashMap::new()),
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

/// A `.day` member read for binding: the resolved member name and the
/// sha256 of its decompressed bytes.
struct DayMemberRead {
    member: String,
    sha256: String,
}

/// One archive's parsed central directory: member name → all entry
/// indices, so duplicate names stay detectable (absent-or-ambiguous is a
/// source failure, unchanged from the per-call path).
struct ArchiveIndex {
    archive: zip::ZipArchive<std::io::BufReader<std::fs::File>>,
    members: std::collections::HashMap<String, Vec<usize>>,
}

impl ArchiveIndex {
    fn open(path: &std::path::Path) -> Result<Self, SourceError> {
        let file = std::fs::File::open(path)
            .map_err(|e| SourceError::Invocation(format!("archive open failed: {e}")))?;
        let archive = zip::ZipArchive::new(std::io::BufReader::new(file))
            .map_err(|e| SourceError::Invocation(format!("archive is not a valid zip: {e}")))?;
        let mut members = std::collections::HashMap::new();
        for (index, name) in archive.file_names().enumerate() {
            members
                .entry(name.to_owned())
                .or_insert_with(Vec::new)
                .push(index);
        }
        Ok(Self { archive, members })
    }
}

/// The one-time gbbq member decode shared by a `MaterializeSession`:
/// member name and digest are kept for per-batch provenance binding, and
/// events are indexed by `(market, code)` once so per-symbol lookup is
/// O(1) instead of a full-table scan.
struct DecodedGbbq {
    member: String,
    sha256: String,
    /// `(market, code)` → (category-1 events, non-category-1 excluded
    /// count); every record is partitioned exactly once at decode time.
    by_security: std::collections::HashMap<(u8, String), (Vec<gbbq::Event>, usize)>,
}

/// Per-source materialization channel; see `TdxMarketSource::materialize_session`.
pub struct MaterializeSession<'a> {
    source: &'a TdxMarketSource,
    gbbq: std::cell::OnceCell<DecodedGbbq>,
    /// Lazily opened central-directory indexes keyed by canonical archive
    /// path; each archive is opened and scanned at most once per session.
    archives: std::cell::RefCell<std::collections::HashMap<PathBuf, ArchiveIndex>>,
}

impl MaterializeSession<'_> {
    /// The inspected source this session decodes against.
    pub fn source(&self) -> &TdxMarketSource {
        self.source
    }

    /// Read exactly one archive member through the session's shared
    /// central-directory index: the archive is opened and its names scanned
    /// at most once per session, and member lookup is O(1). Absent or
    /// ambiguous members and any decompression/CRC failure are source
    /// failures; the manifest-pinned archive digest is what `inspect()`
    /// already verified.
    fn member_bytes(
        &self,
        input: &FileInput,
        candidates: &[String],
    ) -> Result<(String, Vec<u8>), SourceError> {
        let path = self.source.archive_path(input)?;
        let mut archives = self.archives.borrow_mut();
        let index = match archives.entry(path.clone()) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => e.insert(ArchiveIndex::open(&path)?),
        };
        let mut found: Vec<(&String, usize)> = Vec::new();
        for name in candidates {
            if let Some(indices) = index.members.get(name) {
                found.extend(indices.iter().map(|i| (name, *i)));
            }
        }
        let [(member, member_index)] = found.as_slice() else {
            return Err(SourceError::Invocation(format!(
                "member absent or ambiguous: expected exactly one of {candidates:?}, found {}",
                found.len()
            )));
        };
        let mut entry = index
            .archive
            .by_index(*member_index)
            .map_err(|e| SourceError::Invocation(format!("member open failed: {e}")))?;
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut bytes)
            .map_err(|e| SourceError::Invocation(format!("member bytes failed CRC: {e}")))?;
        Ok(((*member).clone(), bytes))
    }

    /// Ensure the gbbq member is decoded and return the shared state.
    /// The decode happens at most once per session; the member bytes were
    /// already CRC-verified by `member_bytes`.
    fn decoded_gbbq(&self) -> Result<&DecodedGbbq, SourceError> {
        if self.gbbq.get().is_none() {
            let (member, bytes) = self.member_bytes(&self.source.gbbq, &[GBBQ_MEMBER.into()])?;
            let raw = gbbq::parse(&bytes)
                .map_err(|e| SourceError::InvalidResponse(format!("gbbq member malformed: {e}")))?;
            let events = gbbq::decode_events(&raw).map_err(|e| {
                SourceError::InvalidResponse(format!("gbbq record decode failed: {e}"))
            })?;
            let mut by_security: std::collections::HashMap<
                (u8, String),
                (Vec<gbbq::Event>, usize),
            > = std::collections::HashMap::new();
            for event in events {
                // Only category 1 has verified QFQ semantics; other
                // categories are never interpreted or applied to prices.
                let entry = by_security
                    .entry((event.market, event.code.clone()))
                    .or_default();
                if event.category == 1 {
                    entry.0.push(event);
                } else {
                    entry.1 += 1;
                }
            }
            let _ = self.gbbq.set(DecodedGbbq {
                member,
                sha256: crate::hash::sha256(&bytes),
                by_security,
            });
        }
        Ok(self.gbbq.get().expect("initialized above"))
    }

    /// The security's interpreted event set: category-1 events in decode
    /// order plus the count of excluded non-category-1 records.
    fn security_events(
        &self,
        market: &str,
        code: &str,
    ) -> Result<(&[gbbq::Event], usize), SourceError> {
        let gbbq = self.decoded_gbbq()?;
        let market_byte: u8 = if market == "sh" { 1 } else { 0 };
        Ok(gbbq
            .by_security
            .get(&(market_byte, code.to_string()))
            .map(|(events, excluded)| (events.as_slice(), *excluded))
            .unwrap_or((&[], 0)))
    }

    /// Candidate member names for a security's `.day` file.
    fn day_names(market: &str, code: &str) -> [String; 2] {
        [
            format!("{market}\\lday\\{market}{code}.day"),
            format!("{market}/lday/{market}{code}.day"),
        ]
    }

    /// Same output as `TdxMarketSource::materialize_daily`: the `.day`
    /// member is re-read per symbol through the shared archive index while
    /// the verified gbbq member bytes are decoded once and shared.
    pub fn materialize_daily(&self, symbol: &str) -> Result<MarketDataBatch, SourceError> {
        let source = self.source;
        let Some((market, code)) = symbol.split_once('.').filter(|(m, c)| {
            matches!(*m, "sh" | "sz") && c.len() == 6 && c.bytes().all(|b| b.is_ascii_digit())
        }) else {
            return Err(SourceError::Unsupported(format!(
                "unsupported symbol {symbol:?}; expected sh|sz + six digits"
            )));
        };
        let (day_member, day_bytes) =
            self.member_bytes(&source.day, &Self::day_names(market, code))?;
        let raw_bars = day_file::parse(&day_bytes)
            .map_err(|e| SourceError::InvalidResponse(format!(".day member malformed: {e}")))?;
        if raw_bars.is_empty() {
            return Err(SourceError::InvalidResponse(
                ".day member carries no records".into(),
            ));
        }
        let (events, excluded) = self.security_events(market, code)?;
        let bars = Self::qfq_bars(&raw_bars, events)?;
        self.assemble_daily(
            symbol,
            DayMemberRead {
                member: day_member,
                sha256: crate::hash::sha256(&day_bytes),
            },
            bars,
            events,
            excluded,
        )
    }

    /// Map verified-raw `.day` records through the QFQ event chain into
    /// publishable bars. Output depends only on each record and the events
    /// strictly after its date, so a proven-identical prefix never needs
    /// re-adjustment.
    fn qfq_bars(
        raw_bars: &[crate::day_file::RawDayRecord],
        events: &[gbbq::Event],
    ) -> Result<Vec<Bar>, SourceError> {
        let adjusted = qfq::forward_adjust(raw_bars, events)
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
        Ok(bars)
    }

    /// Build the snapshot-bound batch shared by full materialization and
    /// the incremental update path: the same bars plus bindings covering
    /// the `.day` member, the gbbq member, and this security's decoded
    /// event set. The per-security binding is what lets a later update
    /// prove event continuity without trusting the whole gbbq table.
    fn assemble_daily(
        &self,
        symbol: &str,
        day: DayMemberRead,
        bars: Vec<Bar>,
        events: &[gbbq::Event],
        excluded: usize,
    ) -> Result<MarketDataBatch, SourceError> {
        let source = self.source;
        let gbbq = self.decoded_gbbq()?;
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
            member: day.member,
            archive_sha256: source.day.sha256.clone(),
            member_sha256: day.sha256,
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
        let events_binding = SnapshotInputBinding {
            role: SECURITY_EVENTS_ROLE.into(),
            member: format!("gbbq#{symbol}"),
            archive_sha256: source.gbbq.sha256.clone(),
            member_sha256: security_events_digest(events, excluded),
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
                snapshot_inputs: Some(vec![day_binding, gbbq_binding, events_binding]),
            },
            coverage: CoverageEvidence::complete(format!(
                "complete .day member history from inspected snapshot manifest {}; {DECLARED_SCOPE}",
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
                    "qfq::forward_adjust over category==1 GBBQ affine chain ({} events, {excluded} non-category-1 records excluded uninterpreted); {DECLARED_SCOPE}",
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
    /// Shared decode/index channel; a single-item caller builds one fresh
    /// session and pays the one-time decode itself.
    pub session: &'a MaterializeSession<'a>,
    /// Acquisition generation bound to this source view. In inspected-env
    /// mode the manifest digest is the only stable generation identity.
    pub generation_id: String,
    /// Commit clock for a changed publication's combined record.
    pub published_at: DateTime<Utc>,
}

/// How the member bytes and event set just read relate to the fixed
/// base's pinned snapshot inputs (#262 incremental update).
enum Continuity {
    /// Member digest and event digest both identical — every published
    /// value is byte-proven unchanged; the base's stored bars are exactly
    /// what re-materialization would produce. The proven prefix is the
    /// whole member.
    Unchanged { prefix_bytes: usize },
    /// The base's member bytes are an exact `prefix_bytes` prefix of the
    /// current member and the event set is unchanged — only the
    /// strictly-newer tail records were parsed and adjusted.
    Append {
        prefix_bytes: usize,
        tail: Vec<day_file::RawDayRecord>,
    },
    /// Digest proof unavailable or contradicted (missing bindings,
    /// changed event set, rewritten/shrunk/overlapping member bytes).
    /// The caller falls back to full materialization; `prepare` then
    /// decides on values alone, so a real revision still fails closed.
    Unverified,
}

impl TdxRangeProvider<'_> {
    /// Read the security's `.day` member and event set through the shared
    /// session once and classify continuity against the fixed base's
    /// snapshot bindings. The member digest and the prefix digest are the
    /// proof: no parsed value is trusted for the classification.
    fn probe(
        &self,
        base: &CheckedRecord,
        market: &str,
        code: &str,
    ) -> Result<(String, String, String, Continuity), SourceError> {
        let inputs = base
            .batch
            .provenance
            .snapshot_inputs
            .as_deref()
            .unwrap_or(&[]);
        let base_day_sha = inputs
            .iter()
            .find(|i| i.role == "day")
            .map(|i| i.member_sha256.as_str());
        let base_events_sha = inputs
            .iter()
            .find(|i| i.role == SECURITY_EVENTS_ROLE)
            .map(|i| i.member_sha256.as_str());
        // The base's member covered exactly its published records — .day
        // records are fixed-width and parse-verified contiguous.
        let prefix_bytes = base.version().records.len() * day_file::RECORD_BYTES;
        let last_base_date = base.version().records.last().map(|b| b.date);
        self.probe_bound(
            base_day_sha,
            base_events_sha,
            prefix_bytes,
            last_base_date,
            market,
            code,
        )
    }

    /// Typed-facts probe for the ops-first update path (#259 S5G): the
    /// same digest classification as `probe`, but bound to the compact
    /// `source_bindings` row instead of the envelope `snapshot_inputs`.
    /// The stored binding is the committed fact — verified at commit
    /// time by `verified_source_binding` — so probing against it is
    /// strictly stronger than trusting envelope metadata.
    fn probe_facts(
        &self,
        facts: &crate::market_update::UpdateBaseFacts,
        market: &str,
        code: &str,
    ) -> Result<(String, String, String, Continuity), SourceError> {
        self.probe_bound(
            Some(facts.day_member_sha256.as_str()),
            Some(facts.security_event_sha256.as_str()),
            facts.record_count as usize * day_file::RECORD_BYTES,
            Some(facts.last_day),
            market,
            code,
        )
    }

    /// Digest-proof continuity classification shared by `probe` (envelope
    /// bindings) and `probe_facts` (compact source bindings).
    fn probe_bound(
        &self,
        base_day_sha: Option<&str>,
        base_events_sha: Option<&str>,
        prefix_bytes: usize,
        last_base_date: Option<NaiveDate>,
        market: &str,
        code: &str,
    ) -> Result<(String, String, String, Continuity), SourceError> {
        let source = self.session.source();
        let (day_member, day_bytes) = self
            .session
            .member_bytes(&source.day, &MaterializeSession::day_names(market, code))?;
        let day_member_sha256 = crate::hash::sha256(&day_bytes);
        let (events, excluded) = self.session.security_events(market, code)?;
        let events_sha256 = security_events_digest(events, excluded);
        let continuity = match (base_day_sha, base_events_sha) {
            (Some(day_sha), Some(ev_sha))
                if ev_sha == events_sha256 && day_sha == day_member_sha256 =>
            {
                Continuity::Unchanged {
                    prefix_bytes: day_bytes.len(),
                }
            }
            (Some(day_sha), Some(ev_sha))
                if ev_sha == events_sha256
                    && day_bytes.len() > prefix_bytes
                    && day_bytes.len().is_multiple_of(day_file::RECORD_BYTES)
                    && crate::hash::sha256(&day_bytes[..prefix_bytes]) == day_sha =>
            {
                // A parse failure here means the member itself is
                // malformed — a source error, not a revision.
                let tail = day_file::parse(&day_bytes[prefix_bytes..]).map_err(|e| {
                    SourceError::InvalidResponse(format!(".day member tail malformed: {e}"))
                })?;
                match (last_base_date, tail.first()) {
                    (Some(last), Some(first)) if first.date > last => {
                        Continuity::Append { prefix_bytes, tail }
                    }
                    // Overlapping or missing tail records mean rewritten
                    // history — no continuity proof.
                    _ => Continuity::Unverified,
                }
            }
            _ => Continuity::Unverified,
        };
        Ok((day_member, day_member_sha256, events_sha256, continuity))
    }

    /// Serialize the merged update candidate into an immutable version
    /// record and bind the evidence chain the publication gate checks:
    /// base integrity → segment digest → combined record integrity, plus
    /// the snapshot inputs that claim complete coverage.
    fn combined_candidate(
        &self,
        ctx: &ContinuityCtx<'_>,
        base: &CheckedRecord,
        batch: &MarketDataBatch,
        segment: &MarketDataBatch,
        merged: Vec<Bar>,
        full_revision: bool,
    ) -> Result<CombinedCandidate, SourceError> {
        let mut combined_batch = batch.clone();
        combined_batch.bars = merged;
        combined_batch.coverage = if combined_batch.bars == batch.bars {
            // Merged == full materialized history: carry the same
            // coverage claim a fresh materialization makes so the stored
            // metadata stays stable for the next update.
            batch.coverage.clone()
        } else {
            base.batch.coverage.clone()
        };
        let raw = combined_version_record(
            &combined_batch,
            ctx.candidate_id,
            self.published_at,
            Some(staged_compact_fields(
                self.session.source(),
                &self.generation_id,
                &combined_batch,
                // A full revision stages as a root checkpoint: the base→result
                // chain is carried by the update result's digest, not by a
                // version parent (compact `ContinuityMode::Full` contract).
                (!full_revision).then_some(base.version().version_id.as_str()),
            )?),
        )?;
        let segment_sha256 = crate::market_update::candidate_digest(segment)
            .map_err(|e| SourceError::InvalidResponse(e.to_string()))?;
        Ok(CombinedCandidate {
            evidence: CombinedEvidence {
                base_integrity_sha256: ctx.base_integrity.into(),
                segment_sha256,
                combined_integrity_sha256: raw["integrity_sha256"]
                    .as_str()
                    .expect("just set")
                    .into(),
                complete_coverage_ref: format!(
                    "inspected snapshot manifest {} + day member {}:{} + gbbq member {} + security events {}",
                    self.session.source().manifest_sha256(),
                    ctx.day_member,
                    ctx.day_member_sha256,
                    self.session.source().gbbq.sha256,
                    ctx.events_sha256,
                ),
            },
            record: raw,
        })
    }

    /// Cross-generation append metadata the typed delta can carry (#320):
    /// same complete-coverage class and no unit semantics drift, but a
    /// new manifest-pinned source binding plus the coverage claim the
    /// envelope merge would publish. Anything
    /// stronger (status drift or unit conflict/enrichment) still returns
    /// `None` so the envelope path reproduces its full decision.
    fn typed_append_metadata(
        &self,
        facts: &crate::market_update::UpdateBaseFacts,
        segment: &MarketDataBatch,
        result_coverage: &CoverageEvidence,
        day_member_sha256: &str,
        events_sha256: &str,
    ) -> Result<Option<crate::market_update::TypedVersionMetadata>, SourceError> {
        if result_coverage.status != CoverageStatus::Complete
            || facts.coverage.status != CoverageStatus::Complete
            || crate::market_update::unit_drift(&facts.unit_currency, &segment.unit_currency)
        {
            return Ok(None);
        }
        let inputs = segment
            .provenance
            .snapshot_inputs
            .as_deref()
            .ok_or_else(|| {
                SourceError::InvalidResponse("snapshot-bound segment lacks snapshot inputs".into())
            })?;
        let member = |role: &str| {
            inputs
                .iter()
                .find(|input| input.role == role)
                .map(|input| input.member_sha256.clone())
                .ok_or_else(|| {
                    SourceError::InvalidResponse(format!(
                        "snapshot binding lacks the {role} member"
                    ))
                })
        };
        let bound_day = member("day")?;
        let bound_events = member(SECURITY_EVENTS_ROLE)?;
        if bound_day != day_member_sha256 || bound_events != events_sha256 {
            return Err(SourceError::InvalidResponse(
                "typed metadata snapshot inputs do not match the continuity probe".into(),
            ));
        }
        let manifest = self.session.source().manifest_sha256().to_string();
        Ok(Some(crate::market_update::TypedVersionMetadata {
            generation_id: self.generation_id.clone(),
            generation_manifest_sha256: manifest.clone(),
            day_member_sha256: bound_day,
            security_event_sha256: bound_events,
            generation_evidence: json!({
                "manifest_sha256": manifest,
                "snapshot_inputs": inputs,
            }),
            source: segment.provenance.source.clone(),
            coverage: result_coverage.clone(),
            unit_currency: segment.unit_currency.clone(),
            adjustment_evidence: segment.adjustment_evidence.clone(),
        }))
    }

    /// Typed ops-first acquisition (#259 S5G): probe continuity against
    /// compact base facts, parse only the proven-new tail records, and
    /// return exactly what the typed commit needs — no `fixed(base)`
    /// materialization and no combined JSON envelope. `Ok(None)` means
    /// the ops path cannot carry this update (absent continuity proof or
    /// unsupported coverage/unit metadata drift) and the caller falls
    /// back to `acquire`, which reproduces the envelope semantics for
    /// the rare cases that still need it.
    ///
    /// `base_window` is the caller's `base_bars_in_range` read — the
    /// base's leaf bars inside the accepted range only — used for the
    /// segment batch and the empty-range check the envelope path makes.
    fn acquire_typed(
        &self,
        facts: &crate::market_update::UpdateBaseFacts,
        base_window: &[Bar],
        range: &crate::market_update::DateRange,
    ) -> Result<Option<crate::market_update::runner::TypedAcquisition>, SourceError> {
        let symbol = facts.symbol.as_str();
        let Some((market, code)) = symbol.split_once('.').filter(|(m, c)| {
            matches!(*m, "sh" | "sz") && c.len() == 6 && c.bytes().all(|b| b.is_ascii_digit())
        }) else {
            return Err(SourceError::Unsupported(format!(
                "unsupported symbol {symbol:?}; expected sh|sz + six digits"
            )));
        };
        let (day_member, day_member_sha256, events_sha256, continuity) =
            self.probe_facts(facts, market, code)?;
        let (events, excluded) = self.session.security_events(market, code)?;
        let gbbq_member_sha256 = self.session.decoded_gbbq()?.sha256.clone();
        let source = self.session.source();
        let (mode, prefix_bytes) = match &continuity {
            Continuity::Unchanged { prefix_bytes } => ("unchanged", *prefix_bytes),
            Continuity::Append { prefix_bytes, .. } => ("append", *prefix_bytes),
            Continuity::Unverified => return Ok(None),
        };
        let make_evidence = |candidate_sha256: String| {
            Ok::<_, SourceError>(crate::market_update::Evidence {
                base_version_id: facts.version_id.clone(),
                base_integrity_sha256: facts.integrity_sha256.clone(),
                candidate_sha256,
                range: range.clone(),
                same_qfq_basis_ref: format!(
                    "inspected snapshot manifest {} + gbbq member {}",
                    source.manifest_sha256(),
                    gbbq_member_sha256
                ),
                revision_ref: None,
                continuity: Some(crate::market_update::ContinuityEvidence {
                    mode: mode.into(),
                    day_member_sha256: day_member_sha256.clone(),
                    day_prefix_bytes: prefix_bytes as u64,
                    gbbq_events_sha256: events_sha256.clone(),
                }),
            })
        };
        match continuity {
            Continuity::Unverified => unreachable!("returned above"),
            Continuity::Unchanged { .. } => {
                // Envelope parity: no base bars inside the accepted range
                // is a source failure there, not a no-change.
                if base_window.is_empty() {
                    return Err(SourceError::InvalidResponse(
                        "snapshot carries no bars inside the accepted range".into(),
                    ));
                }
                let mut segment = self.session.assemble_daily(
                    symbol,
                    DayMemberRead {
                        member: day_member.clone(),
                        sha256: day_member_sha256.clone(),
                    },
                    base_window.to_vec(),
                    events,
                    excluded,
                )?;
                // The member bytes and event set are byte-proven
                // identical to what the base was published from, so the
                // base's coverage claim remains exactly true for this
                // data (same pinning rule as the envelope path).
                segment.coverage = facts.coverage.clone();
                // Metadata the ops path cannot carry — a unit conflict
                // or enrichment (`prepare`'s two gates; reason text is
                // not drift) or a non-complete coverage claim requires
                // the envelope path's combined-candidate machinery.
                if crate::market_update::unit_drift(&facts.unit_currency, &segment.unit_currency)
                    || facts.coverage.status != crate::market_data::CoverageStatus::Complete
                {
                    return Ok(None);
                }
                crate::publish::validate_and_deduplicate(&segment)
                    .map_err(|e| SourceError::InvalidResponse(e.to_string()))?;
                crate::publish::validate_unit_currency_evidence(&segment)
                    .map_err(|e| SourceError::InvalidResponse(e.to_string()))?;
                crate::publish::validate_adjustment_evidence(&segment)
                    .map_err(|e| SourceError::InvalidResponse(e.to_string()))?;
                Ok(Some(crate::market_update::runner::TypedAcquisition {
                    decision: crate::market_update::runner::TypedDecision::Unchanged,
                    evidence: make_evidence(
                        crate::market_update::candidate_digest(&segment)
                            .map_err(|e| SourceError::InvalidResponse(e.to_string()))?,
                    )?,
                }))
            }
            Continuity::Append { tail, .. } => {
                let tail_bars = MaterializeSession::qfq_bars(&tail, events)?;
                let all_tail_in_range = tail_bars
                    .iter()
                    .all(|bar| range.start <= bar.date && bar.date <= range.end);
                let mut segment_bars = base_window.to_vec();
                segment_bars.extend(tail_bars);
                segment_bars.retain(|b| range.start <= b.date && b.date <= range.end);
                if segment_bars.is_empty() {
                    return Err(SourceError::InvalidResponse(
                        "snapshot carries no bars inside the accepted range".into(),
                    ));
                }
                let segment = self.session.assemble_daily(
                    symbol,
                    DayMemberRead {
                        member: day_member.clone(),
                        sha256: day_member_sha256.clone(),
                    },
                    segment_bars,
                    events,
                    excluded,
                )?;
                // Unit conflicts/enrichment and non-complete coverage
                // still need the envelope path (and its NEEDS_VERIFICATION
                // outcome where applicable). The published coverage is the
                // envelope merge's result: the new claim only when every
                // appended source record is inside the accepted range;
                // otherwise the base claim stays pinned (#320).
                let result_coverage = if all_tail_in_range {
                    segment.coverage.clone()
                } else {
                    facts.coverage.clone()
                };
                if segment.coverage.status != CoverageStatus::Complete
                    || result_coverage.status != CoverageStatus::Complete
                    || crate::market_update::unit_drift(
                        &facts.unit_currency,
                        &segment.unit_currency,
                    )
                {
                    return Ok(None);
                }
                crate::publish::validate_and_deduplicate(&segment)
                    .map_err(|e| SourceError::InvalidResponse(e.to_string()))?;
                crate::publish::validate_unit_currency_evidence(&segment)
                    .map_err(|e| SourceError::InvalidResponse(e.to_string()))?;
                crate::publish::validate_adjustment_evidence(&segment)
                    .map_err(|e| SourceError::InvalidResponse(e.to_string()))?;
                // The probe proved every tail record is strictly newer
                // than the base's last day, so the in-range tail bars
                // are pure upserts: no base date can be revised and none
                // is removed.
                let upserts: Vec<Bar> = segment
                    .bars
                    .iter()
                    .filter(|b| b.date > facts.last_day)
                    .cloned()
                    .collect();
                let metadata = if upserts.is_empty() {
                    None
                } else {
                    self.typed_append_metadata(
                        facts,
                        &segment,
                        &result_coverage,
                        &day_member_sha256,
                        &events_sha256,
                    )?
                };
                if (!upserts.is_empty() && metadata.is_none())
                    || (upserts.is_empty() && !all_tail_in_range)
                {
                    return Ok(None);
                }
                Ok(Some(crate::market_update::runner::TypedAcquisition {
                    decision: if upserts.is_empty() {
                        crate::market_update::runner::TypedDecision::Unchanged
                    } else {
                        crate::market_update::runner::TypedDecision::Append {
                            upserts,
                            metadata: metadata.map(Box::new),
                        }
                    },
                    evidence: make_evidence(
                        crate::market_update::candidate_digest(&segment)
                            .map_err(|e| SourceError::InvalidResponse(e.to_string()))?,
                    )?,
                }))
            }
        }
    }
}

/// Measurement-harness provider (#259 S5G-7): delegates the typed
/// acquisition to the real `TdxRangeProvider` — member read, sha256,
/// continuity probe, event digest, validations all run for real — then
/// substitutes a fabricated one-bar tail so the Append commit path is
/// exercised against an unchanged snapshot. The fabricated tail is
/// marked in the evidence; no production path uses this provider.
#[cfg(test)]
pub struct FabricatedAppendProvider<'a> {
    pub inner: TdxRangeProvider<'a>,
    /// Wall time spent inside the real `acquire_typed` call.
    pub acquire_ns: std::cell::Cell<u64>,
}
#[cfg(test)]
impl RangeProvider for FabricatedAppendProvider<'_> {
    fn acquire(
        &self,
        base: &CheckedRecord,
        range: &DateRange,
        candidate_id: &str,
    ) -> Result<Acquisition, SourceError> {
        self.inner.acquire(base, range, candidate_id)
    }
    fn acquire_typed(
        &self,
        facts: &crate::market_update::UpdateBaseFacts,
        base_window: &[Bar],
        range: &crate::market_update::DateRange,
    ) -> Result<Option<crate::market_update::runner::TypedAcquisition>, SourceError> {
        let started = std::time::Instant::now();
        let acquired = self.inner.acquire_typed(facts, base_window, range);
        self.acquire_ns
            .set(self.acquire_ns.get() + started.elapsed().as_nanos() as u64);
        let Some(mut acquired) = acquired? else {
            return Ok(None);
        };
        let next_day = facts.last_day + chrono::Duration::days(1);
        if next_day > range.end {
            return Ok(None);
        }
        let upsert = Bar {
            date: next_day,
            open: rust_decimal::Decimal::new(10000, 2),
            high: rust_decimal::Decimal::new(10025, 2),
            low: rust_decimal::Decimal::new(9975, 2),
            close: rust_decimal::Decimal::new(10010, 2),
            volume: Some(rust_decimal::Decimal::new(1_500_000, 0)),
            amount: Some(rust_decimal::Decimal::new(150_225_000, 0)),
            r7_raw: None,
        };
        acquired.decision = crate::market_update::runner::TypedDecision::Append {
            upserts: vec![upsert.clone()],
            metadata: None,
        };
        if let Some(continuity) = acquired.evidence.continuity.as_mut() {
            continuity.mode = "append".into();
        }
        acquired.evidence.candidate_sha256 = crate::hash::sha256(
            serde_json::json!({"harness_fabricated_tail": [upsert.date.to_string()]})
                .to_string()
                .as_bytes(),
        );
        acquired
            .evidence
            .same_qfq_basis_ref
            .push_str(" + harness fabricated tail");
        Ok(Some(acquired))
    }
}

/// Digest/identity context a continuity-proof update carries into its
/// combined candidate evidence.
struct ContinuityCtx<'a> {
    base_integrity: &'a str,
    day_member: &'a str,
    day_member_sha256: &'a str,
    events_sha256: &'a str,
    candidate_id: &'a str,
}

impl RangeProvider for TdxRangeProvider<'_> {
    fn acquire(
        &self,
        base: &CheckedRecord,
        range: &DateRange,
        candidate_id: &str,
    ) -> Result<Acquisition, SourceError> {
        let source = self.session.source();
        let symbol = base.version().symbol.clone();
        let Some((market, code)) = symbol.split_once('.').filter(|(m, c)| {
            matches!(*m, "sh" | "sz") && c.len() == 6 && c.bytes().all(|b| b.is_ascii_digit())
        }) else {
            return Err(SourceError::Unsupported(format!(
                "unsupported symbol {symbol:?}; expected sh|sz + six digits"
            )));
        };
        let base_integrity: String = base.raw()["integrity_sha256"]
            .as_str()
            .ok_or_else(|| {
                SourceError::NotVerified("fixed base carries no integrity digest".into())
            })?
            .into();
        let (day_member, day_member_sha256, events_sha256, continuity) =
            self.probe(base, market, code)?;
        let ctx = ContinuityCtx {
            base_integrity: &base_integrity,
            day_member: &day_member,
            day_member_sha256: &day_member_sha256,
            events_sha256: &events_sha256,
            candidate_id,
        };
        let (mode, prefix_bytes) = match &continuity {
            Continuity::Unchanged { prefix_bytes } => ("unchanged", *prefix_bytes),
            Continuity::Append { prefix_bytes, .. } => ("append", *prefix_bytes),
            Continuity::Unverified => ("full", 0),
        };
        let (events, excluded) = self.session.security_events(market, code)?;
        // Under an `unchanged`/`append` proof the prefix bars are byte-
        // identical to the base's materialization input, so the base's
        // stored bars are reused verbatim; only the tail is adjusted.
        // `unverified` pays the full materialize as before.
        let (batch, combined) = match continuity {
            Continuity::Unchanged { .. } => {
                let mut batch = self.session.assemble_daily(
                    &symbol,
                    DayMemberRead {
                        member: day_member.clone(),
                        sha256: day_member_sha256.clone(),
                    },
                    base.batch.bars.clone(),
                    events,
                    excluded,
                )?;
                // The member bytes and event set are byte-proven identical
                // to what the base was published from, so the base's
                // coverage claim remains exactly true for this data.
                // Carrying it forward (instead of the new manifest's
                // identical-in-content claim) keeps a cross-generation
                // no-change a real NoChange — otherwise the volatile
                // manifest digest alone would bump every symbol's version
                // on every generation.
                batch.coverage = base.batch.coverage.clone();
                let mut segment = batch.clone();
                segment
                    .bars
                    .retain(|b| range.start <= b.date && b.date <= range.end);
                if segment.bars.is_empty() {
                    return Err(SourceError::InvalidResponse(
                        "snapshot carries no bars inside the accepted range".into(),
                    ));
                }
                // Records are byte-proven identical; only batch metadata
                // (e.g. a different generation's manifest coverage claim)
                // can still authorize a new version. A provable no-change
                // must not carry a combined candidate — NoChange forbids
                // one — while a metadata change requires it.
                let combined = if batch.coverage == base.batch.coverage
                    && batch.unit_currency == base.version().unit_currency
                {
                    None
                } else {
                    Some(self.combined_candidate(
                        &ctx,
                        base,
                        &batch,
                        &segment,
                        base.version().records.clone(),
                        false,
                    )?)
                };
                (segment, combined)
            }
            Continuity::Append { tail, .. } => {
                let mut bars = base.batch.bars.clone();
                bars.extend(MaterializeSession::qfq_bars(&tail, events)?);
                let batch = self.session.assemble_daily(
                    &symbol,
                    DayMemberRead {
                        member: day_member.clone(),
                        sha256: day_member_sha256.clone(),
                    },
                    bars,
                    events,
                    excluded,
                )?;
                let mut segment = batch.clone();
                segment
                    .bars
                    .retain(|b| range.start <= b.date && b.date <= range.end);
                if segment.bars.is_empty() {
                    return Err(SourceError::InvalidResponse(
                        "snapshot carries no bars inside the accepted range".into(),
                    ));
                }
                let merged: Vec<Bar> = merge_segment_bars(base, &segment.bars)?
                    .into_values()
                    .collect();
                // A narrow accepted range can exclude every appended
                // record; when the merge reproduces the base exactly and
                // units are unchanged the update is a no-change, which
                // forbids a combined candidate. The base's coverage claim
                // remains exactly true for identical data — pinning it
                // keeps a cross-generation no-change from becoming a
                // metadata-only version bump on the volatile manifest
                // digest.
                if merged == base.version().records
                    && batch.unit_currency == base.version().unit_currency
                {
                    let mut segment = segment;
                    segment.coverage = base.batch.coverage.clone();
                    (segment, None)
                } else {
                    let combined =
                        self.combined_candidate(&ctx, base, &batch, &segment, merged, false)?;
                    (segment, Some(combined))
                }
            }
            Continuity::Unverified => {
                // Corporate-action rewrite (#290): the new member is the
                // authoritative full history — qfq repricing rewrote prior
                // bytes, so no prefix proof is possible. Adopt it wholesale
                // as a combined rewrite candidate (evidence mode `full`);
                // `prepare_range` binds it by requiring every acquired
                // segment bar verbatim and every base date still present.
                let full_batch = self.session.materialize_daily(&symbol)?;
                let mut segment = full_batch.clone();
                segment
                    .bars
                    .retain(|b| range.start <= b.date && b.date <= range.end);
                if segment.bars.is_empty() {
                    return Err(SourceError::InvalidResponse(
                        "snapshot carries no bars inside the accepted range".into(),
                    ));
                }
                let combined = self.combined_candidate(
                    &ctx,
                    base,
                    &full_batch,
                    &segment,
                    full_batch.bars.clone(),
                    true,
                )?;
                (segment, Some(combined))
            }
        };
        let gbbq_member_sha256 = self.session.decoded_gbbq()?.sha256.clone();
        Ok(Acquisition {
            evidence: Evidence {
                base_version_id: base.version().version_id.clone(),
                base_integrity_sha256: base_integrity,
                candidate_sha256: crate::market_update::candidate_digest(&batch)
                    .map_err(|e| SourceError::InvalidResponse(e.to_string()))?,
                range: range.clone(),
                // Same-QFQ-basis proof: this segment derives from the
                // manifest-pinned snapshot named here; continuity vs the
                // fixed base is carried by `continuity` below.
                same_qfq_basis_ref: format!(
                    "inspected snapshot manifest {} + gbbq member {}",
                    source.manifest_sha256(),
                    gbbq_member_sha256
                ),
                revision_ref: None,
                continuity: Some(crate::market_update::ContinuityEvidence {
                    mode: mode.into(),
                    day_member_sha256,
                    day_prefix_bytes: prefix_bytes as u64,
                    gbbq_events_sha256: events_sha256,
                }),
            },
            batch,
            combined,
        })
    }
    fn acquire_typed(
        &self,
        facts: &crate::market_update::UpdateBaseFacts,
        base_window: &[Bar],
        range: &crate::market_update::DateRange,
    ) -> Result<Option<crate::market_update::runner::TypedAcquisition>, SourceError> {
        TdxRangeProvider::acquire_typed(self, facts, base_window, range)
    }
}

/// Overlay in-range segment bars onto the fixed base's records. Same rule
/// as `market_update::prepare` (#4 AC-71): filling an unknown optional
/// field is enrichment, not a correction — a source bar contradicting a
/// known base value is a basis conflict and fails closed.
fn merge_segment_bars(
    base: &CheckedRecord,
    segment_bars: &[Bar],
) -> Result<BTreeMap<NaiveDate, Bar>, SourceError> {
    let mut merged: BTreeMap<_, _> = base
        .version()
        .records
        .iter()
        .map(|b| (b.date, b.clone()))
        .collect();
    for bar in segment_bars {
        if let Some(existing) = merged.get(&bar.date) {
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
    Ok(merged)
}

/// Serialize a merged candidate batch into the immutable version record
/// and bind its integrity digest.
/// Backend-private fields stamped into an envelope-staged daily record
/// before `integrity_sha256` is computed. The compact store requires
/// `compact_source` (generation identity, member digests and inspection
/// evidence) on every staged daily record and links the parent through
/// `compact_parent_version_id`; the typed collection path binds the same
/// facts inside `commit_collection_batch`, envelope staging carries them
/// inline instead (#288). The fields are inside the digest on purpose so
/// the staged record stays self-consistent on either backend.
fn staged_compact_fields(
    source: &TdxMarketSource,
    generation_id: &str,
    combined_batch: &MarketDataBatch,
    parent_version_id: Option<&str>,
) -> Result<Value, SourceError> {
    let inputs = combined_batch
        .provenance
        .snapshot_inputs
        .as_deref()
        .unwrap_or(&[]);
    let member = |role: &str| -> Result<String, SourceError> {
        inputs
            .iter()
            .find(|i| i.role == role)
            .map(|i| i.member_sha256.clone())
            .ok_or_else(|| {
                SourceError::InvalidResponse(format!("snapshot binding lacks the {role} member"))
            })
    };
    let mut fields = json!({
        "compact_source": {
            "generation_id": generation_id,
            "generation_manifest_sha256": source.manifest_sha256(),
            "day_member_sha256": member("day")?,
            "security_event_sha256": member(SECURITY_EVENTS_ROLE)?,
            "generation_evidence": {
                "manifest_sha256": source.manifest_sha256(),
                "snapshot_inputs": inputs,
            },
        },
    });
    // `None` means a full-revision root checkpoint: no parent linkage in the
    // version row — the base→result chain lives in the update result's
    // `full_revision_digest` instead (compact `ContinuityMode::Full` model).
    if let Some(parent) = parent_version_id {
        fields["compact_parent_version_id"] = json!(parent);
    }
    Ok(fields)
}

fn combined_version_record(
    combined_batch: &MarketDataBatch,
    candidate_id: &str,
    published_at: DateTime<Utc>,
    extras: Option<Value>,
) -> Result<Value, SourceError> {
    let (records, validation) = publish::validate_and_deduplicate(combined_batch)
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
    raw["batch"] = serde_json::to_value(combined_batch)
        .map_err(|e| SourceError::InvalidResponse(e.to_string()))?;
    if let Some(Value::Object(extras)) = extras {
        for (key, value) in extras {
            raw[key] = value;
        }
    }
    raw["integrity_sha256"] = Value::String(
        crate::market_storage::compat::legacy_digest(&raw)
            .map_err(|e| SourceError::InvalidResponse(e.to_string()))?,
    );
    Ok(raw)
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
    session: &MaterializeSession<'_>,
    base: &CheckedRecord,
    range: &DateRange,
    candidate_id: &str,
    published_at: DateTime<Utc>,
    revision_ref: Option<String>,
) -> Result<Acquisition, SourceError> {
    let source = session.source();
    let batch = session.materialize_daily(&base.version().symbol)?;
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
    let merged = merge_segment_bars(base, &segment.bars)?;
    let mut combined_batch = batch.clone();
    combined_batch.bars = merged.into_values().collect();
    combined_batch.coverage = if combined_batch.bars == batch.bars {
        // Merged == full materialized history: carry the same coverage
        // claim a fresh materialization makes so the stored metadata
        // stays stable for the next update.
        batch.coverage.clone()
    } else {
        base.batch.coverage.clone()
    };
    let raw = combined_version_record(
        &combined_batch,
        candidate_id,
        published_at,
        Some(staged_compact_fields(
            source,
            source.manifest_sha256(),
            &combined_batch,
            Some(base.version().version_id.as_str()),
        )?),
    )?;
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
            // Same-generation equality above already pins every input;
            // the digest-continuity modes exist for cross-generation
            // updates only.
            continuity: None,
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
    /// Shared decode/index channel; a single-item caller builds one fresh
    /// session and pays the one-time decode itself.
    pub session: &'a MaterializeSession<'a>,
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
            self.session,
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
    /// Shared decode/index channel; a single-item caller builds one fresh
    /// session and pays the one-time decode itself.
    pub session: &'a MaterializeSession<'a>,
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
            self.session,
            base,
            range,
            candidate_id,
            self.published_at,
            None,
        )
    }
}

/// Typed first-collection candidate for the compact bulk staging path
/// (#259 S5F): carries the canonical facts an envelope embeds — bars,
/// coverage, unit/adjustment evidence and the generation source binding —
/// without materializing legacy JSON.
pub struct CollectionCandidate {
    pub task_id: String,
    pub item_id: String,
    pub publication_id: String,
    pub version_id: String,
    pub run_id: String,
    pub symbol: String,
    pub source: String,
    pub bars: Vec<crate::market_data::Bar>,
    pub coverage: crate::market_data::CoverageEvidence,
    pub unit_currency: crate::market_data::UnitCurrencyEvidence,
    pub adjustment_evidence: crate::market_data::AdjustmentEvidence,
    pub published_at: DateTime<Utc>,
    pub generation_id: String,
    pub generation_manifest_sha256: String,
    pub day_member_sha256: String,
    pub security_event_sha256: String,
    pub generation_evidence: Value,
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
    /// Optional typed bulk first-collection commit (#259 S5F): one write
    /// transaction covers the whole task's items on backends that stage
    /// compact checkpoints directly. `None` keeps the per-item envelope
    /// path on legacy or any backend without a typed entry.
    fn commit_collection_batch(
        &self,
        task_id: &str,
        items: &[CollectionCandidate],
    ) -> Result<Option<Vec<Result<Receipt, Error>>>, Error> {
        let _ = (task_id, items);
        Ok(None)
    }
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
        let mut binding = match self.repository.task_item_binding(task_id, item_id)? {
            Some(binding) => binding,
            None => {
                self.repository.task_head(task_id)?;
                return Err(Error::Invalid(
                    "collection item absent from accepted task".into(),
                ));
            }
        };
        if binding.head.key.operation != "start_collection"
            || binding.head.key.dataset != "market.daily"
            || binding.head.key.contract_version != "v1"
        {
            return Err(Error::Invalid("unsupported collection contract".into()));
        }
        if let Some(receipt) = self.repository.collection_receipt(task_id, item_id)? {
            return Ok(ItemExecution::Published(receipt));
        }
        if binding.head.visible_status(&self.run_id) == Status::NeedsVerification {
            return Err(Error::Task(crate::task_control::Error::NeedsVerification(
                "old run or unresolved execution".into(),
            )));
        }
        if binding.item.stage == Stage::Running {
            return Err(Error::OutcomeUnknown(
                "running item has no verified result; inspect before any retry".into(),
            ));
        }
        if binding.head.status.terminal() || binding.item.stage == Stage::Finished {
            if binding.item.outcome.as_ref().is_some_and(|o| o.success) {
                return Err(Error::Integrity(
                    "successful collection item has no receipt".into(),
                ));
            }
            return self.not_published(task_id);
        }
        if binding.head.status == Status::CancelRequested {
            self.repository.transition_task_head(
                &mut binding.head,
                None,
                &self.run_id,
                Change::FinishExecution,
            )?;
            return self.not_published(task_id);
        }
        if binding.head.status == Status::Pending {
            self.repository.transition_task_head(
                &mut binding.head,
                None,
                &self.run_id,
                Change::Claim,
            )?;
        }
        self.repository.transition_task_head(
            &mut binding.head,
            Some(&mut binding.item),
            &self.run_id,
            Change::StartItem(item_id.into()),
        )?;
        let outcome = self.materialize_and_commit(&mut binding, item_id);
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
                    let mut binding = self
                        .repository
                        .task_item_binding(task_id, item_id)?
                        .ok_or(Error::Integrity("bound task item missing".into()))?;
                    if let Err(recording) = self.repository.transition_task_head(
                        &mut binding.head, Some(&mut binding.item), &self.run_id,
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
                let mut binding = self
                    .repository
                    .task_item_binding(task_id, item_id)?
                    .ok_or(Error::Integrity("bound task item missing".into()))?;
                let cancelled_without_publication = matches!(error, Error::Conflict(_))
                    && binding.head.status == Status::CancelRequested;
                self.repository.transition_task_head(
                    &mut binding.head,
                    Some(&mut binding.item),
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
                if binding.head.counts.finished == binding.head.counts.total
                    || binding.head.status == Status::CancelRequested
                {
                    self.repository.transition_task_head(
                        &mut binding.head,
                        None,
                        &self.run_id,
                        if cancelled_without_publication {
                            Change::FinishCancellation
                        } else {
                            Change::FinishExecution
                        },
                    )?;
                }
                self.not_published(task_id)
            }
        }
    }
    fn not_published(&self, task_id: &str) -> Result<ItemExecution, Error> {
        Ok(ItemExecution::NotPublished(Box::new(
            self.repository.task(task_id)?,
        )))
    }
    /// Bulk collection path (#259 S5F): produce the typed candidate for
    /// every item (member parse stays outside the writer), then stage and
    /// commit them in one repository transaction. Items that cannot
    /// produce a candidate — replays, leftover running items, materialize
    /// failures — fall back to `run_item` so outcome recording and crash
    /// reconciliation stay identical. `None` means the repository only
    /// supports the envelope path; callers keep the per-item loop.
    /// `generation_id` binds the snapshot generation the caller already
    /// resolved for this task's frozen `source_generation_id` input.
    pub fn run_items(
        &self,
        task_id: &str,
        item_ids: &[String],
        generation_id: &str,
    ) -> Result<Option<()>, Error> {
        let mut offset = 0;
        while offset < item_ids.len() {
            // Cancellation lands between chunks: committed chunks keep
            // their receipts, remaining items keep the per-item path
            // which owns the cancellation finish semantics.
            if offset > 0
                && self.repository.task_head(task_id)?.status
                    == crate::task_control::Status::CancelRequested
            {
                break;
            }
            let chunk = &item_ids
                [offset..(offset + crate::market_update::runner::EXEC_CHUNK).min(item_ids.len())];
            let mut candidates = Vec::new();
            let mut fallback = Vec::new();
            for item_id in chunk {
                match self.collection_candidate(task_id, item_id, generation_id) {
                    Ok(Some(candidate)) => candidates.push(candidate),
                    Ok(None) | Err(_) => fallback.push(item_id.clone()),
                }
            }
            let results = match self
                .repository
                .commit_collection_batch(task_id, &candidates)
            {
                Ok(Some(results)) => results,
                // The first chunk's `None` keeps the documented contract —
                // no batch surface, the caller runs the whole task per item.
                Ok(None) if offset == 0 => return Ok(None),
                // A mid-task `None`/structural error rolled the chunk back:
                // isolate per item so one bad item cannot wedge the task in a
                // Running-retry loop (the documented batch contract).
                Ok(None) | Err(_) => {
                    fallback.extend(candidates.iter().map(|item| item.item_id.clone()));
                    for item_id in fallback {
                        self.run_item(task_id, &item_id)?;
                    }
                    offset += crate::market_update::runner::EXEC_CHUNK;
                    continue;
                }
            };
            if results.len() != candidates.len() {
                return Err(Error::Integrity(
                    "collection batch result count does not match candidates".into(),
                ));
            }
            for (candidate, result) in candidates.iter().zip(results) {
                if result.is_err() {
                    fallback.push(candidate.item_id.clone());
                }
            }
            for item_id in fallback {
                self.run_item(task_id, &item_id)?;
            }
            offset += crate::market_update::runner::EXEC_CHUNK;
        }
        for item_id in &item_ids[offset.min(item_ids.len())..] {
            self.run_item(task_id, item_id)?;
        }
        Ok(Some(()))
    }

    /// Typed candidate production for `run_items`: every check `run_item`
    /// performs before staging, minus the envelope materialization. `None`
    /// defers the item to the per-item path (receipt replay, leftover
    /// running state or terminal task bookkeeping).
    fn collection_candidate(
        &self,
        task_id: &str,
        item_id: &str,
        generation_id: &str,
    ) -> Result<Option<CollectionCandidate>, Error> {
        let binding = self
            .repository
            .task_item_binding(task_id, item_id)?
            .ok_or_else(|| Error::Invalid("collection item absent from accepted task".into()))?;
        if binding.head.key.operation != "start_collection"
            || binding.head.key.dataset != "market.daily"
            || binding.head.key.contract_version != "v1"
        {
            return Err(Error::Invalid("unsupported collection contract".into()));
        }
        if binding.head.status.terminal()
            || binding.head.visible_status(&self.run_id) == Status::NeedsVerification
            || binding.item.stage != Stage::Pending
        {
            // Terminal tasks, unverified runs and items already started or
            // finished keep the per-item path: its replay and
            // reconciliation semantics are authoritative there.
            return Ok(None);
        }
        if binding.head.status == Status::CancelRequested {
            return Ok(None);
        }
        let target = &binding.item.spec.target;
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
        let id = crate::hash::sha256(serde_json::json!([task_id, item_id]).to_string().as_bytes());
        let (records, _) =
            publish::validate_and_deduplicate(&batch).map_err(|e| Error::Invalid(e.to_string()))?;
        let inputs = batch.provenance.snapshot_inputs.clone().unwrap_or_default();
        let member_sha = |role: &str| {
            inputs
                .iter()
                .find(|input| input.role == role)
                .map(|input| input.member_sha256.clone())
                .ok_or_else(|| {
                    Error::Integrity(format!("snapshot inputs lack the {role} member binding"))
                })
        };
        Ok(Some(CollectionCandidate {
            task_id: task_id.into(),
            item_id: item_id.into(),
            publication_id: format!("collection-result-{id}"),
            version_id: format!("collection-version-{id}"),
            run_id: self.run_id.clone(),
            symbol: batch.symbol.clone(),
            source: batch.provenance.source.clone(),
            bars: records,
            coverage: batch.coverage.clone(),
            unit_currency: batch.unit_currency.clone(),
            adjustment_evidence: batch.adjustment_evidence.clone(),
            published_at: self.published_at,
            generation_id: generation_id.into(),
            generation_manifest_sha256: self.source.manifest_sha256().to_string(),
            day_member_sha256: member_sha("day")?,
            security_event_sha256: member_sha(SECURITY_EVENTS_ROLE)?,
            generation_evidence: serde_json::json!({
                "manifest_sha256": self.source.manifest_sha256(),
                "snapshot_inputs": inputs,
            }),
        }))
    }

    /// Source failure preserves the old version: nothing is staged or
    /// committed on any materialization/validation error.
    fn materialize_and_commit(
        &self,
        binding: &mut crate::task_control::ItemBinding,
        item_id: &str,
    ) -> Result<Receipt, Error> {
        let target = &binding.item.spec.target;
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
        let current = self.repository.task_head(&binding.head.task_id)?;
        if current.status != Status::Running
            || current.state_revision != binding.head.state_revision
        {
            return Err(Error::Conflict(
                "task changed or cancelled during materialization".into(),
            ));
        }
        let id = crate::hash::sha256(
            serde_json::json!([binding.head.task_id, item_id])
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
            task_id: binding.head.task_id.clone(),
            item_id: item_id.into(),
            run_id: self.run_id.clone(),
            expected_revision: binding.head.state_revision,
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
