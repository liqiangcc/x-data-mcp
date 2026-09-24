// src/official_market.rs — S11b decision half: the official runners'
// pure decision layer over the S11a materialization port (NM-101).
//
// Ported verbatim: TdxRangeProvider (probe/probe_facts/probe_bound/
// combined_candidate + RangeProvider acquire/acquire_typed),
// acquire_merged_segment, combined_version_record,
// TdxBackfillProvider/TdxRepublishProvider, TdxGapVerifier::verify,
// TdxDailyRunner (run_item/run_items/collection_candidate/
// materialize_and_commit/not_published).
//
// Typed seam: every repository/storage call lands on the injected
// `repository`/`publisher` objects — the defaults throw a typed
// MarketError("storage") so an unwired arm fails closed, never a silent
// success. `Publisher::prepare` (`publisher_prepare`) and
// `commit`/`commit_collection_batch` plug in once W1's repo objects
// land; the seam text documents the pending arm.
//
// Cross-lane locals pending w5's market_update module: `candidateDigest`
// (sha256 over serde struct-order batch JSON) and `unitDrift` (the two
// prepare gates). CheckedRecord values are `record_codec.check()`'s
// `{raw, version, batch}` shape. Dates are {y,mo,d,s} objects (compare
// via .s — NaiveDate Ord = lexical ISO).
//
// Deltas: none beyond NM-075's documented set — error texts verbatim.

import { createHash } from "node:crypto";
import {
  SourceError, srcNotVerified, srcUnsupported, srcInvalidResponse,
  MaterializeSession, mergeSegmentBars, securityEventsDigest,
  sha256hex, rustDebugStr, SOURCE_NAME, SECURITY_EVENTS_ROLE,
} from "./official_market.mjs";
import {
  MarketError, legacyDigest, validateAndDeduplicate,
  validateUnitCurrencyEvidence, validateAdjustmentEvidence,
} from "../market/record_codec.mjs";
import { StorageError } from "../storage/sqlite_adapter.mjs";
import {
  basisContentDigest, basisNotVerified, gapIntegrity, outcomeUnknown,
} from "../gap_verification/mod.mjs";
import { RECORD_BYTES, parse as parseDay } from "../parsers/day_file.mjs";

const sha256 = (b) => createHash("sha256").update(b).digest("hex");

// ---- market_storage::Error constructors (verbatim Display) ----
const mInvalid = (m) => new MarketError("invalid", `INVALID_MARKET_COMMAND: ${m}`);
const mIntegrity = (m) => new MarketError("integrity", `MARKET_INTEGRITY: ${m}`);
const mConflict = (m) => new MarketError("conflict", `PUBLISH_PRECONDITION: ${m}`);

// ---- task_control values (wire strings, same repr as
//      storage/task_repository.mjs decideHead) ----
const Status = {
  Pending: "pending", Running: "running", CancelRequested: "cancel_requested",
  NeedsVerification: "needs_verification", Succeeded: "succeeded",
  PartiallyFailed: "partially_failed", Failed: "failed",
  Cancelled: "cancelled", Interrupted: "interrupted",
};
const TERMINAL = new Set(["succeeded", "partially_failed", "failed",
  "cancelled", "interrupted"]);
const statusTerminal = (s) => TERMINAL.has(s);
// task_control::visible_status
const visibleStatus = (status, accepted, owner, run) =>
  !TERMINAL.has(status) &&
  (accepted !== run || (owner !== null && owner !== undefined && owner !== run))
    ? "needs_verification" : status;
const Stage = { Pending: "pending", Running: "running", Finished: "finished" };

// task_control::Error carriers used by the runner (Display verbatim).
const tcNeedsVerification = (m) =>
  new StorageError("needs_verification", `ACCEPTANCE_NEEDS_VERIFICATION: ${m}`);

// ---------------------------------------------------------------------
// market_update::candidate_digest — sha256 over serde_json::to_vec(batch)
// (struct field order, Decimals as strings, NaiveDate as ISO string).
// Pending w5's market_update module; kept serde-exact here.
const J = JSON.stringify;
const serOptDec = (v) => (v === null || v === undefined ? "null" : J(v));
const serBar = (b) =>
  `{"date":${J(b.date.s ?? b.date)},"open":${J(b.open)},"high":${J(b.high)},"low":${J(b.low)},` +
  `"close":${J(b.close)},"volume":${serOptDec(b.volume)},"amount":${serOptDec(b.amount)},` +
  `"r7_raw":${b.r7_raw === null || b.r7_raw === undefined ? "null" : String(b.r7_raw)}}`;
const serEvidence = (e) =>
  `{"status":${J(e.status)},"value":${e.value === null || e.value === undefined ? "null" : J(e.value)},"reason":${J(e.reason)}}`;
const serBinding = (b) =>
  `{"role":${J(b.role)},"member":${J(b.member)},"archive_sha256":${J(b.archive_sha256)},"member_sha256":${J(b.member_sha256)}}`;
const serDerivation = (d) => d === null || d === undefined ? "null" :
  `{"base_daily_version_id":${J(d.base_daily_version_id)},"algorithm_version":${J(d.algorithm_version)},` +
  `"first_trading_date":${J(d.first_trading_date.s ?? d.first_trading_date)},"last_trading_date":${J(d.last_trading_date.s ?? d.last_trading_date)}}`;
const serProvenance = (p) =>
  `{"source":${J(p.source)},"source_period":${J(p.source_period)},"source_adjustment":${J(p.source_adjustment)},` +
  `"symbol":${J(p.symbol)},"acquired_at":${J(p.acquired_at)},"adapter_version":${J(p.adapter_version)},` +
  `"yearly_derivation":${serDerivation(p.yearly_derivation)},` +
  `"snapshot_inputs":${p.snapshot_inputs === null || p.snapshot_inputs === undefined ? "null" : `[${p.snapshot_inputs.map(serBinding).join(",")}]`}}`;
