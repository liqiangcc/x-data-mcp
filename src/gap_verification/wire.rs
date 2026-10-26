//! Closed protocol shape for `verify_data_gap`: the caller names only the
//! target; conclusion, digests, subject version and verifier identity are
//! server-derived and can never arrive over the wire (deny_unknown_fields
//! rejects any pass-through field at decode).
use crate::data_supply::wire::ContractVersion;
use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
/// `verify_data_gap` input: `request_id` + target only.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifyDataGap {
    pub contract_version: ContractVersion,
    pub request_id: String,
    pub target: VerifyGapTarget,
}
/// Which day range of which symbol to verify. `period` is a closed enum:
/// `year` (or anything else) fails decode — yearly stays derived.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifyGapTarget {
    pub symbol: String,
    pub period: VerifyGapPeriod,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum VerifyGapPeriod {
    Day,
}
impl VerifyDataGap {
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    /// Deterministic minimal intent for the contract: the wire envelope
    /// (request_id/contract_version) never enters the normalized command,
    /// so request_id replay matching stays deterministic.
    pub fn payload(&self) -> Value {
        serde_json::json!({"target": self.target})
    }
}
pub const DESCRIPTION: &str = "Verify a claimed market data gap for one symbol and range. Read-only; local owner only. Replaying request_id returns the stored receipt.";
