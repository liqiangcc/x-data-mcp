//! Security-master assembly (M2 PR-3, issue-391 §4/§9.2/§11/§16).
//!
//! Layer position (separation point 1): this module turns *facts* — TDX
//! lifecycle/arrangement facts and an admitted exchange-list snapshot — into
//! the `security_master` interval rows and `code_migrations` facts. It never
//! parses a source file and never opens a database. The exchange adapter's
//! whole boundary is [`exchange_source`]: one function plus a [`PendingSource`]
//! value, no trait, because an admitted source is a frozen snapshot and "not
//! admitted" must stay distinguishable from "zero rows" (AC-M2-2/AC-M2-13).
//!
//! Two identity rules drive the assembly:
//!
//! - The row identity is `(exchange, code, effective_from)` (Q2). A code can be
//!   reused by a new issuer; when the exchange listing date and the TDX
//!   subject-level `SSDATE` disagree, the facts describe two subjects, not a
//!   conflict to average away (#396). The assembly opens two intervals at the
//!   later date, keeps both source values, and marks the row whose history
//!   crosses the boundary `issuer_resolution='ambiguous'` (Q3). If the inputs
//!   still overlap (a hand-built or future-source case), nothing is merged and
//!   [`lookup`] returns [`LookupError::AmbiguousCodeReuse`] (AC-M2-3).
//! - A *confirmed* code migration collapses the old code's interval at the
//!   migration day and starts the new code there (§4.1, §9.2). A *candidate*
//!   migration is recorded but never changes an interval: §9.2 says only
//!   `certainty='confirmed'` rows may collapse two code histories.
//!
//! The output is deterministic: rows, migrations and conflicts are sorted by
//! their identity, so the same facts always produce the same digest and the
//! same `version_id` (§3.10).

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use sha2::{Digest as _, Sha256};

use super::{
    ArrangementFact, DelistingReason, DelistingSource, Exchange, IssuerResolution, ListingSource,
    SecurityLifecycleFact,
};

/// `derived_dataset_versions.dataset_kind` of the assembled main table.
pub const DATASET_KIND: &str = "security_master";
/// `derived_dataset_versions.dataset_key`: M2 assembles the A-share master. A
/// later security type is a row property (`instrument_class`), not a second
/// dataset (§15 scenario 1).
pub const DATASET_KEY: &str = "a_share";
/// Digest algorithm name stored in `digest_format` (§3.10):
/// `SHA256("security-master-v1" ‖ rows ‖ migrations)`.
pub const DIGEST_FORMAT: &str = "security-master-v1";
/// Derivation label stored in `algorithm_version`.
pub const ALGORITHM_VERSION: &str = "security-master-assembly-v1";

/// A source branch that has no admitted snapshot. It is a value, not an error:
/// the assembly still writes the TDX facts and records the branch so a run can
/// report `pending_source` per dataset (AC-M2-13, §10.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PendingSource {
    pub dataset: &'static str,
    pub source: &'static str,
    pub reason: &'static str,
}

/// One listing row from the exchange list (S-1). Only the facts the domain
/// needs: the adapter owns `STOCK_TYPE`/`STATE_CODE` filtering and the B-share
/// rows (#396/#398).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExchangeListingFact {
    pub exchange: Exchange,
    pub code: String,
    /// The exchange's A-share listing date: the *code's* first listing day,
    /// which is not always the current subject's listing day (#396).
    pub listing_date: Option<NaiveDate>,
    /// The B-share listing date of the same company, when the list carries it.
    pub b_listing_date: Option<NaiveDate>,
    /// The exchange's issuer key (company name / orgId) for the *current*
    /// subject. `None` when the source has no stable key.
    pub issuer_key: Option<String>,
    /// 0 = A share. The adapter maps `STOCK_TYPE=1/8` to 0; later security
    /// types use the schema's reserved values.
    pub instrument_class: u8,
}

/// One delisting row from the exchange terminated-list (S-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExchangeDelistingFact {
    pub exchange: Exchange,
    pub code: String,
    /// The delisting-effective day (open upper bound, §4.3).
    pub delisting_date: NaiveDate,
    pub reason_class: DelistingReason,
}

/// One code change from the exchange announcement (A-2, `sz.300114` ->
/// `sz.302132` on 2025-02-17).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExchangeCodeMigration {
    pub from_exchange: Exchange,
    pub from_code: String,
    pub to_exchange: Exchange,
    pub to_code: String,
    pub effective_from: NaiveDate,
    pub source_record_id: Option<String>,
    pub known_at: Option<NaiveDate>,
}

