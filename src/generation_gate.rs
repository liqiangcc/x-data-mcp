//! Generation retirement gate (M3 PR-8, issue `#446` §8.3/§9.4.3).
//!
//! Retiring a generation deletes its `base.zip`/`gbbq.zip`/`hsjday.zip`
//! bytes. Before that may happen, two questions must be answered with
//! evidence, not with "someone remembered":
//!
//! 1. **Source-store evidence (authoritative, fail-closed)** — the
//!    irreproducible members this generation carries
//!    (`base.zip!tdxhy.cfg`, `gbbq.zip!zhb.zip!incon.dat`) are registered in
//!    the source store under this generation's manifest sha, and the blob
//!    reads back byte for byte. A missing registration, a missing blob or a
//!    digest mismatch refuses the retire: only "read the bytes back" counts.
//! 2. **Derived-fact index (acceleration)** — `generation_facts` holds a row
//!    for every required `(role, fact_kind)`. The table is rebuildable from
//!    the source store plus the derived tables
//!    (`source_store replay --scope generation_facts`), so a missing row is
//!    a hint to replay, never proof that the facts are gone.
//!
//! Both the manual `retire_generation` path and the automatic "keep the
//! newest two superseded generations" retention call [`RetireGate::check`]
//! through the same acquire helper. An exemption (`retire_exemptions`) can
//! name a missing key; evidence exemptions additionally require an explicit
//! owner approval when the row is written (see `source_store exempt`).
//!
//! **Process boundary (D-56).** Retirement runs in the acquire process,
//! which must not open the market database writable: the single-writer lease
//! belongs to `data_service`. [`MarketFactIndex`] therefore opens its own
//! read-only connection (`SqliteTasks::connect` with `readonly = true`, the
//! same path `task_sqlite check` uses). WAL readers do
//! not take the writer lease; they only need the `-shm` file the live writer
//! already maintains, so a read cannot block, checkpoint or otherwise change
//! the database. Without a database path the gate refuses instead of
//! deleting bytes unchecked (`retire_gate_unconfigured`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::hash::sha256;
use crate::source_store::{
    INCON_PARSER_VERSION, INCON_SOURCE_ID, SourceStore, StoreError, TDXHY_PARSER_VERSION,
    TDXHY_SOURCE_ID,
};

/// Role of the base archive (industry snapshot source).
pub const ROLE_BASE: &str = "base.zip";
/// Role of the corporate-action archive (industry dictionary + events).
pub const ROLE_GBBQ: &str = "gbbq.zip";
/// Role of the day archive (daily series).
pub const ROLE_DAY: &str = "hsjday.zip";

/// Industry snapshot fact (`base.zip!tdxhy.cfg`).
pub const KIND_INDUSTRY_SNAPSHOT: &str = "industry_snapshot";
/// Industry dictionary fact (`gbbq.zip!zhb.zip!incon.dat`).
pub const KIND_INDUSTRY_DICTIONARY: &str = "industry_dictionary";
/// Capital (share count) events derived from `gbbq.zip`.
pub const KIND_CAPITAL_EVENTS: &str = "capital_events";
/// Adjustment (cat-1 factor) events derived from `gbbq.zip`.
pub const KIND_ADJUSTMENT_EVENTS: &str = "adjustment_events";
/// Daily OHLCV series derived from `hsjday.zip`.
pub const KIND_DAILY_SERIES: &str = "daily_series";

/// The facts a generation must have registered before its bytes may go
/// (§8.3, initial set). Both the writer and the gate read this table, so the
/// required set cannot drift between "what the daily run records" and "what
/// the retire checks".
pub const REQUIRED_FACTS: [(&str, &str); 6] = [
    (ROLE_BASE, KIND_INDUSTRY_SNAPSHOT),
    (ROLE_BASE, KIND_INDUSTRY_DICTIONARY),
    (ROLE_GBBQ, KIND_INDUSTRY_DICTIONARY),
    (ROLE_GBBQ, KIND_CAPITAL_EVENTS),
    (ROLE_GBBQ, KIND_ADJUSTMENT_EVENTS),
    (ROLE_DAY, KIND_DAILY_SERIES),
];