export const serBatch = (b) =>
  `{"symbol":${J(b.symbol)},"period":${J(b.period)},"adjustment":${J(b.adjustment)},` +
  `"bars":[${b.bars.map(serBar).join(",")}],"provenance":${serProvenance(b.provenance)},` +
  `"coverage":{"status":${J(b.coverage.status)},"reason":${J(b.coverage.reason)}},` +
  `"unit_currency":{"price_currency":${serEvidence(b.unit_currency.price_currency)},` +
  `"price_unit":${serEvidence(b.unit_currency.price_unit)},"volume_unit":${serEvidence(b.unit_currency.volume_unit)},` +
  `"amount_currency":${serEvidence(b.unit_currency.amount_currency)},` +
  `"amount_unit":${serEvidence(b.unit_currency.amount_unit)}},"adjustment_evidence":${serEvidence(b.adjustment_evidence)}}`;

export const candidateDigest = (batch) => sha256(serBatch(batch));

// market_update::unit_drift — the two prepare gates verbatim: a
// confirmed-value conflict, or an unconfirmed→confirmed enrichment.
// Reason text alone is not drift.
const evConfirmed = (e) =>
  e.status === "Confirmed" && typeof e.value === "string" && e.value !== "";
export const unitDrift = (base, neu) =>
  ["price_currency", "price_unit", "volume_unit", "amount_currency", "amount_unit"]
    .some((k) =>
      (evConfirmed(base[k]) && evConfirmed(neu[k]) && base[k].value !== neu[k].value) ||
      (!evConfirmed(base[k]) && evConfirmed(neu[k])));

// ---- value equality mirroring Rust PartialEq on the serde shapes ----
const dateEq = (a, b) => (a.s ?? a) === (b.s ?? b);
const barEq = (a, b) =>
  dateEq(a.date, b.date) && a.open === b.open && a.high === b.high &&
  a.low === b.low && a.close === b.close && a.volume === b.volume &&
  a.amount === b.amount && (a.r7_raw ?? null) === (b.r7_raw ?? null);
const barListEq = (a, b) =>
  a.length === b.length && a.every((x, i) => barEq(x, b[i]));
const evEq = (a, b) =>
  a.status === b.status && (a.value ?? null) === (b.value ?? null) && a.reason === b.reason;
const ucEq = (a, b) =>
  ["price_currency", "price_unit", "volume_unit", "amount_currency", "amount_unit"]
    .every((k) => evEq(a[k], b[k]));
const coverageEq = (a, b) => a.status === b.status && a.reason === b.reason;
const bindingEq = (a, b) =>
  a.role === b.role && a.member === b.member &&
  a.archive_sha256 === b.archive_sha256 && a.member_sha256 === b.member_sha256;
// Option<Vec<SnapshotInputBinding>> PartialEq: None vs Some(_) differ
// (an empty-Some does not equal None).
const inputsEq = (a, b) => {
  const an = a === null || a === undefined, bn = b === null || b === undefined;
  if (an !== bn) return false;
  if (an) return true;
  return a.length === b.length && a.every((v, i) => bindingEq(v, b[i]));
};

const dateStr = (d) => (typeof d === "string" ? d : d.s);

// ---------------------------------------------------------------------
// Continuity — how the member bytes/event set just read relate to the
// fixed base's pinned snapshot inputs (#262 incremental update).
//   {kind:"unchanged", prefix_bytes}
//   {kind:"append", prefix_bytes, tail:[RawDayRecord]}
//   {kind:"unverified"}

/// official_market::TdxRangeProvider — refresh-path range provider for
/// update_data: digest-proven continuity against the fixed base's pinned
/// snapshot inputs; the unchanged/append arms reuse the base's stored
/// bars and only adjust the strictly-newer tail.
export class TdxRangeProvider {
  constructor(session, publishedAt) {
    this.session = session;
    this.published_at = publishedAt;
  }

  // official_market.rs:571 — digest classification against the base's
  // envelope snapshot_inputs bindings.
  probe(base, market, code) {
    const inputs = base.batch.provenance.snapshot_inputs ?? [];
    const baseDaySha = inputs.find((i) => i.role === "day")?.member_sha256;
    const baseEventsSha =
      inputs.find((i) => i.role === SECURITY_EVENTS_ROLE)?.member_sha256;
    const prefixBytes = base.version.records.length * RECORD_BYTES;
    const lastBaseDate = base.version.records.length
      ? base.version.records[base.version.records.length - 1].date
      : null;
    return this.probeBound(baseDaySha, baseEventsSha, prefixBytes,
      lastBaseDate, market, code);
  }

  // official_market.rs:611 — typed-facts probe bound to the compact
  // source_bindings row (#259 S5G).
  probeFacts(facts, market, code) {
    return this.probeBound(
      facts.day_member_sha256,
      facts.security_event_sha256,
      facts.record_count * RECORD_BYTES,
      facts.last_day,
      market, code);
  }

  // official_market.rs:629 — shared digest-proof continuity
  // classification. The member digest and the prefix digest are the
  // proof; no parsed value is trusted for the classification.
  probeBound(baseDaySha, baseEventsSha, prefixBytes, lastBaseDate, market, code) {
    const source = this.session.source;
    const [dayMember, dayBytes] =
      this.session.memberBytes(source.day, MaterializeSession.dayNames(market, code));
    const dayMemberSha256 = sha256hex(dayBytes);
    const [events, excluded] = this.session.securityEvents(market, code);
    const eventsSha256 = securityEventsDigest(events, excluded);
    let continuity;
    if (baseDaySha !== undefined && baseDaySha !== null &&
        baseEventsSha !== undefined && baseEventsSha !== null) {
      if (baseEventsSha === eventsSha256 && baseDaySha === dayMemberSha256) {
        continuity = { kind: "unchanged", prefix_bytes: dayBytes.length };
      } else if (
        baseEventsSha === eventsSha256 &&
        dayBytes.length > prefixBytes &&
        dayBytes.length % RECORD_BYTES === 0 &&
        sha256hex(dayBytes.subarray(0, prefixBytes)) === baseDaySha
      ) {
        // A parse failure here means the member itself is malformed —
        // a source error, not a revision.
        let tail;
        try {
          tail = parseDay(dayBytes.subarray(prefixBytes));
        } catch (e) {
          throw srcInvalidResponse(`.day member tail malformed: ${e.message}`);
        }
        // Overlapping or missing tail records mean rewritten history —
        // no continuity proof.
        continuity = lastBaseDate !== null && lastBaseDate !== undefined &&
            tail.length > 0 && tail[0].date > dateStr(lastBaseDate)
          ? { kind: "append", prefix_bytes: prefixBytes, tail }
          : { kind: "unverified" };
      } else {
        continuity = { kind: "unverified" };
      }
    } else {
      continuity = { kind: "unverified" };
    }
    return [dayMember, dayMemberSha256, eventsSha256, continuity];
  }

