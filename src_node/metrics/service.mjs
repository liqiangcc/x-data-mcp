// metrics/service.rs — Metrics<R: InputRepository>::compute. All
// pre-repository validation precedes any storage read; observations come
// from the InputRepository seam (SqliteMetrics — sqlite_metrics.mjs),
// then consecutive::evaluate + the Computation assembly are verbatim.

import { invalid } from "../data_supply/access.mjs";
import { securityValidate, DATASET_YEARLY } from "../data_supply/wire.mjs";
import { yearlyReference } from "./wire.mjs";
import { evaluate, ConsecutiveError, ConsecutiveInputError } from
  "../algorithms/consecutive.mjs";

// completion provenance limitation — service.rs verbatim text.
const LIMITATION =
  "completion is per-observation evidence-derived (fixture coverage or verifier-signed derivation evidence); completeness is base-relative, not a natural-year or listing-history claim";

export class Metrics {
  constructor(repository) {
    this.repository = repository; // InputRepository — SqliteMetrics
  }

  // metrics/service.rs:24-56 — fixed identity/digest binding, entity
  // validation, access gate, resource/range budget; then
  // repository.observations(&ref, &range) -> evaluate -> Computation.
  compute(request, access) {
    if (request.inputs.length !== 1 || request.metrics.length !== 1)
      throw invalid("exactly one inline input and metric supported");
    const input = request.inputs[0];
    const metric = request.metrics[0];
    if (input.input_id.trim() === "" || metric.metric_id.trim() === "" ||
        metric.input_ids.length !== 1 ||
        metric.input_ids[0] !== input.input_id ||
        input.version_ref.version_id.trim() === "" ||
        input.version_ref.version_id === "latest" ||
        input.version_ref.integrity_sha256.length !== 64)
      throw invalid("fixed yearly identity/digest/input reference required");
    const entity = input.version_ref.entity;
    const bad = securityValidate(entity);
    if (bad) throw invalid(bad);
    access.require(DATASET_YEARLY, entity);
    const range = request.range;
    if (range.start_year < 1 || range.end_year > 9999 ||
        range.end_year < range.start_year ||
        range.end_year - range.start_year >= 1000 ||
        !(metric.parameters.min_periods >= 1 && metric.parameters.min_periods <= 1000))
      throw invalid("computation resource/range budget");
    const observations = this.repository.observations(
      yearlyReference(input.version_ref), range);
    let result;
    try {
      result = evaluate(observations, range, metric.parameters);
    } catch (e) {
      // consecutive::evaluate's domain errors are market_storage
      // Error::Invalid — carry the kind so mapped() yields
      // invalid_arguments verbatim.
      if (e instanceof ConsecutiveError)
        throw invalid(e.message.replace(/^INVALID_MARKET_COMMAND: /, ""));
      if (e instanceof ConsecutiveInputError)
        throw invalid(e.message);
      throw e;
    }
    result.limitations.push(LIMITATION);
    return {
      contract_version: request.contract_version,
      input,
      metric,
      requested_range: range,
      result,
    };
  }
}