/// The irreproducible source-store members a generation must have
/// registered (§8.3 step 2): `(role, source_id)`.
pub const REQUIRED_SOURCE_EVIDENCE: [(&str, &str); 2] =
    [(ROLE_BASE, TDXHY_SOURCE_ID), (ROLE_GBBQ, INCON_SOURCE_ID)];

/// Prefix of the exemption key that covers a missing source-store host.
pub const EVIDENCE_KEY_PREFIX: &str = "source_store_evidence";
/// Prefix of the exemption key that covers a missing derived fact.
pub const FACT_KEY_PREFIX: &str = "derived_fact";

/// One registered derived fact (`generation_facts` row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationFact {
    pub generation_manifest_sha256: String,
    pub role: String,
    pub fact_kind: String,
    /// Content digest of the derived fact (hex sha256), the replay's
    /// `match`/`drift` unit.
    pub fact_digest: String,
    /// Source-store blob the fact was derived from; mandatory for
    /// irreproducible facts and NULL for facts that the derived tables
    /// already explain.
    pub evidence_sha256: Option<String>,
    pub parser_version: Option<String>,
    pub registered_at: String,
}

impl GenerationFact {
    /// The gate's `(role, fact_kind)` key.
    pub fn key(&self) -> (String, String) {
        (self.role.clone(), self.fact_kind.clone())
    }
}

/// One `retire_exemptions` row: the explicit, auditable exception path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetireExemption {
    pub generation_manifest_sha256: String,
    pub missing_kind: String,
    pub reason: String,
    pub recorded_by: String,
    pub recorded_at: String,
}

/// Absence of one required source-store evidence registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceGap {
    pub role: String,
    pub source_id: String,
    /// The registered blob digest, when a registration exists but does not
    /// read back. `None` means "no manifest line for this generation".
    pub sha256: Option<String>,
    /// Store-level reason when the bytes did not read back.
    pub problem: Option<String>,
}

impl EvidenceGap {
    pub fn key(&self) -> String {
        format!("{EVIDENCE_KEY_PREFIX}:{}:{}", self.role, self.source_id)
    }
}

/// Absence of one required derived-fact registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactGap {
    pub role: String,
    pub fact_kind: String,
}

impl FactGap {
    pub fn key(&self) -> String {
        format!("{FACT_KEY_PREFIX}:{}:{}", self.role, self.fact_kind)
    }
}

/// What the gate found. `missing_*` excludes keys covered by an exemption;
/// `exemptions` names the keys that were used, and is carried into the retire
/// reply so the audit trail is visible without a second query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GateCheck {
    pub missing_evidence: Vec<EvidenceGap>,
    pub missing_facts: Vec<FactGap>,
    pub exemptions: Vec<String>,
}

impl GateCheck {
    pub fn passes(&self) -> bool {
        self.missing_evidence.is_empty() && self.missing_facts.is_empty()
    }

