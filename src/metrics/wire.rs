use crate::data_supply::wire::{ContractVersion, VersionRef};
use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Compute {
    pub contract_version: ContractVersion,
    #[schemars(length(min = 1, max = 1))]
    pub inputs: Vec<Input>,
    pub range: Years,
    #[schemars(length(min = 1, max = 1))]
    pub metrics: Vec<Metric>,
    pub execution: Execution,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Execution {
    #[serde(rename = "inline")]
    Inline,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub input_id: String,
    pub version_ref: VersionRef,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Years {
    #[schemars(range(min = 1, max = 9999))]
    pub start_year: i32,
    #[schemars(range(min = 1, max = 9999))]
    pub end_year: i32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Metric {
    pub metric_id: String,
    pub name: MetricName,
    pub algorithm_version: Algorithm,
    #[schemars(length(min = 1, max = 1))]
    pub input_ids: Vec<String>,
    pub parameters: Parameters,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum MetricName {
    #[serde(rename = "consecutive_direction")]
    ConsecutiveDirection,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Algorithm {
    #[serde(rename = "local-consecutive-direction-v1")]
    V1,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Up,
    Down,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Parameters {
    pub direction: Direction,
    #[schemars(range(min = 1, max = 1000))]
    pub min_periods: usize,
    #[serde(default)]
    pub price_field: PriceField,
    #[serde(default = "yes")]
    pub completed_only: bool,
    #[serde(default)]
    pub equal_policy: EqualPolicy,
    #[serde(default)]
    pub gap_policy: GapPolicy,
}
fn yes() -> bool {
    true
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum PriceField {
    #[default]
    #[serde(rename = "close")]
    Close,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum EqualPolicy {
    #[default]
    #[serde(rename = "break")]
    Break,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum GapPolicy {
    #[default]
    #[serde(rename = "reject")]
    Reject,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub year: i32,
    pub first_trading_date: NaiveDate,
    pub last_trading_date: NaiveDate,
    pub close: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ObservedDirection {
    Up,
    Down,
    Flat,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Exact,
    LowerBound,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    WindowTooShort,
    MissingYear,
    UnknownCompletion,
    IncompletePeriod,
    NonPositivePrice,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Precision {
    pub scale: u32,
    pub rounding: String,
    pub unit: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Computed {
        observed_direction: ObservedDirection,
        run_length: usize,
        run_length_kind: RunKind,
        matches: Option<bool>,
        anchor: Option<Point>,
        first_change: Option<Point>,
        end: Point,
        cumulative_change: Option<String>,
        precision: Box<Precision>,
        observations_used: usize,
        continuity_evidence: String,
        actual_input_range: Years,
        excluded_tail: Vec<i32>,
        provisional: bool,
        limitations: Vec<String>,
    },
    NotComputable {
        reason: Reason,
        excluded_tail: Vec<i32>,
        limitations: Vec<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Computation {
    pub contract_version: ContractVersion,
    pub input: Input,
    pub metric: Metric,
    pub requested_range: Years,
    pub result: Outcome,
}