/// An admitted exchange-list snapshot for one generation. `as_of` is the
/// snapshot date (evidence only); it is never used as a `known_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExchangeSnapshot {
    pub as_of: NaiveDate,
    pub listing: Vec<ExchangeListingFact>,
    pub delisting: Vec<ExchangeDelistingFact>,
    pub migrations: Vec<ExchangeCodeMigration>,
}

/// The exchange-list adapter's answer for one generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExchangeSource {
    Admitted(ExchangeSnapshot),
    Pending(PendingSource),
}

/// The exchange-list adapter boundary (§16 PR-3): one function, no trait.
///
/// `Some(snapshot)` is an admitted, generation-frozen snapshot (A-1/A-2).
/// `None` is not an empty list: it is the `pending_source` state, and the
/// assembly keeps the TDX value instead of fabricating an exchange row
/// (AC-M2-2/AC-M2-13). The concrete SSE/SZSE parser lives next to
/// `official_security` once A-1/A-2 are implemented; this function is the only
/// seam the domain sees.
pub fn exchange_source(snapshot: Option<ExchangeSnapshot>) -> ExchangeSource {
    match snapshot {
        Some(snapshot) => ExchangeSource::Admitted(snapshot),
        None => ExchangeSource::Pending(PendingSource {
            dataset: DATASET_KIND,
            source: "exchange_list",
            reason: "no admitted exchange listing/delisting snapshot is frozen for this generation (A-1/A-2)",
        }),
    }
}

/// How sure the source is about a code change (§4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MigrationCertainty {
    Confirmed,
    Candidate,
}

impl MigrationCertainty {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Candidate => "candidate",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "confirmed" => Some(Self::Confirmed),
            "candidate" => Some(Self::Candidate),
            _ => None,
        }
    }
}

/// One `code_migrations` fact. `source_kind` is the evidence vocabulary
/// (`exchange_announcement`, `bar_evidence`); the domain does not know the
/// publisher.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CodeMigrationFact {
    pub from_exchange: Exchange,
    pub from_code: String,
    pub to_exchange: Exchange,
    pub to_code: String,
    pub effective_from: NaiveDate,
    pub certainty: MigrationCertainty,
    pub source_kind: String,
    pub source_record_id: Option<String>,
    pub known_at: Option<NaiveDate>,
}

impl CodeMigrationFact {
    pub fn from_entity(&self) -> String {
        format!("{}.{}", self.from_exchange.as_str(), self.from_code)
    }

    pub fn to_entity(&self) -> String {
        format!("{}.{}", self.to_exchange.as_str(), self.to_code)
    }
}

/// One entity's `.day` span, used only for the *candidate* migration rule
/// (§4.1). A candidate is a lead, never dedup truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BarSpan {
    pub exchange: Exchange,
    pub code: String,
    pub first_day: NaiveDate,
    pub last_day: NaiveDate,
}

/// Candidate code migrations from bar evidence: two codes share the same first
/// bar day, the old code's bars stop, and the new code's bars continue
/// (§4.1). The rule is deliberately narrow but not proof: two genuine IPOs on
/// the same day with one delisted early also match. That is why every result is
/// `certainty='candidate'` and the assembly never uses it to collapse two
/// histories (§9.2).
pub fn detect_candidate_migrations(spans: &[BarSpan]) -> Vec<CodeMigrationFact> {
    let mut by_first: BTreeMap<NaiveDate, Vec<&BarSpan>> = BTreeMap::new();
    for span in spans {
        if span.last_day < span.first_day {
            continue;
        }
        by_first.entry(span.first_day).or_default().push(span);
    }
    let mut out = Vec::new();
    for group in by_first.values() {
        if group.len() < 2 {
            continue;
        }
        let Some(max_last) = group.iter().map(|span| span.last_day).max() else {
            continue;
        };
        let successors: Vec<_> = group
            .iter()
            .filter(|span| span.last_day == max_last)
            .collect();
        // A tie means the continuing code is ambiguous; do not guess one.
        if successors.len() != 1 {
            continue;
        }
        let successor = successors[0];
        for predecessor in group {
            if predecessor.exchange == successor.exchange && predecessor.code == successor.code {
                continue;
            }
            if predecessor.last_day >= successor.last_day {
                continue;
            }
            let Some(effective_from) = predecessor.last_day.succ_opt() else {
                continue;
            };
            out.push(CodeMigrationFact {
                from_exchange: predecessor.exchange,
                from_code: predecessor.code.clone(),
                to_exchange: successor.exchange,
                to_code: successor.code.clone(),
                effective_from,
                certainty: MigrationCertainty::Candidate,
                source_kind: "bar_evidence".into(),
                source_record_id: None,
                known_at: None,
            });
        }
    }
    out.sort();
    out.dedup();
    out
}