  // official_market.rs:682 — serialize the merged update candidate into
  // an immutable version record and bind the evidence chain.
  combinedCandidate(ctx, base, batch, segment, merged) {
    const combinedBatch = { ...batch, bars: merged };
    combinedBatch.coverage = barListEq(combinedBatch.bars, batch.bars)
      ? batch.coverage       // merged == full history: same coverage claim
      : base.batch.coverage; // otherwise carry the base coverage forward
    const raw = combinedVersionRecord(combinedBatch, ctx.candidate_id, this.published_at);
    const segmentSha256 = candidateDigest(segment);
    const source = this.session.source;
    return {
      evidence: {
        base_integrity_sha256: ctx.base_integrity,
        segment_sha256: segmentSha256,
        combined_integrity_sha256: raw.integrity_sha256,
        complete_coverage_ref:
          `inspected snapshot manifest ${source.manifest_sha256} + day member ` +
          `${ctx.day_member}:${ctx.day_member_sha256} + gbbq member ` +
          `${source.gbbq.sha256} + security events ${ctx.events_sha256}`,
      },
      record: raw,
    };
  }

  // official_market.rs:736 — typed ops-first acquisition (#259 S5G).
  // `null` means the ops path cannot carry this update; caller falls
  // back to `acquire`.
  acquireTyped(facts, baseWindow, range) {
    const symbol = facts.symbol;
    const mc = splitSymbol(symbol);
    if (mc === null) {
      throw srcUnsupported(
        `unsupported symbol ${rustDebugStr(symbol)}; expected sh|sz + six digits`);
    }
    const [market, code] = mc;
    const [dayMember, dayMemberSha256, eventsSha256, continuity] =
      this.probeFacts(facts, market, code);
    const [events, excluded] = this.session.securityEvents(market, code);
    const gbbqMemberSha256 = this.session.decodedGbbq().sha256;
    const source = this.session.source;
    if (continuity.kind === "unverified") return null;
    const mode = continuity.kind; // "unchanged" | "append"
    const prefixBytes = continuity.prefix_bytes;
    const makeEvidence = (candidateSha256) => ({
      base_version_id: facts.version_id,
      base_integrity_sha256: facts.integrity_sha256,
      candidate_sha256: candidateSha256,
      range,
      same_qfq_basis_ref:
        `inspected snapshot manifest ${source.manifest_sha256} + gbbq member ${gbbqMemberSha256}`,
      revision_ref: null,
      continuity: {
        mode,
        day_member_sha256: dayMemberSha256,
        day_prefix_bytes: prefixBytes,
        gbbq_events_sha256: eventsSha256,
      },
    });
    if (mode === "unchanged") {
      // Envelope parity: no base bars inside the accepted range is a
      // source failure there, not a no-change.
      if (baseWindow.length === 0) {
        throw srcInvalidResponse(
          "snapshot carries no bars inside the accepted range");
      }
      const segment = this.session.assembleDaily(
        symbol,
        { member: dayMember, sha256: dayMemberSha256 },
        [...baseWindow],
        events,
        excluded,
      );
      // Byte-proven identical inputs: the base's coverage claim remains
      // exactly true for this data (envelope-path pinning rule).
      segment.coverage = facts.coverage;
      // Metadata the ops path cannot carry — unit drift (prepare's two
      // gates; reason text is not drift) or a non-complete coverage
      // claim requires the envelope path.
      if (unitDrift(facts.unit_currency, segment.unit_currency) ||
          facts.coverage.status !== "Complete") {
        return null;
      }
      validateSegment(segment);
      return {
        decision: "Unchanged",
        evidence: makeEvidence(candidateDigest(segment)),
      };
    }
    // mode === "append"
    const segmentBars = [...baseWindow];
    for (const b of MaterializeSession.qfqBars(continuity.tail, events)) {
      segmentBars.push(b);
    }
    const inRangeBars = segmentBars.filter(inRangeOf(range));
    if (inRangeBars.length === 0) {
      throw srcInvalidResponse(
        "snapshot carries no bars inside the accepted range");
    }
    const segment = this.session.assembleDaily(
      symbol,
      { member: dayMember, sha256: dayMemberSha256 },
      inRangeBars,
      events,
      excluded,
    );
    // A coverage or unit drift means the merged result carries new
    // metadata — the ops path has no channel for it; the envelope path
    // handles it.
    if (!coverageEq(segment.coverage, facts.coverage) ||
        unitDrift(facts.unit_currency, segment.unit_currency)) {
      return null;
    }
    validateSegment(segment);
    // The probe proved every tail record is strictly newer than the
    // base's last day: in-range tail bars are pure upserts.
    const upserts = segment.bars.filter(
      (b) => dateStr(b.date) > dateStr(facts.last_day));
    return {
      decision: upserts.length === 0 ? "Unchanged" : { Append: { upserts } },
      evidence: makeEvidence(candidateDigest(segment)),
    };
  }

