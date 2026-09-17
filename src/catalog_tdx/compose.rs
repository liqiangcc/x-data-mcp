//! Composition layer S4–S6 (design: docs/tasks/issue-6-catalog-impl.md §2).
//!
//! Joins scoped members to `infoharbor_ex.code` names (S4), resolves the
//! TDX research-industry level-2 assignment through `incon.dat` `#TDXRSHY`
//! (S5), and assembles the `CatalogSnapshot` consumed by
//! `CatalogService::refresh` (S6). Every `Assigned` record carries
//! `IndustryScheme::TdxResearchIndustry`; no other scheme is produced.
//!
//! Product-decision rulings honored here (#6):
//! - P1 `reject_candidate` (ruled by comment 5710586446): a scope member
//!   without a name row is a governed exclusion — it never enters the
//!   candidate, is enumerated verbatim in `nameless`, and does not block
//!   publication of the named remainder. No name is guessed or derived
//!   from code patterns.
//! - P2 scheme label: records carry `tdx_research_industry`; this module
//!   never emits an eastmoney label or an unlabeled production assignment.
//!
//! Diagnostics are computed over published (named) members only — an
//! excluded nameless member must not appear in `missing_industry` or
//! `t_code_residuals` — so the audit trail reports exactly what the
//! published candidate contains.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};

use super::{
    incon::{self, InconSection},
    scope::ScopedMember,
};
use crate::catalog::{
    CatalogSnapshot, CompletenessEvidence, Exchange, IndustryInfo, IndustryScheme, SecurityRecord,
    SecurityType,
};

/// Member identity: (tdxhy.cfg market digit, six-digit code). Leading zeroes
/// are preserved by construction.
pub type MemberId = (u8, String);

/// A member whose parallel T-classification code is absent from `#TDXNHY`.
/// Reported verbatim; the code is never bound to a name (contract residual,
/// e.g. `688688` → `T00`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TCodeResidual {
    pub member: MemberId,
    pub t_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeOutcome {
    /// The assembled candidate snapshot of named members (P1 ruling
    /// 5710586446: nameless members are governed exclusions). `None` only
    /// when nothing is publishable; consumers must not publish a `None`
    /// snapshot or rebuild one from diagnostics.
    pub snapshot: Option<CatalogSnapshot>,
    /// Scope members with no `infoharbor_ex.code` name row.
    pub nameless: Vec<MemberId>,
    /// Published members kept with `IndustryInfo::Missing` per AC-05:
    /// their level-2 code could not be derived or is absent from
    /// `#TDXRSHY`. Excluded nameless members never appear here.
    pub missing_industry: Vec<MemberId>,
    /// T-codes of published members not present in `#TDXNHY` (checked
    /// only when that section exists; `t_dictionary_checked` records
    /// whether the check ran).
    pub t_code_residuals: Vec<TCodeResidual>,
    pub t_dictionary_checked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ComposeError {
    #[error("incon.dat has no #{0} section")]
    MissingDictionary(&'static str),
    #[error("scope member {0:?} has market digit outside the anchored {{0,1}} range")]
    BadMarket(MemberId),
}

/// Compose one refresh candidate. `source` must already bind the inspected
/// snapshot identity (e.g. `official-file:<manifest sha16>`); `acquired_at`
/// is the manifest acquisition time, never an industry effective time.
pub fn compose(
    members: &[ScopedMember],
    names: &BTreeMap<&str, &str>,
    sections: &[InconSection],
    source: &str,
    acquired_at: DateTime<Utc>,
) -> Result<ComposeOutcome, ComposeError> {
    let research: BTreeMap<&str, &str> = incon::section_map(sections, "TDXRSHY")
        .ok_or(ComposeError::MissingDictionary("TDXRSHY"))?
        .into_iter()
        .collect();
    let t_dictionary: Option<BTreeSet<&str>> = incon::section_map(sections, "TDXNHY")
        .map(|entries| entries.into_iter().map(|(code, _)| code).collect());

    let mut records = Vec::with_capacity(members.len());
    let mut nameless = Vec::new();
    let mut missing_industry = Vec::new();
    let mut t_code_residuals = Vec::new();

    for member in members {
        let id: MemberId = (member.market, member.code.clone());
        let exchange = match member.market {
            0 => Exchange::Shenzhen,
            1 => Exchange::Shanghai,
            _ => return Err(ComposeError::BadMarket(id)),
        };
        // P1 ruling 5710586446: a nameless scope member is a governed
        // exclusion, enumerated verbatim — industry diagnostics apply only
        // to members that actually publish.
        let Some(name) = names.get(member.code.as_str()) else {
            nameless.push(id);
            continue;
        };
        let industry = match incon::research_level2(&member.x_code) {
            None => {
                missing_industry.push(id.clone());
                IndustryInfo::Missing {
                    reason: format!(
                        "tdx_research: no level-2 code derivable from {}",
                        member.x_code
                    ),
                }
            }
            Some(l2) => match research.get(l2.as_str()) {
                Some(name) => IndustryInfo::Assigned {
                    code: l2,
                    name: (*name).to_string(),
                    scheme: Some(IndustryScheme::TdxResearchIndustry),
                },
                None => {
                    missing_industry.push(id.clone());
                    IndustryInfo::Missing {
                        reason: format!("tdx_research: level-2 code {l2} absent from TDXRSHY"),
                    }
                }
            },
        };
        if let Some(t_dictionary) = &t_dictionary
            && !t_dictionary.contains(member.t_code.as_str())
        {
            t_code_residuals.push(TCodeResidual {
                member: id.clone(),
                t_code: member.t_code.clone(),
            });
        }
        records.push(SecurityRecord {
            symbol: member.code.clone(),
            name: (*name).to_string(),
            exchange,
            security_type: SecurityType::AShare,
            source_security_id: format!("tdxhy:{}:{}", member.market, member.code),
            industry,
            source: source.to_string(),
            acquired_at,
        });
    }

    let snapshot = if records.is_empty() {
        None
    } else {
        Some(CatalogSnapshot {
            source: source.to_string(),
            acquired_at,
            // Complete here means: the member parsed fully, the scope +
            // exclusion partition reconciled, and every published member
            // is present — not market-wide completeness. Governed
            // exclusions are enumerated in `nameless`, never dropped
            // silently.
            completeness: CompletenessEvidence::Complete {
                expected_count: records.len(),
            },
            records,
        })
    };
    Ok(ComposeOutcome {
        snapshot,
        nameless,
        missing_industry,
        t_code_residuals,
        t_dictionary_checked: t_dictionary.is_some(),
    })
}
