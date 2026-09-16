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
//! published evidence reason. `.day` amount/volume units are not
//! evidence-anchored, so those fields are not promoted into published bars.
use crate::{
    day_file, gbbq,
    market_data::{
        Adjustment, AdjustmentEvidence, Bar, CoverageEvidence, EvidenceValue, MarketDataBatch,
        MarketDataPipeline, Period, Provenance, SnapshotInputBinding, SourceError,
        UnitCurrencyEvidence,
    },
    market_storage::{
        Error, MarketRepository, Publication, PublishIntent, Publisher, Receipt, Scope,
    },
    market_update::runner::ItemExecution,
    publish::{self, EffectiveVersion},
    qfq,
    snapshot::{self, FileInput, FileRole},
    task_control::{Change, ItemOutcome, Stage, Status, TaskSnapshot},
};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::{io::Read, path::PathBuf};

pub const SOURCE_NAME: &str = "tdx-official-file-snapshot";
pub const ADAPTER_VERSION: &str = "tdx-market-source-v1";
pub const NOT_PRODUCTION_READY: &str =
    "NOT_PRODUCTION_READY: official_files::PRODUCTION_BLOCKERS gates unchanged";
const GBBQ_MEMBER: &str = "gbbq";

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

    /// Materialize the snapshot's complete `.day` history for one security
    /// as forward-adjusted (QFQ) bars.
    pub fn materialize_daily(&self, symbol: &str) -> Result<MarketDataBatch, SourceError> {
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
        let (day_member, day_bytes) = self.member_bytes(&self.day, &day_names)?;
        let raw_bars = day_file::parse(&day_bytes)
            .map_err(|e| SourceError::InvalidResponse(format!(".day member malformed: {e}")))?;
        if raw_bars.is_empty() {
            return Err(SourceError::InvalidResponse(
                ".day member carries no records".into(),
            ));
        }
        let (gbbq_member, gbbq_bytes) = self.member_bytes(&self.gbbq, &[GBBQ_MEMBER.into()])?;
        let raw = gbbq::parse(&gbbq_bytes)
            .map_err(|e| SourceError::InvalidResponse(format!("gbbq member malformed: {e}")))?;
        let market_byte: u8 = if market == "sh" { 1 } else { 0 };
        let mut events = Vec::new();
        let mut excluded = 0usize;
        for event in gbbq::decode_events(&raw)
            .map_err(|e| SourceError::InvalidResponse(format!("gbbq record decode failed: {e}")))?
        {
            if event.market != market_byte || event.code != code {
                continue;
            }
            // Only category 1 has verified QFQ semantics; other categories are
            // never interpreted or applied to prices.
            if event.category == 1 {
                events.push(event);
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
                volume: None,
                amount: None,
            })
            .collect();
        let acquired_at = self
            .day
            .acquisition_completed_at
            .max(self.gbbq.acquisition_completed_at);
        let binding = |input: &FileInput, member: String, bytes: &[u8]| SnapshotInputBinding {
            role: serde_json::to_value(input.role)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string(),
            member,
            archive_sha256: input.sha256.clone(),
            member_sha256: crate::hash::sha256(bytes),
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
                snapshot_inputs: Some(vec![
                    binding(&self.day, day_member, &day_bytes),
                    binding(&self.gbbq, gbbq_member, &gbbq_bytes),
                ]),
            },
            coverage: CoverageEvidence::complete(format!(
                "complete .day member history from inspected snapshot manifest {}; {NOT_PRODUCTION_READY}",
                self.manifest_sha256
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
                volume_unit: EvidenceValue::unknown(
                    ".day volume unit semantics not evidence-anchored; raw volume not promoted",
                ),
                amount_currency: EvidenceValue::unknown(
                    ".day amount semantics not evidence-anchored; raw amount not promoted",
                ),
                amount_unit: EvidenceValue::unknown(
                    ".day amount unit semantics not evidence-anchored; raw amount not promoted",
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
        let batch = self
            .source
            .materialize_daily(symbol)
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