  // official_market.rs:971 — RangeProvider::acquire (envelope path).
  acquire(base, range, candidateId) {
    const source = this.session.source;
    const symbol = base.version.symbol;
    const mc = splitSymbol(symbol);
    if (mc === null) {
      throw srcUnsupported(
        `unsupported symbol ${rustDebugStr(symbol)}; expected sh|sz + six digits`);
    }
    const [market, code] = mc;
    const baseIntegrity = base.raw.integrity_sha256;
    if (typeof baseIntegrity !== "string") {
      throw srcNotVerified("fixed base carries no integrity digest");
    }
    const [dayMember, dayMemberSha256, eventsSha256, continuity] =
      this.probe(base, market, code);
    const ctx = {
      base_integrity: baseIntegrity,
      day_member: dayMember,
      day_member_sha256: dayMemberSha256,
      events_sha256: eventsSha256,
      candidate_id: candidateId,
    };
    const mode = continuity.kind === "unverified" ? "full" : continuity.kind;
    const prefixBytes = continuity.kind === "unverified" ? 0 : continuity.prefix_bytes;
    const [events, excluded] = this.session.securityEvents(market, code);
    let batch, combined;
    if (continuity.kind === "unchanged") {
      batch = this.session.assembleDaily(
        symbol,
        { member: dayMember, sha256: dayMemberSha256 },
        [...base.batch.bars],
        events,
        excluded,
      );
      // Byte-proven identical inputs: carry the base's coverage claim
      // forward so a cross-generation no-change stays NoChange.
      batch.coverage = base.batch.coverage;
      const segment = { ...batch, bars: batch.bars.filter(inRangeOf(range)) };
      if (segment.bars.length === 0) {
        throw srcInvalidResponse(
          "snapshot carries no bars inside the accepted range");
      }
      // Records are byte-proven identical; only batch metadata can still
      // authorize a new version. A provable no-change forbids a combined
      // candidate; a metadata change requires it.
      combined = coverageEq(batch.coverage, base.batch.coverage) &&
          ucEq(batch.unit_currency, base.version.unit_currency)
        ? null
        : this.combinedCandidate(ctx, base, batch, segment, [...base.version.records]);
      batch = segment;
    } else if (continuity.kind === "append") {
      const bars = [...base.batch.bars];
      for (const b of MaterializeSession.qfqBars(continuity.tail, events)) {
        bars.push(b);
      }
      batch = this.session.assembleDaily(
        symbol,
        { member: dayMember, sha256: dayMemberSha256 },
        bars,
        events,
        excluded,
      );
      const segment = { ...batch, bars: batch.bars.filter(inRangeOf(range)) };
      if (segment.bars.length === 0) {
        throw srcInvalidResponse(
          "snapshot carries no bars inside the accepted range");
      }
      const merged = [...mergeSegmentBars(base, segment.bars).values()];
      // A narrow accepted range can exclude every appended record; when
      // the merge reproduces the base exactly and units are unchanged
      // the update is a no-change, which forbids a combined candidate.
      if (barListEq(merged, base.version.records) &&
          ucEq(batch.unit_currency, base.version.unit_currency)) {
        segment.coverage = base.batch.coverage;
        batch = segment;
        combined = null;
      } else {
        combined = this.combinedCandidate(ctx, base, batch, segment, merged);
        batch = segment;
      }
    } else {
      // Continuity::Unverified — full materialization.
      batch = this.session.materializeDaily(symbol);
      const segment = { ...batch, bars: batch.bars.filter(inRangeOf(range)) };
      if (segment.bars.length === 0) {
        throw srcInvalidResponse(
          "snapshot carries no bars inside the accepted range");
      }
      batch = segment;
      combined = null;
    }
    const gbbqMemberSha256 = this.session.decodedGbbq().sha256;
    const evidence = {
      base_version_id: base.version.version_id,
      base_integrity_sha256: baseIntegrity,
      candidate_sha256: candidateDigest(batch),
      range,
      same_qfq_basis_ref:
        `inspected snapshot manifest ${source.manifest_sha256} + gbbq member ${gbbqMemberSha256}`,
      revision_ref: null,
      continuity: {
        mode,
        day_member_sha256: dayMemberSha256,
        day_prefix_bytes: prefixBytes,
        gbbq_events_sha256: eventsSha256,
      },
    };
    return { batch, evidence, combined };
  }
}

// Range filter matching the Rust `range.start <= b.date && b.date <= range.end`
// (NaiveDate Ord — lexical on the canonical ISO string).
const inRangeOf = (range) => (b) =>
  dateStr(range.start) <= dateStr(b.date) && dateStr(b.date) <= dateStr(range.end);

// `{market}.{code}` — verbatim symbol gate shared by acquire/acquire_typed.
const splitSymbol = (symbol) => {
  const dot = symbol.indexOf(".");
  const mc = dot < 0 ? null : [symbol.slice(0, dot), symbol.slice(dot + 1)];
  return mc !== null && (mc[0] === "sh" || mc[0] === "sz") &&
      mc[1].length === 6 && /^[0-9]+$/.test(mc[1])
    ? mc : null;
};

// The three publish validators over the assembled segment batch —
// publish::validate_* -> SourceError::InvalidResponse(e.to_string()).
const validateSegment = (segment) => {
  try {
    validateAndDeduplicate(segment);
    validateUnitCurrencyEvidence(segment);
    validateAdjustmentEvidence(segment);
  } catch (e) {
    throw srcInvalidResponse(e.message);
  }
};

// serde_json::to_value on the typed structs — plain-object Value form:
// NaiveDate/DateTime as ISO strings, Decimals as strings. `legacyDigest`
// (sorted-key canonical serde) over these equals serde_json::to_vec.
const barValue = (b) => ({
  date: dateStr(b.date), open: b.open, high: b.high, low: b.low,
  close: b.close, volume: b.volume ?? null, amount: b.amount ?? null,
  r7_raw: b.r7_raw ?? null,
});
const evValue = (e) => ({
  status: e.status, value: e.value ?? null, reason: e.reason,
});
const ucValue = (u) => ({
  price_currency: evValue(u.price_currency),
  price_unit: evValue(u.price_unit),
  volume_unit: evValue(u.volume_unit),
  amount_currency: evValue(u.amount_currency),
  amount_unit: evValue(u.amount_unit),
});
const derivValue = (d) => d === null || d === undefined ? null : ({
  base_daily_version_id: d.base_daily_version_id,
  algorithm_version: d.algorithm_version,
  first_trading_date: dateStr(d.first_trading_date),
  last_trading_date: dateStr(d.last_trading_date),
});
const provValue = (p) => ({
  source: p.source, source_period: p.source_period,
  source_adjustment: p.source_adjustment, symbol: p.symbol,
  acquired_at: p.acquired_at, adapter_version: p.adapter_version,
  yearly_derivation: derivValue(p.yearly_derivation),
  snapshot_inputs: p.snapshot_inputs ?? null,
});
const batchValue = (b) => ({
  symbol: b.symbol, period: b.period, adjustment: b.adjustment,
  bars: b.bars.map(barValue), provenance: provValue(b.provenance),
  coverage: { status: b.coverage.status, reason: b.coverage.reason },
  unit_currency: ucValue(b.unit_currency),
  adjustment_evidence: evValue(b.adjustment_evidence),
});
const versionValue = (v) => ({
  version_id: v.version_id, symbol: v.symbol, period: v.period,
  adjustment: v.adjustment, source: v.source,
  records: v.records.map(barValue), published_at: v.published_at,
  validation: {
    record_count: v.validation.record_count,
    unique_record_count: v.validation.unique_record_count,
    duplicate_count: v.validation.duplicate_count,
  },
  unit_currency: ucValue(v.unit_currency),
  adjustment_evidence: evValue(v.adjustment_evidence),
  yearly_derivation: derivValue(v.yearly_derivation),
});

