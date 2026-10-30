//! Shared exchange-source entry (M3 PR-2, design
//! `docs/tasks/issue-446-m3-delisted-industry.md` §6 and §9.4).
//!
//! Five consumers (A-1 listing, A-2 delisting, historical ST, A-3 suspension,
//! S-6 short names) need the same four steps and must not each invent them:
//!
//! ```text
//! fetch    → bounded retries, Referer/UA, canonical request fingerprint
//! freeze   → raw bytes into the source store (§9.4.1) *before* parsing
//! validate → HTTP status, format, page counts, segment counts, 0-row policy
//! bind     → snapshot ids + parser version + basis status → binding digest
//! ```
//!
//! Consumers only supply `source_id`, the request template, their parser and
//! their writer; PR-2 deliberately ships **no** source-specific parser or
//! writer (those are PR-3/PR-10/PR-11/PR-12).
//!
//! ## Where the socket lives
//!
//! The Rust side never holds one. `src/acquire/fetch.rs` shells out to the
//! stdlib-only `scripts/tdx_fetch.py` helper and this entry follows the same
//! boundary: [`transport::Helper`] runs `scripts/exchange_fetch.py` as a child
//! process and only moves bytes/metadata. Networking therefore needs exactly
//! the runtime permission the acquire unit already has, and the helper is
//! packaged next to `tdx_fetch.py` by `scripts/build_release.sh`. The entry is
//! called from the acquire process, so no new unit, socket or sandbox rule is
//! introduced.
//!
//! ## Write order (forced, §9.4.1)
//!
//! `blob + manifest line (fsynced) → exchange_source_snapshots row`. The
//! freeze step registers the response bytes before any parser runs, so a
//! parse failure keeps the evidence (and the failure is reported, never
//! silently dropped). Replay rebuilds the index from the manifest alone
//! (no HTTP) and a byte digest mismatch is `source_store_corrupt`.
//!
//! ## What this PR does not do
//!
//! No real network fetch in CI (fixtures only), no source-specific parser, no
//! consumer writer, no deployment change. `docs/exchange-source.md` documents
//! the entry and its failure classes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub mod bind;
pub mod fetch;
pub mod freeze;
pub mod index;
pub mod request;
pub mod transport;
pub mod validate;

#[cfg(test)]
mod tests;

pub use bind::{BindError, BoundSnapshot, SnapshotBinding};
pub use fetch::{
    AcquireError, AcquisitionOutcome, AcquisitionPlan, Entry, RunCache, WindowFailure,
    WindowOutcome, WindowResult,
};
pub use freeze::{
    FreezeError, FrozenBytes, FrozenSnapshot, freeze_bytes, freeze_page, now, record_snapshots,
};
pub use index::{Rebuild, RebuildProblem, SnapshotRow, rebuild, summary};
pub use request::{
    BULLETIN_ARCHIVE_START, DateFormat, ExchangeRequest, MAX_SEGMENT_DAYS, PagingFormat,
    RequestError, Segment, quarter_segments,
};
pub use transport::{
    Client, Helper, HttpRequest, HttpResponse, RetryPolicy, Transport, TransportError,
};
pub use validate::{EmptyPolicy, FailureClass, PageEnvelope, PageSet, ValidateError, ZeroPolicy};

/// Logical source ids of the five consumers (§6.2 and the schema-25 comment).
///
/// `snapshot_id` (the response-byte sha256) is what a binding names; this enum
/// is only the registration key, so a new consumer adds one value here and one
/// parser/writer pair — never a new fetch path (§12 separation point 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceId {
    SseListing,
    SseDelisting,
    SseBulletin,
    SseSuspension,
    SzseListing,
    SzseDelisting,
    SzseShortName,
}

impl SourceId {
    pub const ALL: [SourceId; 7] = [
        Self::SseListing,
        Self::SseDelisting,
        Self::SseBulletin,
        Self::SseSuspension,
        Self::SzseListing,
        Self::SzseDelisting,
        Self::SzseShortName,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SseListing => "sse_listing",
            Self::SseDelisting => "sse_delisting",
            Self::SseBulletin => "sse_bulletin",
            Self::SseSuspension => "sse_suspension",
            Self::SzseListing => "szse_listing",
            Self::SzseDelisting => "szse_delisting",
            Self::SzseShortName => "szse_short_name",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|source| source.as_str() == value)
    }

    /// Referer the exchange APIs require (§6.3); the request builder adds it.
    pub fn referer(self) -> &'static str {
        match self {
            Self::SseListing | Self::SseDelisting | Self::SseBulletin | Self::SseSuspension => {
                "https://www.sse.com.cn/"
            }
            Self::SzseListing | Self::SzseDelisting | Self::SzseShortName => "https://www.szse.cn/",
        }
    }

    /// Owner-assigned default tier (§6.2, D-45/D-46): every consumer that has a
    /// writer today is an `approved_source`; the ST and suspension consumers
    /// downgrade individual snapshots per the design, not per source.
    pub fn default_basis(self) -> BasisStatus {
        BasisStatus::ApprovedSource
    }
}

/// Evidence tier of one frozen snapshot (§6.2, §6.3 "失败语义").
///
/// The order is the admission order: a writer that requires
/// [`BasisStatus::ApprovedSource`] refuses `owner_risk_accepted` and
/// `not_available` rows (`source_not_admitted`), instead of writing 0 rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BasisStatus {
    NotAvailable,
    OwnerRiskAccepted,
    ApprovedSource,
}

impl BasisStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotAvailable => "not_available",
            Self::OwnerRiskAccepted => "owner_risk_accepted",
            Self::ApprovedSource => "approved_source",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "not_available" => Some(Self::NotAvailable),
            "owner_risk_accepted" => Some(Self::OwnerRiskAccepted),
            "approved_source" => Some(Self::ApprovedSource),
            _ => None,
        }
    }

    /// Admission rank; only meaningful through [`BasisStatus::admits`].
    pub fn rank(self) -> u8 {
        match self {
            Self::NotAvailable => 0,
            Self::OwnerRiskAccepted => 1,
            Self::ApprovedSource => 2,
        }
    }

    /// True when a writer requiring `required` may use this snapshot.
    pub fn admits(self, required: Self) -> bool {
        self.rank() >= required.rank()
    }
}

/// Headers every exchange request sends (§6.3: "Referer/UA").
pub fn default_headers(source: SourceId) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert(
        "Accept".to_string(),
        "application/json, text/javascript, */*".to_string(),
    );
    headers.insert("Referer".to_string(), source.referer().to_string());
    headers.insert("User-Agent".to_string(), transport::USER_AGENT.to_string());
    headers
}
