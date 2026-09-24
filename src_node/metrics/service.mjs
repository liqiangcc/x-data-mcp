// metrics/service.rs — Metrics<R: InputRepository>::compute skeleton.
// All pre-repository validation is ported verbatim; the repository
// `observations()` call is a typed seam (StorageError — never silent)
// until SqliteMetrics lands. The `evaluate` + Computation tail is
// unreachable behind the seam today and is not ported yet.

import { invalid } from "../data_supply/access.mjs";
import { securityValidate, DATASET_YEARLY } from "../data_supply/wire.mjs";
import { yearlyReference } from "./wire.mjs";

export class Metrics {
  constructor(repository) {
    this.repository = repository; // InputRepository — seam today
  }

  // metrics/service.rs:24-56 — fixed identity/digest binding, entity
  // validation, access gate, resource/range budget; then
  // repository.observations(&ref, &range) — the seam.
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
    return this.repository.observations(yearlyReference(input.version_ref), range);
    // -> consecutive::evaluate(observations, range, metric.parameters)
    //    + limitations push + Computation assembly — S12c-adjacent.
  }
}