// official_market.rs:1197 — serialize a merged candidate batch into the
// immutable version record and bind its integrity digest.
export function combinedVersionRecord(combinedBatch, candidateId, publishedAt) {
  let records, validation;
  try {
    ({ bars: records, validation } = validateAndDeduplicate(combinedBatch));
  } catch (e) {
    throw srcInvalidResponse(e.message);
  }
  const raw = versionValue({
    version_id: candidateId,
    symbol: combinedBatch.symbol,
    period: combinedBatch.period,
    adjustment: combinedBatch.adjustment,
    source: combinedBatch.provenance.source,
    records,
    published_at: publishedAt,
    validation,
    unit_currency: combinedBatch.unit_currency,
    adjustment_evidence: combinedBatch.adjustment_evidence,
    yearly_derivation: null,
  });
  raw.batch = batchValue(combinedBatch);
  let digest;
  try {
    digest = legacyDigest(raw);
  } catch (e) {
    throw srcInvalidResponse(e.message);
  }
  raw.integrity_sha256 = digest;
  return raw;
}

// official_market.rs:1246 — shared acquisition for providers that build
// a trusted combined candidate: fixed base merged with real source bars
// inside the accepted range, outside-range base values preserved.
export function acquireMergedSegment(
  session, base, range, candidateId, publishedAt, revisionRef,
) {
  const source = session.source;
  const batch = session.materializeDaily(base.version.symbol);
  // Same pinned snapshot = same QFQ basis: the base's declared snapshot
  // inputs must equal the member/archive digests this installed source
  // actually read.
  if (base.batch.provenance.source !== SOURCE_NAME ||
      base.batch.provenance.source_adjustment !== "qfq" ||
      !inputsEq(base.batch.provenance.snapshot_inputs,
        batch.provenance.snapshot_inputs)) {
    throw srcNotVerified(
      "base does not provably derive from the installed snapshot");
  }
  const inputs = batch.provenance.snapshot_inputs ?? [];
  const day = inputs.find((i) => i.role === "day");
  if (day === undefined) {
    throw srcInvalidResponse("snapshot binding lacks the day member");
  }
  const gbbq = inputs.find((i) => i.role === "gbbq");
  if (gbbq === undefined) {
    throw srcInvalidResponse("snapshot binding lacks the gbbq member");
  }
  // The segment is the real source bars inside the accepted union of
  // frozen proven-gap ranges; an empty segment cannot fill anything.
  const segment = { ...batch, bars: batch.bars.filter(inRangeOf(range)) };
  if (segment.bars.length === 0) {
    throw srcInvalidResponse(
      "snapshot carries no bars inside the frozen gap range");
  }
  const baseIntegrity = base.raw.integrity_sha256;
  if (typeof baseIntegrity !== "string") {
    throw srcNotVerified("fixed base carries no integrity digest");
  }
  // Mirror the merge market_update::prepare approves (#4 AC-71).
  const merged = mergeSegmentBars(base, segment.bars);
  const combinedBatch = { ...batch, bars: [...merged.values()] };
  combinedBatch.coverage = barListEq(combinedBatch.bars, batch.bars)
    ? batch.coverage : base.batch.coverage;
  const raw = combinedVersionRecord(combinedBatch, candidateId, publishedAt);
  const segmentSha256 = candidateDigest(segment);
  return {
    batch: segment,
    evidence: {
      base_version_id: base.version.version_id,
      base_integrity_sha256: baseIntegrity,
      candidate_sha256: segmentSha256,
      range,
      same_qfq_basis_ref:
        `inspected snapshot manifest ${source.manifest_sha256} + gbbq member ${gbbq.member_sha256}`,
      revision_ref: revisionRef,
      // Same-generation equality above already pins every input; the
      // digest-continuity modes exist for cross-generation updates only.
      continuity: null,
    },
    combined: {
      evidence: {
        base_integrity_sha256: baseIntegrity,
        segment_sha256: segmentSha256,
        combined_integrity_sha256: raw.integrity_sha256,
        complete_coverage_ref:
          `inspected snapshot manifest ${source.manifest_sha256} + day member ` +
          `${day.member}:${day.member_sha256} + gbbq member ${gbbq.member}:${gbbq.member_sha256}`,
      },
      record: raw,
    },
  };
}

/// #187 Phase C provider for `backfill_data` under the default frozen-gap
/// contract only — never for explicit ranges and never for update_data.
export class TdxBackfillProvider {
  constructor(session, publishedAt) {
    this.session = session;
    this.published_at = publishedAt;
  }
  acquire(base, range, candidateId) {
    return acquireMergedSegment(
      this.session, base, range, candidateId, this.published_at,
      `frozen proven-gap authorization checked by the default-contract frozen inputs; base ${base.version.version_id} range ${dateStr(range.start)}..${dateStr(range.end)} candidate ${candidateId}`,
    );
  }
}

/// #247 provider for `republish_data` under the explicit republish
/// contract — revision_ref stays None so market_update::prepare
/// independently rejects any revised known value.
export class TdxRepublishProvider {
  constructor(session, publishedAt) {
    this.session = session;
    this.published_at = publishedAt;
  }
  acquire(base, range, candidateId) {
    return acquireMergedSegment(
      this.session, base, range, candidateId, this.published_at, null);
  }
}