/// One recorded disagreement or unresolved interval. The DB keeps the flags
/// (`listing_conflict`/`delisting_conflict`); this list is what the evidence
/// bundle enumerates (§4.2/§4.3 "冲突清单可枚举").
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Conflict {
    ListingDate {
        exchange: Exchange,
        code: String,
        exchange_date: NaiveDate,
        tdx_date: NaiveDate,
    },
    DelistingDate {
        exchange: Exchange,
        code: String,
        exchange_date: NaiveDate,
        tdx_date: NaiveDate,
    },
    AmbiguousCodeReuse {
        exchange: Exchange,
        code: String,
        /// The start of the overlapping interval.
        from: NaiveDate,
        /// The end of the overlap; `None` = both intervals are open-ended.
        to: Option<NaiveDate>,
    },
}

/// A fact the assembly could not place. Counted and kept in the evidence
/// bundle; it never becomes a row with a guessed `effective_from`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnresolvedFact {
    ListingWithoutDate {
        exchange: Exchange,
        code: String,
    },
    DelistingWithoutInterval {
        exchange: Exchange,
        code: String,
        date: NaiveDate,
    },
    MigrationWithoutFromInterval {
        from: String,
        to: String,
        effective_from: NaiveDate,
    },
    MigrationWithoutToInterval {
        from: String,
        to: String,
        effective_from: NaiveDate,
    },
}

/// The assembled dataset: rows, migrations, conflicts and the source branches
/// that were pending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assembly {
    pub rows: Vec<SecurityLifecycleFact>,
    pub migrations: Vec<CodeMigrationFact>,
    pub conflicts: Vec<Conflict>,
    pub pending_sources: Vec<PendingSource>,
    pub unresolved: Vec<UnresolvedFact>,
    /// The admitted exchange snapshot's date; `None` when the branch is
    /// pending. Evidence only: never a `known_at` (§4.2).
    pub exchange_as_of: Option<NaiveDate>,
}