    /// Stable reason code of the first failing step, or `None` when the gate
    /// passes. Source-store evidence is authoritative, so it reports first.
    pub fn reason_code(&self) -> Option<&'static str> {
        if !self.missing_evidence.is_empty() {
            return Some("source_store_evidence_missing");
        }
        if !self.missing_facts.is_empty() {
            return Some("derived_facts_missing");
        }
        None
    }

    /// Operator-facing detail: the `(role, source_id, sha256)` list for
    /// evidence, the `(role, missing kinds)` list for facts, plus the used
    /// exemptions.
    pub fn detail(&self) -> String {
        let mut parts = Vec::new();
        if !self.missing_evidence.is_empty() {
            parts.push(format!(
                "missing source-store evidence (role|source_id|sha256): {}",
                self.missing_evidence
                    .iter()
                    .map(|gap| match (&gap.sha256, &gap.problem) {
                        (Some(sha), Some(problem)) => {
                            format!("{}|{}|{}|{problem}", gap.role, gap.source_id, sha)
                        }
                        (Some(sha), None) => format!("{}|{}|{sha}", gap.role, gap.source_id),
                        (None, _) => format!("{}|{}|-", gap.role, gap.source_id),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !self.missing_facts.is_empty() {
            let mut by_role: std::collections::BTreeMap<&str, Vec<&str>> = Default::default();
            for gap in &self.missing_facts {
                by_role
                    .entry(gap.role.as_str())
                    .or_default()
                    .push(gap.fact_kind.as_str());
            }
            parts.push(format!(
                "missing derived facts: {} (run `source_store replay --scope generation_facts --db <market.sqlite>` to rebuild the index)",
                by_role
                    .iter()
                    .map(|(role, kinds)| format!("{role}|{}", kinds.join("+")))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !self.exemptions.is_empty() {
            parts.push(format!("exemptions: {}", self.exemptions.join(",")));
        }
        parts.join("; ")
    }
}

/// Read-only view of the market database's gate rows. Opens a fresh
/// read-only connection per check and closes it: the acquire service holds a
/// lease on its *own* store, never on the market database, so there is
/// nothing to keep open.
#[derive(Debug, Clone)]
pub struct MarketFactIndex {
    database: PathBuf,
}

impl MarketFactIndex {
    pub fn new(database: impl Into<PathBuf>) -> Self {
        Self {
            database: database.into(),
        }
    }

    pub fn database(&self) -> &Path {
        &self.database
    }
}

impl MarketFactIndex {
    /// One read-only snapshot of both tables: the gate must not see facts
    /// from one transaction and exemptions from another.
    fn state(
        &self,
        generation_manifest_sha256: &str,
    ) -> Result<(Vec<GenerationFact>, Vec<RetireExemption>), String> {
        crate::sqlite_market::generation_facts::read_state_readonly(
            &self.database,
            generation_manifest_sha256,
        )
        .map_err(|error| error.to_string())
    }
}

/// Errors of the gate's read paths (store unreadable, database unavailable).
#[derive(Debug, thiserror::Error)]
pub enum GateError {
    #[error("source store: {0}")]
    Store(#[from] StoreError),
    #[error("fact index: {0}")]
    Index(String),
}

/// The gate itself: the authoritative source store plus the fact index.
#[derive(Debug, Clone)]
pub struct RetireGate {
    store: SourceStore,
    index: MarketFactIndex,
}

impl RetireGate {
    pub fn new(store: SourceStore, database: impl Into<PathBuf>) -> Self {
        Self {
            store,
            index: MarketFactIndex::new(database),
        }
    }

    pub fn store(&self) -> &SourceStore {
        &self.store
    }

    pub fn database(&self) -> &Path {
        self.index.database()
    }

    /// Check one generation before its bytes are deleted.
    pub fn check(&self, generation_manifest_sha256: &str) -> Result<GateCheck, GateError> {
        let (facts, exemptions) = self
            .index
            .state(generation_manifest_sha256)
            .map_err(GateError::Index)?;
        evaluate(&self.store, generation_manifest_sha256, &facts, &exemptions)
    }
}

/// Pure gate evaluation: the same function serves the retire path and the
/// tests, so "manual and automatic retire use one gate" is structural.
pub fn evaluate(
    store: &SourceStore,
    generation_manifest_sha256: &str,
    facts: &[GenerationFact],
    exemptions: &[RetireExemption],
) -> Result<GateCheck, GateError> {
    let exempted: BTreeSet<&str> = exemptions
        .iter()
        .map(|row| row.missing_kind.as_str())
        .collect();
    let mut used = Vec::new();

    // Step 2 — source-store evidence is authoritative: registration under the
    // generation's manifest sha plus a byte-verified read-back.
    let records = store.records()?;
    let origin = format!("generation:{generation_manifest_sha256}");
    let mut missing_evidence = Vec::new();
    for (role, source_id) in REQUIRED_SOURCE_EVIDENCE {
        let registered: Vec<_> = records
            .iter()
            .filter(|record| {
                record.origin == origin
                    && record.source_id == source_id
                    && record.store_kind == crate::source_store::StoreKind::GenerationMember
            })
            .collect();
        if registered.is_empty() {
            let gap = EvidenceGap {
                role: role.to_string(),
                source_id: source_id.to_string(),
                sha256: None,
                problem: None,
            };
            if !exempted.contains(gap.key().as_str()) {
                missing_evidence.push(gap);
            } else {
                used.push(gap.key());
            }
            continue;
        }
        let mut verified = false;
        let mut failure = None;
        for record in &registered {
            match store.read_verified(&record.sha256) {
                Ok(_) => {
                    verified = true;
                    break;
                }
                Err(error) => failure = Some((record.sha256.clone(), error.to_string())),
            }
        }
        if !verified {
            let (sha256, problem) = failure.unwrap_or_default();
            let gap = EvidenceGap {
                role: role.to_string(),
                source_id: source_id.to_string(),
                sha256: Some(sha256),
                problem: Some(problem),
            };
            if !exempted.contains(gap.key().as_str()) {
                missing_evidence.push(gap);
            } else {
                used.push(gap.key());
            }
        }
    }

    // Step 3 — derived-fact index (rebuildable; missing means "replay first").
    let registered: BTreeSet<(String, String)> = facts
        .iter()
        .map(|fact| (fact.role.clone(), fact.fact_kind.clone()))
        .collect();
    let mut missing_facts = Vec::new();
    for (role, fact_kind) in REQUIRED_FACTS {
        let gap = FactGap {
            role: role.to_string(),
            fact_kind: fact_kind.to_string(),
        };
        if registered.contains(&(role.to_string(), fact_kind.to_string())) {
            continue;
        }
        if exempted.contains(gap.key().as_str()) {
            used.push(gap.key());
        } else {
            missing_facts.push(gap);
        }
    }

    Ok(GateCheck {
        missing_evidence,
        missing_facts,
        exemptions: used,
    })
}

/// The industry facts a successful `derived:industry_snapshot` item registers
/// (§8.3): snapshot + dictionary under `base.zip`, dictionary under `gbbq.zip`.
///
/// Both `evidence_sha256` values are read back from the store before the rows
/// are returned, so a registration cannot name bytes the store does not hold
/// (§8.2: irreproducible facts must be non-empty and verified at registration
/// time).
pub fn industry_fact_rows(
    store: &SourceStore,
    generation_manifest_sha256: &str,
    tdxhy_sha256: &str,
    incon_sha256: &str,
) -> Result<Vec<GenerationFact>, GateError> {
    store.read_verified(tdxhy_sha256)?;
    store.read_verified(incon_sha256)?;
    let now = timestamp(Utc::now());
    let snapshot_digest = industry_snapshot_digest(generation_manifest_sha256, tdxhy_sha256);
    let dictionary_digest = industry_dictionary_digest(generation_manifest_sha256, incon_sha256);
    Ok(vec![
        GenerationFact {
            generation_manifest_sha256: generation_manifest_sha256.to_string(),
            role: ROLE_BASE.into(),
            fact_kind: KIND_INDUSTRY_SNAPSHOT.into(),
            fact_digest: snapshot_digest,
            evidence_sha256: Some(tdxhy_sha256.to_string()),
            parser_version: Some(TDXHY_PARSER_VERSION.into()),
            registered_at: now.clone(),
        },
        GenerationFact {
            generation_manifest_sha256: generation_manifest_sha256.to_string(),
            role: ROLE_BASE.into(),
            fact_kind: KIND_INDUSTRY_DICTIONARY.into(),
            fact_digest: dictionary_digest.clone(),
            evidence_sha256: Some(incon_sha256.to_string()),
            parser_version: Some(INCON_PARSER_VERSION.into()),
            registered_at: now.clone(),
        },
        GenerationFact {
            generation_manifest_sha256: generation_manifest_sha256.to_string(),
            role: ROLE_GBBQ.into(),
            fact_kind: KIND_INDUSTRY_DICTIONARY.into(),
            fact_digest: dictionary_digest,
            evidence_sha256: Some(incon_sha256.to_string()),
            parser_version: Some(INCON_PARSER_VERSION.into()),
            registered_at: now,
        },
    ])
}

/// Rebuild the industry fact rows from the source-store manifest alone, the
/// `--db`-less half of `replay --scope generation_facts`.
///
/// A generation is identified by the `generation:<manifest sha>` origin PR-6
/// writes; the required `tdxhy.cfg`/`incon.dat` digests are the registered
/// member digests, so the rows are identical to [`industry_fact_rows`].
pub fn industry_fact_rows_from_store(
    store: &SourceStore,
) -> Result<Vec<GenerationFact>, GateError> {
    let records = store.records()?;
    let mut origins: BTreeSet<String> = BTreeSet::new();
    for record in &records {
        if record.store_kind == crate::source_store::StoreKind::GenerationMember
            && (record.source_id == TDXHY_SOURCE_ID || record.source_id == INCON_SOURCE_ID)
            && let Some(generation) = record.origin.strip_prefix("generation:")
        {
            origins.insert(generation.to_string());
        }
    }
    let now = timestamp(Utc::now());
    let mut rows = Vec::new();
    for generation in origins {
        let origin = format!("generation:{generation}");
        let latest = |source_id: &str| -> Option<String> {
            records
                .iter()
                .filter(|record| {
                    record.origin == origin
                        && record.source_id == source_id
                        && record.store_kind == crate::source_store::StoreKind::GenerationMember
                })
                .map(|record| record.sha256.clone())
                .next_back()
        };
        let (Some(tdxhy), Some(incon)) = (latest(TDXHY_SOURCE_ID), latest(INCON_SOURCE_ID)) else {
            // A generation with only one of the two members has no complete
            // industry fact; the gate refuses it rather than inventing rows.
            continue;
        };
        for mut row in industry_fact_rows(store, &generation, &tdxhy, &incon)
            .map_err(|error| GateError::Index(error.to_string()))?
        {
            row.registered_at = now.clone();
            rows.push(row);
        }
    }
    Ok(rows)
}

/// Content digest of the industry snapshot fact: the generation identity plus
/// the member digest the snapshot row is keyed by.
pub fn industry_snapshot_digest(generation_manifest_sha256: &str, member_sha256: &str) -> String {
    sha256(
        format!(
            "generation-fact|{KIND_INDUSTRY_SNAPSHOT}|{generation_manifest_sha256}|{member_sha256}"
        )
        .as_bytes(),
    )
}

/// Content digest of the industry dictionary fact.
pub fn industry_dictionary_digest(
    generation_manifest_sha256: &str,
    dictionary_sha256: &str,
) -> String {
    sha256(
        format!(
            "generation-fact|{KIND_INDUSTRY_DICTIONARY}|{generation_manifest_sha256}|{dictionary_sha256}"
        )
        .as_bytes(),
    )
}

/// RFC 3339 seconds in UTC, the timestamp shape the gate tables store.
pub fn timestamp(moment: chrono::DateTime<Utc>) -> String {
    moment.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Canonical summary of a generation-facts row set (§9.4.2): fixed fields,
/// sorted, sha256 over the joined lines. `registered_at` is deliberately
/// excluded — it records when the row was written, not what it says.
pub fn facts_summary(facts: &[GenerationFact]) -> String {
    let mut lines: Vec<String> = facts
        .iter()
        .map(|fact| {
            format!(
                "{}|{}|{}|{}|{}",
                fact.generation_manifest_sha256,
                fact.role,
                fact.fact_kind,
                fact.fact_digest,
                fact.evidence_sha256.as_deref().unwrap_or(""),
            )
        })
        .collect();
    lines.sort();
    lines.dedup();
    sha256(lines.join("\n").as_bytes())
}

/// Stable primary-key list for the replay's drift report, capped by `limit`.
pub fn drift_keys(
    computed: &[GenerationFact],
    stored: &[GenerationFact],
    limit: usize,
) -> Vec<String> {
    let key = |facts: &[GenerationFact]| -> BTreeSet<String> {
        facts
            .iter()
            .map(|fact| {
                format!(
                    "{}|{}|{}|{}",
                    fact.generation_manifest_sha256, fact.role, fact.fact_kind, fact.fact_digest
                )
            })
            .collect()
    };
    let computed = key(computed);
    let stored = key(stored);
    computed
        .symmetric_difference(&stored)
        .take(limit)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests;