// ---------------------------------------------------------------------
// Typed repository seam — every storage-facing call fails closed with a
// market_storage::Error Storage text so an unwired arm is a typed error,
// never a silent success (data_supply RepositorySeam convention).
const SEAM =
  "official runner repository seam not wired (S11b storage arm pending): ";
const seamErr = (op) =>
  new MarketError("storage", `MARKET_STORAGE: ${SEAM}${op}`);
export class RunnerRepositorySeam {
  // MarketRepository reads
  candidate() { throw seamErr("candidate"); }
  mark_validated() { throw seamErr("mark_validated"); }
  fixed() { throw seamErr("fixed"); }
  fixed_many() { throw seamErr("fixed_many"); }
  fixed_facts() { throw seamErr("fixed_facts"); }
  fixed_window() { throw seamErr("fixed_window"); }
  fixed_window_years() { throw seamErr("fixed_window_years"); }
  task() { throw seamErr("task"); }
  task_head() { throw seamErr("task_head"); }
  task_item_binding() { throw seamErr("task_item_binding"); }
  transition_task_head() { throw seamErr("transition_task_head"); }
  commit() { throw seamErr("commit"); }
  receipt() { throw seamErr("receipt"); }
  range() { throw seamErr("range"); }
  versions() { throw seamErr("versions"); }
  stage() { throw seamErr("stage"); }
  // DailyCollectionRepository adds
  collection_receipt() { throw seamErr("collection_receipt"); }
  effective_head() { throw seamErr("effective_head"); }
  commit_collection_batch() { throw seamErr("commit_collection_batch"); }
  // market_storage::Publisher::prepare — wraps this.repository in Rust;
  // modelled as a repository-facing seam call here (W1 pending).
  publisher_prepare() { throw seamErr("publisher_prepare"); }
}

// ---------------------------------------------------------------------
// official_market.rs:1890 — #187 Phase B trusted gap verifier bound to
// an installed official-file snapshot (sealed SourceGapVerifier port).
const CONCLUSION_SOURCE_LIMITATION = "source_limitation";
const CONCLUSION_NO_LOCAL_GAP = "no_local_gap";
const CONFIRMED_MISSING_RANGE = "confirmed_missing_range";
const GAP_VERIFIER_ID = "tdx-official-gap-verifier-v1";

export class TdxGapVerifier {
  constructor(source, repository, observedAt) {
    this.source = source;
    this.repository = repository ?? new RunnerRepositorySeam();
    this.observed_at = observedAt;
  }
  verifierId() { return GAP_VERIFIER_ID; }
  verify(target, subject) {
    const targetDay = dateStr(target.start_date);
    // Bounded slice: one frozen day only — wider targets are refused.
    if (targetDay !== dateStr(target.end_date)) {
      throw basisNotVerified(
        "bounded gap verifier supports single-day targets only");
    }
    let batch;
    try {
      batch = this.source.materializeDaily(target.symbol);
    } catch (e) {
      throw basisNotVerified(
        `official-file source cannot materialize the target symbol: ${e.message}`);
    }
    // Re-read the frozen subject: a fully verified published record
    // whose stored digest must equal the frozen binding.
    const record = this.repository.fixed(subject.version_id);
    if (record.raw.integrity_sha256 !== subject.integrity_sha256) {
      throw basisNotVerified("subject digest does not match frozen binding");
    }
    const version = record.version;
    if (version.symbol !== target.symbol || version.period !== "Day" ||
        version.source !== SOURCE_NAME || version.adjustment !== "Forward" ||
        !evConfirmed(version.adjustment_evidence) ||
        version.adjustment_evidence.value !== "qfq") {
      throw basisNotVerified(
        "subject is not a pinned official-file QFQ daily version");
    }
    // Same pinned snapshot = same QFQ basis: the subject's declared
    // snapshot inputs must equal the member/archive digests this
    // installed source actually read.
    if (record.batch.provenance.source !== SOURCE_NAME ||
        record.batch.provenance.source_adjustment !== "qfq" ||
        !inputsEq(record.batch.provenance.snapshot_inputs,
          batch.provenance.snapshot_inputs)) {
      throw basisNotVerified(
        "subject does not provably derive from the installed snapshot");
    }
    // Comparable overlap must agree field-for-field; a conflict is an
    // integrity doubt, never a gap.
    const sourceBars = new Map(batch.bars.map((b) => [dateStr(b.date), b]));
    for (const local of record.batch.bars) {
      const remote = sourceBars.get(dateStr(local.date));
      if (remote !== undefined && !barEq(local, remote)) {
        throw basisNotVerified(
          `subject/source value conflict on ${dateStr(local.date)}`);
      }
    }
    const conclusion = !sourceBars.has(targetDay)
      ? CONCLUSION_SOURCE_LIMITATION
      : record.batch.bars.some((b) => dateStr(b.date) === targetDay)
        ? CONCLUSION_NO_LOCAL_GAP
        : CONFIRMED_MISSING_RANGE;
    const inputs = batch.provenance.snapshot_inputs ?? [];
    const day = inputs.find((i) => i.role === "day");
    if (day === undefined) {
      throw gapIntegrity("materialized batch missing day binding");
    }
    const gbbq = inputs.find((i) => i.role === "gbbq");
    if (gbbq === undefined) {
      throw gapIntegrity("materialized batch missing gbbq binding");
    }
    const basis = {
      source_identity: `${SOURCE_NAME} manifest ${this.source.manifest_sha256}`,
      source_contract: ADAPTER_VERSION,
      symbol: target.symbol,
      start_date: targetDay,
      end_date: dateStr(target.end_date),
      subject_version_id: subject.version_id,
      subject_version_digest: subject.integrity_sha256,
      artifact_identity:
        `${day.member}:${day.member_sha256} manifest ${this.source.manifest_sha256} gbbq member ${gbbq.member}:${gbbq.member_sha256}`,
      artifact_sha256: day.member_sha256,
      conclusion,
      observed_at: this.observed_at,
      verifier_id: this.verifierId(),
      basis_sha256: "",
    };
    // content_digest feeds observed_at.to_rfc3339() ("+00:00" suffix)
    // while the serialized field keeps serde's "Z" form — digest over
    // the rfc3339 view.
    basis.basis_sha256 = basisContentDigest({
      ...basis,
      observed_at: this.observed_at.endsWith("Z")
        ? `${this.observed_at.slice(0, -1)}+00:00`
        : this.observed_at,
    });
    return basis;
  }
}