impl Assembly {
    pub fn ambiguous_code_reuse_count(&self) -> usize {
        self.conflicts
            .iter()
            .filter(|conflict| matches!(conflict, Conflict::AmbiguousCodeReuse { .. }))
            .count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AssemblyError {
    #[error("security_master has two intervals for {entity} starting {effective_from}")]
    DuplicateInterval {
        entity: String,
        effective_from: NaiveDate,
    },
    #[error("TDX facts for {entity} contain {count} intervals; PR-2 emits one per code")]
    AmbiguousTdxRows { entity: String, count: usize },
    #[error("security_master interval for {entity} is inconsistent: {detail}")]
    InconsistentInterval { entity: String, detail: String },
    #[error("code migration {from} -> {to} is invalid: {detail}")]
    InvalidMigration {
        from: String,
        to: String,
        detail: String,
    },
}

/// Interval rows keyed by `(exchange, code)`. A code has more than one row
/// only when it was reused by a new subject (§4.1).
type CodeIntervals = BTreeMap<(Exchange, String), Vec<SecurityLifecycleFact>>;

/// Assemble the main table from the parsed facts (§4, §9.2, §11).
///
/// `tdx` are PR-2's lifecycle facts (one interval per code), `arrangements`
/// are the TDX `infoharbor` rows including codes whose `SSDATE` is the `'0'`
/// sentinel, `exchange` is the adapter's answer, and `candidates` are the
/// bar-evidence leads. The function is pure: same inputs, same [`Assembly`].
pub fn assemble_security_master(
    tdx: &[SecurityLifecycleFact],
    arrangements: &[ArrangementFact],
    exchange: ExchangeSource,
    candidates: &[CodeMigrationFact],
) -> Result<Assembly, AssemblyError> {
    let arrangement_index: BTreeMap<(Exchange, String), &ArrangementFact> = arrangements
        .iter()
        .map(|fact| ((fact.exchange, fact.code.clone()), fact))
        .collect();
    let mut by_code = CodeIntervals::new();
    for fact in tdx {
        let key = (fact.exchange, fact.code.clone());
        let rows = by_code.entry(key.clone()).or_default();
        if rows
            .iter()
            .any(|row| row.effective_from == fact.effective_from)
        {
            return Err(AssemblyError::DuplicateInterval {
                entity: fact.entity(),
                effective_from: fact.effective_from,
            });
        }
        rows.push(fact.clone());
    }

    let mut conflicts = Vec::new();
    let mut pending_sources = Vec::new();
    let mut unresolved = Vec::new();
    let mut confirmed = Vec::new();
    let mut exchange_as_of = None;

    match exchange {
        ExchangeSource::Pending(pending) => pending_sources.push(pending),
        ExchangeSource::Admitted(snapshot) => {
            exchange_as_of = Some(snapshot.as_of);
            for listing in &snapshot.listing {
                let key = (listing.exchange, listing.code.clone());
                let Some(listing_date) = listing.listing_date else {
                    unresolved.push(UnresolvedFact::ListingWithoutDate {
                        exchange: listing.exchange,
                        code: listing.code.clone(),
                    });
                    continue;
                };
                let arrangement = arrangement_index.get(&key).copied();
                match by_code.entry(key) {
                    Entry::Vacant(entry) => {
                        let mut row = exchange_row(listing, listing_date);
                        apply_arrangement(&mut row, arrangement);
                        entry.insert(vec![row]);
                    }
                    Entry::Occupied(mut entry) => {
                        let rows = entry.get_mut();
                        if rows.len() != 1 {
                            return Err(AssemblyError::AmbiguousTdxRows {
                                entity: entity_of(listing.exchange, &listing.code),
                                count: rows.len(),
                            });
                        }
                        let tdx_row = rows.remove(0);
                        if tdx_row.effective_from == listing_date {
                            let mut merged = tdx_row;
                            merge_exchange_listing(&mut merged, listing);
                            apply_arrangement(&mut merged, arrangement);
                            rows.push(merged);
                        } else {
                            let tdx_date = tdx_row.effective_from;
                            let (earlier, later) =
                                split_subject_change(tdx_row, listing, arrangement)?;
                            conflicts.push(Conflict::ListingDate {
                                exchange: listing.exchange,
                                code: listing.code.clone(),
                                exchange_date: listing_date,
                                tdx_date,
                            });
                            rows.push(earlier);
                            rows.push(later);
                        }
                    }
                }
            }

            for delisting in &snapshot.delisting {
                let key = (delisting.exchange, delisting.code.clone());
                let Some(rows) = by_code.get_mut(&key) else {
                    unresolved.push(UnresolvedFact::DelistingWithoutInterval {
                        exchange: delisting.exchange,
                        code: delisting.code.clone(),
                        date: delisting.delisting_date,
                    });
                    continue;
                };
                let Some(row) = rows.iter_mut().max_by_key(|row| row.effective_from) else {
                    unresolved.push(UnresolvedFact::DelistingWithoutInterval {
                        exchange: delisting.exchange,
                        code: delisting.code.clone(),
                        date: delisting.delisting_date,
                    });
                    continue;
                };
                if delisting.delisting_date <= row.effective_from {
                    unresolved.push(UnresolvedFact::DelistingWithoutInterval {
                        exchange: delisting.exchange,
                        code: delisting.code.clone(),
                        date: delisting.delisting_date,
                    });
                    continue;
                }
                let tdx_date = row.delisting_date_tdx;
                row.delisting_date = Some(delisting.delisting_date);
                row.delisting_source = DelistingSource::ExchangeList;
                row.delisting_reason_class = delisting.reason_class;
                row.effective_to = Some(delisting.delisting_date);
                if let Some(tdx_date) = tdx_date
                    && tdx_date != delisting.delisting_date
                {
                    row.delisting_conflict = true;
                    conflicts.push(Conflict::DelistingDate {
                        exchange: delisting.exchange,
                        code: delisting.code.clone(),
                        exchange_date: delisting.delisting_date,
                        tdx_date,
                    });
                }
            }

            for migration in &snapshot.migrations {
                confirmed.push(CodeMigrationFact {
                    from_exchange: migration.from_exchange,
                    from_code: migration.from_code.clone(),
                    to_exchange: migration.to_exchange,
                    to_code: migration.to_code.clone(),
                    effective_from: migration.effective_from,
                    certainty: MigrationCertainty::Confirmed,
                    source_kind: "exchange_announcement".into(),
                    source_record_id: migration.source_record_id.clone(),
                    known_at: migration.known_at,
                });
            }
        }
    }

    let migrations = merge_migrations(confirmed, candidates)?;
    for migration in &migrations {
        if migration.certainty != MigrationCertainty::Confirmed {
            continue;
        }
        apply_confirmed_migration(&mut by_code, migration, &mut unresolved)?;
    }

    let mut rows = Vec::new();
    for ((exchange, code), mut intervals) in by_code {
        intervals.sort_by_key(|row| row.effective_from);
        for pair in intervals.windows(2) {
            if pair[0].effective_from == pair[1].effective_from {
                return Err(AssemblyError::DuplicateInterval {
                    entity: entity_of(exchange, &code),
                    effective_from: pair[0].effective_from,
                });
            }
        }
        for index in 0..intervals.len().saturating_sub(1) {
            let (head, tail) = intervals.split_at_mut(index + 1);
            let previous = &mut head[index];
            let next = &mut tail[0];
            let overlap_end = match (previous.effective_to, next.effective_to) {
                (None, None) => None,
                (None, Some(to)) | (Some(to), None) => Some(to),
                (Some(left), Some(right)) => Some(left.min(right)),
            };
            let overlaps = overlap_end.is_none_or(|end| end > next.effective_from);
            if overlaps {
                previous.issuer_resolution = IssuerResolution::Ambiguous;
                next.issuer_resolution = IssuerResolution::Ambiguous;
                conflicts.push(Conflict::AmbiguousCodeReuse {
                    exchange,
                    code: code.clone(),
                    from: next.effective_from,
                    to: overlap_end,
                });
            }
        }
        rows.extend(intervals);
    }

    rows.sort_by(|left, right| {
        (left.exchange, left.code.as_str(), left.effective_from).cmp(&(
            right.exchange,
            right.code.as_str(),
            right.effective_from,
        ))
    });
    conflicts.sort();
    conflicts.dedup();
    unresolved.sort();
    unresolved.dedup();
    pending_sources.sort();
    pending_sources.dedup();
    Ok(Assembly {
        rows,
        migrations,
        conflicts,
        pending_sources,
        unresolved,
        exchange_as_of,
    })
}

fn entity_of(exchange: Exchange, code: &str) -> String {
    format!("{}.{}", exchange.as_str(), code)
}

/// A row that exists only because the exchange list named the code: the
/// `SSDATE='0'` case, or a code the TDX table does not carry.
fn exchange_row(listing: &ExchangeListingFact, listing_date: NaiveDate) -> SecurityLifecycleFact {
    SecurityLifecycleFact {
        exchange: listing.exchange,
        code: listing.code.clone(),
        effective_from: listing_date,
        effective_to: None,
        instrument_class: listing.instrument_class,
        issuer_key: listing.issuer_key.clone(),
        issuer_resolution: if listing.issuer_key.is_some() {
            IssuerResolution::IssuerKey
        } else {
            IssuerResolution::CodeDate
        },
        listing_date: Some(listing_date),
        listing_date_tdx: None,
        listing_source: ListingSource::ExchangeList,
        b_listing_date: listing.b_listing_date,
        delisting_date: None,
        delisting_date_tdx: None,
        delisting_source: DelistingSource::Unknown,
        delisting_reason_class: DelistingReason::Unknown,
        arrangement_start: None,
        arrangement_end: None,
        listing_conflict: false,
        delisting_conflict: false,
    }
}

/// Both sources describe the same interval start: the exchange date wins, the
/// TDX value is kept, and there is no conflict (§4.2).
fn merge_exchange_listing(row: &mut SecurityLifecycleFact, listing: &ExchangeListingFact) {
    row.instrument_class = listing.instrument_class;
    row.issuer_key = listing.issuer_key.clone();
    if listing.issuer_key.is_some() {
        row.issuer_resolution = IssuerResolution::IssuerKey;
    }
    row.listing_date = listing.listing_date;
    if row.listing_date_tdx.is_none() {
        row.listing_date_tdx = listing.listing_date;
    }
    row.listing_source = ListingSource::ExchangeList;
    row.b_listing_date = listing.b_listing_date;
    row.listing_conflict = false;
}

/// Two different listing dates for one code. #396: the exchange value is the
/// code-level first listing, the TDX `SSDATE` is the current subject's listing
/// day, so the later date is a subject-change boundary. The interval before
/// the boundary keeps the earlier source's value; the interval after it is the
/// current subject and is marked `ambiguous` because its entity's history
/// still carries the earlier subject's bars (Q3).
fn split_subject_change(
    tdx_row: SecurityLifecycleFact,
    listing: &ExchangeListingFact,
    arrangement: Option<&ArrangementFact>,
) -> Result<(SecurityLifecycleFact, SecurityLifecycleFact), AssemblyError> {
    let listing_date = listing
        .listing_date
        .ok_or_else(|| AssemblyError::InconsistentInterval {
            entity: entity_of(listing.exchange, &listing.code),
            detail: "exchange listing fact has no listing date".into(),
        })?;
    let entity = entity_of(listing.exchange, &listing.code);
    if listing_date == tdx_row.effective_from {
        return Err(AssemblyError::InconsistentInterval {
            entity,
            detail: "split called for equal listing dates".into(),
        });
    }
    let exchange_is_earlier = listing_date < tdx_row.effective_from;
    let (earlier_date, later_date) = if exchange_is_earlier {
        (listing_date, tdx_row.effective_from)
    } else {
        (tdx_row.effective_from, listing_date)
    };
    if let Some(delisting) = tdx_row.delisting_date
        && delisting <= later_date
        && !exchange_is_earlier
    {
        // The TDX subject delisted before the exchange claims the code listed
        // again: the two facts cannot be ordered into one history.
        return Err(AssemblyError::InconsistentInterval {
            entity,
            detail: format!(
                "TDX delisting {delisting} is not after the exchange listing {listing_date}"
            ),
        });
    }

    let mut earlier = if exchange_is_earlier {
        exchange_row(listing, listing_date)
    } else {
        // The TDX interval is the earlier one. Its arrangement/delisting
        // belongs to the current subject, which is the later row, so clear it
        // here instead of letting the old interval claim it.
        let mut row = tdx_row.clone();
        row.arrangement_start = None;
        row.arrangement_end = None;
        row.delisting_date = None;
        row.delisting_date_tdx = None;
        row.delisting_source = DelistingSource::Unknown;
        row.delisting_reason_class = DelistingReason::Unknown;
        row.issuer_resolution = IssuerResolution::Ambiguous;
        row
    };
    earlier.effective_to = Some(later_date);
    earlier.listing_conflict = true;

    let mut later = if exchange_is_earlier {
        let mut row = tdx_row;
        row.instrument_class = listing.instrument_class;
        row.issuer_key = listing.issuer_key.clone();
        row.b_listing_date = listing.b_listing_date;
        row.issuer_resolution = IssuerResolution::Ambiguous;
        row
    } else {
        let mut row = exchange_row(listing, listing_date);
        // The TDX `SSDATE` is the subject's listing day, which still describes
        // the current subject even when the code itself listed later.
        row.listing_date_tdx = Some(tdx_row.effective_from);
        row
    };
    later.effective_from = later_date;
    later.listing_conflict = true;
    apply_arrangement(&mut later, arrangement);
    Ok((earlier, later))
}

/// Fill the TDX arrangement bounds and, when no other delisting source won,
/// the delisting date itself (§4.3: exchange list > TDX arrangement > unknown).
fn apply_arrangement(row: &mut SecurityLifecycleFact, arrangement: Option<&ArrangementFact>) {
    let Some(arrangement) = arrangement else {
        return;
    };
    if row.arrangement_start.is_none() {
        row.arrangement_start = arrangement.start;
    }
    if row.arrangement_end.is_none() {
        row.arrangement_end = arrangement.end;
    }
    if row.delisting_date.is_none() {
        if let Some(end) = arrangement.end {
            row.delisting_date = Some(end);
            row.delisting_date_tdx = Some(end);
            row.delisting_source = DelistingSource::TdxArrangement;
            row.effective_to = Some(end);
        }
    } else if row.delisting_date_tdx.is_none() {
        row.delisting_date_tdx = arrangement.end;
    }
}

/// Confirmed migrations first; a candidate for the same code pair is dropped
/// because the confirmed row already proves the link (§9.2).
fn merge_migrations(
    confirmed: Vec<CodeMigrationFact>,
    candidates: &[CodeMigrationFact],
) -> Result<Vec<CodeMigrationFact>, AssemblyError> {
    let mut pairs: BTreeSet<(Exchange, String, Exchange, String)> = BTreeSet::new();
    let mut keys: BTreeSet<(Exchange, String, NaiveDate)> = BTreeSet::new();
    let mut out = Vec::new();
    for migration in confirmed {
        if migration.from_exchange == migration.to_exchange
            && migration.from_code == migration.to_code
        {
            return Err(AssemblyError::InvalidMigration {
                from: migration.from_entity(),
                to: migration.to_entity(),
                detail: "a code migration must change the code".into(),
            });
        }
        if !keys.insert((
            migration.from_exchange,
            migration.from_code.clone(),
            migration.effective_from,
        )) {
            return Err(AssemblyError::InvalidMigration {
                from: migration.from_entity(),
                to: migration.to_entity(),
                detail: "duplicate (from, effective_from) in the exchange snapshot".into(),
            });
        }
        pairs.insert((
            migration.from_exchange,
            migration.from_code.clone(),
            migration.to_exchange,
            migration.to_code.clone(),
        ));
        out.push(migration);
    }
    for candidate in candidates {
        if candidate.certainty != MigrationCertainty::Candidate {
            return Err(AssemblyError::InvalidMigration {
                from: candidate.from_entity(),
                to: candidate.to_entity(),
                detail: "bar-evidence detection must produce candidates only".into(),
            });
        }
        if pairs.contains(&(
            candidate.from_exchange,
            candidate.from_code.clone(),
            candidate.to_exchange,
            candidate.to_code.clone(),
        )) {
            continue;
        }
        out.push(candidate.clone());
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// Collapse the old code at the migration day and start the new code there
/// (§4.1, §9.2). Only confirmed rows reach this function.
fn apply_confirmed_migration(
    by_code: &mut CodeIntervals,
    migration: &CodeMigrationFact,
    unresolved: &mut Vec<UnresolvedFact>,
) -> Result<(), AssemblyError> {
    let from_key = (migration.from_exchange, migration.from_code.clone());
    let to_key = (migration.to_exchange, migration.to_code.clone());
    let from_entity = migration.from_entity();
    let to_entity = migration.to_entity();

    let Some(from_rows) = by_code.get_mut(&from_key) else {
        unresolved.push(UnresolvedFact::MigrationWithoutFromInterval {
            from: from_entity,
            to: to_entity,
            effective_from: migration.effective_from,
        });
        return Ok(());
    };
    let Some(from_row) = from_rows.iter_mut().max_by_key(|row| row.effective_from) else {
        unresolved.push(UnresolvedFact::MigrationWithoutFromInterval {
            from: from_entity,
            to: to_entity,
            effective_from: migration.effective_from,
        });
        return Ok(());
    };
    if from_row.effective_from >= migration.effective_from {
        return Err(AssemblyError::InconsistentInterval {
            entity: from_entity,
            detail: format!(
                "the old code starts {} at or after the migration day {}",
                from_row.effective_from, migration.effective_from
            ),
        });
    }
    if from_row
        .delisting_date
        .is_some_and(|date| date > migration.effective_from)
    {
        return Err(AssemblyError::InconsistentInterval {
            entity: from_entity,
            detail: "the old code has a delisting date after the migration day".into(),
        });
    }
    if from_row
        .effective_to
        .is_none_or(|date| date > migration.effective_from)
    {
        from_row.effective_to = Some(migration.effective_from);
    }

    let Some(to_rows) = by_code.get_mut(&to_key) else {
        unresolved.push(UnresolvedFact::MigrationWithoutToInterval {
            from: from_entity,
            to: to_entity,
            effective_from: migration.effective_from,
        });
        return Ok(());
    };
    if to_rows.len() != 1 {
        return Err(AssemblyError::AmbiguousTdxRows {
            entity: to_entity,
            count: to_rows.len(),
        });
    }
    let to_row = &mut to_rows[0];
    if to_row.effective_from < migration.effective_from {
        if to_row
            .effective_to
            .is_some_and(|date| date <= migration.effective_from)
        {
            return Err(AssemblyError::InconsistentInterval {
                entity: to_entity,
                detail: "the new code ends before the migration day".into(),
            });
        }
        to_row.effective_from = migration.effective_from;
    }
    Ok(())
}

/// Resolve `(exchange, code)` at `day`. Exactly one interval is the normal
/// answer; two hits are `ambiguous_code_reuse` and are never merged (AC-M2-3).
pub fn lookup<'a>(
    rows: &'a [SecurityLifecycleFact],
    exchange: Exchange,
    code: &str,
    day: NaiveDate,
) -> Result<Option<&'a SecurityLifecycleFact>, LookupError> {
    let mut hit = None;
    let mut count = 0usize;
    for row in rows {
        if row.exchange != exchange || row.code != code {
            continue;
        }
        if row.effective_from > day {
            continue;
        }
        if row.effective_to.is_some_and(|to| day >= to) {
            continue;
        }
        count += 1;
        hit = Some(row);
    }
    match count {
        0 => Ok(None),
        1 => Ok(hit),
        rows => Err(LookupError::AmbiguousCodeReuse {
            entity: entity_of(exchange, code),
            day,
            rows,
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LookupError {
    #[error("ambiguous_code_reuse: {entity} matches {rows} security_master intervals at {day}")]
    AmbiguousCodeReuse {
        entity: String,
        day: NaiveDate,
        rows: usize,
    },
}

/// Content digest of the assembled dataset: `SHA256("security-master-v1" ‖
/// row_count ‖ rows… ‖ migration_count ‖ migrations…)`. The source binding and
/// the evidence bundle are excluded: they describe *where* the facts came
/// from, not the dataset identity (§3.10).
pub fn assembly_digest(
    rows: &[SecurityLifecycleFact],
    migrations: &[CodeMigrationFact],
) -> [u8; 32] {
    let mut payload = Vec::with_capacity(64 + rows.len() * 96 + migrations.len() * 48);
    payload.extend_from_slice(b"security-master-v1\0");
    payload.extend_from_slice(&(rows.len() as u64).to_be_bytes());
    for row in rows {
        push_text(&mut payload, row.exchange.as_str());
        push_text(&mut payload, &row.code);
        push_day(&mut payload, row.effective_from);
        push_opt_day(&mut payload, row.effective_to);
        payload.push(row.instrument_class);
        push_opt_text(&mut payload, row.issuer_key.as_deref());
        push_text(&mut payload, row.issuer_resolution.as_str());
        push_opt_day(&mut payload, row.listing_date);
        push_opt_day(&mut payload, row.listing_date_tdx);
        push_text(&mut payload, row.listing_source.as_str());
        push_opt_day(&mut payload, row.b_listing_date);
        push_opt_day(&mut payload, row.delisting_date);
        push_opt_day(&mut payload, row.delisting_date_tdx);
        push_text(&mut payload, row.delisting_source.as_str());
        push_text(&mut payload, row.delisting_reason_class.as_str());
        push_opt_day(&mut payload, row.arrangement_start);
        push_opt_day(&mut payload, row.arrangement_end);
        payload.push(row.listing_conflict as u8);
        payload.push(row.delisting_conflict as u8);
    }
    payload.extend_from_slice(&(migrations.len() as u64).to_be_bytes());
    for migration in migrations {
        push_text(&mut payload, migration.from_exchange.as_str());
        push_text(&mut payload, &migration.from_code);
        push_text(&mut payload, migration.to_exchange.as_str());
        push_text(&mut payload, &migration.to_code);
        push_day(&mut payload, migration.effective_from);
        push_text(&mut payload, migration.certainty.as_str());
        push_text(&mut payload, &migration.source_kind);
        push_opt_text(&mut payload, migration.source_record_id.as_deref());
        push_opt_day(&mut payload, migration.known_at);
    }
    Sha256::digest(&payload).into()
}

fn push_text(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u64).to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn push_opt_text(out: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            out.push(1);
            push_text(out, value);
        }
        None => out.push(0),
    }
}

fn push_day(out: &mut Vec<u8>, day: NaiveDate) {
    out.extend_from_slice(&crate::compact_market::epoch_day(day).to_be_bytes());
}

fn push_opt_day(out: &mut Vec<u8>, day: Option<NaiveDate>) {
    match day {
        Some(day) => {
            out.push(1);
            push_day(out, day);
        }
        None => out.push(0),
    }
}

#[cfg(test)]
mod tests;
