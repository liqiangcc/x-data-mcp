// metrics/wire.rs request+output types on the shared serde from_value
// machinery (src_node/mcp/serde_value.mjs). The compute execution lives
// in service.mjs; decode shape pinned by the mcp_data oracle corpus.

import {
  tBool, tDate, tEnumUnit, tI32, tInt, tOpt, tString, tStruct, tTagged,
  tUsize, tVec,
} from "../mcp/serde_value.mjs";
import { ContractVersion, Security } from "../data_supply/wire.mjs";

export const Execution = tEnumUnit("Execution", ["inline"]);
export const MetricName = tEnumUnit("MetricName", ["consecutive_direction"]);
export const Algorithm = tEnumUnit("Algorithm", ["local-consecutive-direction-v1"]);
export const Direction = tEnumUnit("Direction", ["up", "down"]);
export const PriceField = tEnumUnit("PriceField", ["close"]);
export const EqualPolicy = tEnumUnit("EqualPolicy", ["break"]);
export const GapPolicy = tEnumUnit("GapPolicy", ["reject"]);
export const YearlyDataset = tEnumUnit("YearlyDataset", ["market.yearly"]);
export const YearlyVersionRef = tStruct("YearlyVersionRef", [
  { name: "dataset", dec: YearlyDataset },
  { name: "contract_version", dec: ContractVersion },
  { name: "entity", dec: Security },
  { name: "version_id", dec: tString },
  { name: "integrity_sha256", dec: tString },
]);
export const Input = tStruct("Input", [
  { name: "input_id", dec: tString },
  { name: "version_ref", dec: YearlyVersionRef },
]);
export const Years = tStruct("Years", [
  { name: "start_year", dec: tI32 },
  { name: "end_year", dec: tI32 },
]);
export const Parameters = tStruct("Parameters", [
  { name: "direction", dec: Direction },
  { name: "min_periods", dec: tUsize },
  { name: "price_field", dec: PriceField, default: () => "close" },
  { name: "completed_only", dec: tBool, default: () => true },
  { name: "equal_policy", dec: EqualPolicy, default: () => "break" },
  { name: "gap_policy", dec: GapPolicy, default: () => "reject" },
]);
export const Metric = tStruct("Metric", [
  { name: "metric_id", dec: tString },
  { name: "name", dec: MetricName },
  { name: "algorithm_version", dec: Algorithm },
  { name: "input_ids", dec: tVec(tString) },
  { name: "parameters", dec: Parameters },
]);
export const Compute = tStruct("Compute", [
  { name: "contract_version", dec: ContractVersion },
  { name: "inputs", dec: tVec(Input) },
  { name: "range", dec: Years },
  { name: "metrics", dec: tVec(Metric) },
  { name: "execution", dec: Execution },
]);
// Output-side types (decode parity; never emitted by the seam).
export const Point = tStruct("Point", [
  { name: "year", dec: tI32 },
  { name: "first_trading_date", dec: tDate },
  { name: "last_trading_date", dec: tDate },
  { name: "close", dec: tString },
]);
export const ObservedDirection = tEnumUnit("ObservedDirection", ["up", "down", "flat"]);
export const RunKind = tEnumUnit("RunKind", ["exact", "lower_bound"]);
export const Reason = tEnumUnit("Reason",
  ["window_too_short", "missing_year", "unknown_completion",
   "incomplete_period", "non_positive_price"]);
export const Precision = tStruct("Precision", [
  { name: "scale", dec: tInt("u32", 0n, (1n << 32n) - 1n) },
  { name: "rounding", dec: tString },
  { name: "unit", dec: tString },
]);
export const Outcome = tTagged("Outcome", "outcome", {
  computed: { rust: "Computed", fields: [
    { name: "observed_direction", dec: ObservedDirection },
    { name: "run_length", dec: tUsize },
    { name: "run_length_kind", dec: RunKind },
    { name: "matches", dec: tOpt(tBool), opt: true },
    { name: "anchor", dec: tOpt(Point), opt: true },
    { name: "first_change", dec: tOpt(Point), opt: true },
    { name: "end", dec: Point },
    { name: "cumulative_change", dec: tOpt(tString), opt: true },
    { name: "precision", dec: Precision },
    { name: "observations_used", dec: tUsize },
    { name: "continuity_evidence", dec: tString },
    { name: "actual_input_range", dec: Years },
    { name: "excluded_tail", dec: tVec(tI32) },
    { name: "provisional", dec: tBool },
    { name: "limitations", dec: tVec(tString) },
  ] },
  not_computable: { rust: "NotComputable", fields: [
    { name: "reason", dec: Reason },
    { name: "excluded_tail", dec: tVec(tI32) },
    { name: "limitations", dec: tVec(tString) },
  ] },
});
export const Computation = tStruct("Computation", [
  { name: "contract_version", dec: ContractVersion },
  { name: "input", dec: Input },
  { name: "metric", dec: Metric },
  { name: "requested_range", dec: Years },
  { name: "result", dec: Outcome },
]);

// YearlyVersionRef::reference() — version_ref.rs wiring (used by the
// repository observations call).
export const yearlyReference = (v) => ({
  dataset: "market.yearly",
  contract_version: v.contract_version,
  entity: v.entity,
  version_id: v.version_id,
  integrity_sha256: v.integrity_sha256,
});