// ---------------------------------------------------------------------
// official_market.rs:1480 — item runner for start_collection /
// market.daily / v1. Lifecycle mirrors RangeRunner: claim, start,
// materialize, stage, publish via Publisher::prepare/commit.
export const ItemExecution = {
  Published: (receipt) => ({ kind: "published", receipt }),
  NotPublished: (task) => ({ kind: "not_published", task }),
};

export class TdxDailyRunner {
  constructor({ source, session, repository, runId, publishedAt }) {
    this.source = source;
    this.session = session ?? null;
    this.repository = repository ?? new RunnerRepositorySeam();
    this.run_id = runId;
    this.published_at = publishedAt;
  }

  runItem(taskId, itemId) {
    const repository = this.repository;
    let binding = repository.task_item_binding(taskId, itemId);
    if (binding === null || binding === undefined) {
      repository.task_head(taskId);
      throw mInvalid("collection item absent from accepted task");
    }
    if (binding.head.key.operation !== "start_collection" ||
        binding.head.key.dataset !== "market.daily" ||
        binding.head.key.contract_version !== "v1") {
      throw mInvalid("unsupported collection contract");
    }
    const existing = repository.collection_receipt(taskId, itemId);
    if (existing !== null && existing !== undefined) {
      return ItemExecution.Published(existing);
    }
    if (visibleStatus(binding.head.status, binding.head.accepted_run_id,
        binding.head.owner_run_id, this.run_id) === Status.NeedsVerification) {
      // Error::Task(task_control::Error::NeedsVerification)
      throw tcNeedsVerification("old run or unresolved execution");
    }
    if (binding.item.stage === Stage.Running) {
      throw outcomeUnknown(
        "running item has no verified result; inspect before any retry");
    }
    if (statusTerminal(binding.head.status) ||
        binding.item.stage === Stage.Finished) {
      if (binding.item.outcome !== null && binding.item.outcome !== undefined &&
          binding.item.outcome.success === true) {
        throw mIntegrity("successful collection item has no receipt");
      }
      return this.#notPublished(taskId);
    }
    if (binding.head.status === Status.CancelRequested) {
      repository.transition_task_head(binding.head, null, this.run_id,
        { kind: "finishExecution" });
      return this.#notPublished(taskId);
    }
    if (binding.head.status === Status.Pending) {
      repository.transition_task_head(binding.head, null, this.run_id,
        { kind: "claim" });
    }
    repository.transition_task_head(binding.head, binding.item, this.run_id,
      { kind: "startItem", item_id: itemId });
    let receipt;
    try {
      receipt = this.#materializeAndCommit(binding, itemId);
    } catch (error) {
      // A response can be lost after commit. Never overwrite success.
      const replayed = repository.collection_receipt(taskId, itemId);
      if (replayed !== null && replayed !== undefined) {
        return ItemExecution.Published(replayed);
      }
      if (isOutcomeUnknown(error)) {
        const b = repository.task_item_binding(taskId, itemId);
        if (b === null || b === undefined) {
          throw mIntegrity("bound task item missing");
        }
        try {
          repository.transition_task_head(b.head, b.item, this.run_id, {
            kind: "requireVerification", item_id: itemId,
            reason: "item execution result not confirmed after materialization/publication; committed receipt not yet established",
          });
        } catch (recording) {
          const r2 = repository.collection_receipt(taskId, itemId);
          if (r2 !== null && r2 !== undefined) {
            return ItemExecution.Published(r2);
          }
          throw recording;
        }
        throw error;
      }
      const b = repository.task_item_binding(taskId, itemId);
      if (b === null || b === undefined) {
        throw mIntegrity("bound task item missing");
      }
      const cancelledWithoutPublication =
        error instanceof MarketError && error.kind === "conflict" &&
        b.head.status === Status.CancelRequested;
      repository.transition_task_head(b.head, b.item, this.run_id, {
        kind: "finishItem", item_id: itemId,
        outcome: {
          success: false,
          error: cancelledWithoutPublication
            ? `cancelled before publication: ${error.message}`
            : `${error.message}`,
          evidence: null,
        },
      });
      if (b.head.counts.finished === b.head.counts.total ||
          b.head.status === Status.CancelRequested) {
        repository.transition_task_head(b.head, null, this.run_id, {
          kind: cancelledWithoutPublication ? "finishCancellation" : "finishExecution",
        });
      }
      return this.#notPublished(taskId);
    }
    return ItemExecution.Published(receipt);
  }

  #notPublished(taskId) {
    return ItemExecution.NotPublished(this.repository.task(taskId));
  }

  // Bulk collection path (#259 S5F): produce the typed candidate for
  // every item, then stage and commit them in one repository
  // transaction. Unproduceable items fall back to run_item.
  // `null` means the repository only supports the envelope path.
  runItems(taskId, itemIds, generationId) {
    const candidates = [];
    const fallback = [];
    for (const itemId of itemIds) {
      let c;
      try {
        c = this.#collectionCandidate(taskId, itemId, generationId);
      } catch {
        fallback.push(itemId);
        continue;
      }
      if (c === null) fallback.push(itemId);
      else candidates.push(c);
    }
    let results;
    try {
      results = this.repository.commit_collection_batch(taskId, candidates);
    } catch {
      // Structural/transaction errors rolled the whole batch back:
      // isolate per item so one bad item cannot wedge the task in a
      // Running-retry loop (the documented batch contract).
      fallback.push(...candidates.map((item) => item.item_id));
      for (const itemId of fallback) this.runItem(taskId, itemId);
      return true; // Ok(Some(()))
    }
    if (results === null || results === undefined) return null; // Ok(None)
    if (results.length !== candidates.length) {
      throw mIntegrity(
        "collection batch result count does not match candidates");
    }
    // Per-item results: {ok: receipt} | {err: error} (w5 batch convention).
    for (let i = 0; i < candidates.length; i++) {
      if (results[i] === null || results[i] === undefined ||
          results[i].ok === undefined) {
        fallback.push(candidates[i].item_id);
      }
    }
    for (const itemId of fallback) this.runItem(taskId, itemId);
    return true; // Ok(Some(()))
  }

  // Typed candidate production for run_items: every check run_item
  // performs before staging, minus the envelope materialization.
  // `null` defers the item to the per-item path.
  #collectionCandidate(taskId, itemId, generationId) {
    const repository = this.repository;
    const binding = repository.task_item_binding(taskId, itemId);
    if (binding === null || binding === undefined) {
      throw mInvalid("collection item absent from accepted task");
    }
    if (binding.head.key.operation !== "start_collection" ||
        binding.head.key.dataset !== "market.daily" ||
        binding.head.key.contract_version !== "v1") {
      throw mInvalid("unsupported collection contract");
    }
    if (statusTerminal(binding.head.status) ||
        visibleStatus(binding.head.status, binding.head.accepted_run_id,
          binding.head.owner_run_id, this.run_id) === Status.NeedsVerification ||
        binding.item.stage !== Stage.Pending) {
      // Terminal tasks, unverified runs and items already started or
      // finished keep the per-item path.
      return null;
    }
    if (binding.head.status === Status.CancelRequested) {
      return null;
    }
    const target = binding.item.spec.target;
    const symbol = typeof target?.symbol === "string" ? target.symbol : null;
    if (symbol === null) {
      throw mIntegrity("collection item target missing symbol");
    }
    if (target.period !== "day") {
      throw mInvalid("collection target is not daily");
    }
    const scope = { symbol, period: "Day" };
    if (repository.effective_head(scope) !== null &&
        repository.effective_head(scope) !== undefined) {
      throw mInvalid("existing daily head requires the update_data range path");
    }
    let batch;
    try {
      batch = this.session !== null
        ? this.session.materializeDaily(symbol)
        : this.source.materializeDaily(symbol);
    } catch (e) {
      throw mInvalid(e.message);
    }
    const id = sha256(JSON.stringify([taskId, itemId]));
    let records;
    try {
      ({ bars: records } = validateAndDeduplicate(batch));
    } catch (e) {
      throw mInvalid(e.message);
    }
    const inputs = batch.provenance.snapshot_inputs ?? [];
    const memberSha = (role) => {
      const input = inputs.find((i) => i.role === role);
      if (input === undefined) {
        throw mIntegrity(`snapshot inputs lack the ${role} member binding`);
      }
      return input.member_sha256;
    };
    return {
      task_id: taskId,
      item_id: itemId,
      publication_id: `collection-result-${id}`,
      version_id: `collection-version-${id}`,
      run_id: this.run_id,
      symbol: batch.symbol,
      source: batch.provenance.source,
      bars: records,
      coverage: batch.coverage,
      unit_currency: batch.unit_currency,
      adjustment_evidence: batch.adjustment_evidence,
      published_at: this.published_at,
      generation_id: generationId,
      generation_manifest_sha256: this.source.manifest_sha256,
      day_member_sha256: memberSha("day"),
      security_event_sha256: memberSha(SECURITY_EVENTS_ROLE),
      generation_evidence: {
        manifest_sha256: this.source.manifest_sha256,
        snapshot_inputs: inputs,
      },
    };
  }

  // Source failure preserves the old version: nothing is staged or
  // committed on any materialization/validation error.
  #materializeAndCommit(binding, itemId) {
    const repository = this.repository;
    const target = binding.item.spec.target;
    const symbol = typeof target?.symbol === "string" ? target.symbol : null;
    if (symbol === null) {
      throw mIntegrity("collection item target missing symbol");
    }
    if (target.period !== "day") {
      throw mInvalid("collection target is not daily");
    }
    const scope = { symbol, period: "Day" };
    const head = repository.effective_head(scope);
    if (head !== null && head !== undefined) {
      throw mInvalid("existing daily head requires the update_data range path");
    }
    let batch;
    try {
      batch = this.session !== null
        ? this.session.materializeDaily(symbol)
        : this.source.materializeDaily(symbol);
    } catch (e) {
      throw mInvalid(e.message);
    }
    const current = repository.task_head(binding.head.task_id);
    if (current.status !== Status.Running ||
        BigInt(current.state_revision) !== BigInt(binding.head.state_revision)) {
      throw mConflict("task changed or cancelled during materialization");
    }
    const id = sha256(JSON.stringify([binding.head.task_id, itemId]));
    const candidateId = `collection-version-${id}`;
    let records, validation;
    try {
      ({ bars: records, validation } = validateAndDeduplicate(batch));
    } catch (e) {
      throw mInvalid(e.message);
    }
    const raw = versionValue({
      version_id: candidateId,
      symbol: batch.symbol,
      period: batch.period,
      adjustment: batch.adjustment,
      source: batch.provenance.source,
      records,
      published_at: this.published_at,
      validation,
      unit_currency: batch.unit_currency,
      adjustment_evidence: batch.adjustment_evidence,
      yearly_derivation: batch.provenance.yearly_derivation,
    });
    raw.batch = batchValue(batch);
    try {
      raw.integrity_sha256 = legacyDigest(raw);
    } catch (e) {
      throw mIntegrity(e.message);
    }
    repository.stage(raw);
    const plan = repository.publisher_prepare({
      publication_id: `collection-result-${id}`,
      version_id: candidateId,
      task_id: binding.head.task_id,
      item_id: itemId,
      run_id: this.run_id,
      expected_revision: Number(binding.head.state_revision),
      intent: { Daily: { expected_head: null } },
    });
    return repository.commit(plan);
  }
}

// runner.rs outcome guard: Error::OutcomeUnknown | Error::Storage |
// Error::Task(StorageUnavailable | OutcomeUnknown).
const isOutcomeUnknown = (e) =>
  (e instanceof MarketError &&
    (e.kind === "outcome_unknown" || e.kind === "storage")) ||
  (e instanceof StorageError &&
    (e.code === "unavailable" || e.code === "outcome_unknown"));

export { SourceError };
