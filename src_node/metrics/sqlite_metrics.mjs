// src/sqlite_metrics.rs — SqliteMetrics: InputRepository::observations
// over the market backend. Reads a fixed verified yearly version and
// hash-bound fixture coverage in one snapshot read.
//
// Dependency: `yearlyRowMeta` lives in ../data_supply/sqlite_supply.mjs
// (NM-074) — the module loads once that lane lands.

import * as MR from "../market/market_read.mjs";
import { MarketError } from "../market/record_codec.mjs";
import { _dec } from "../algorithms/qfq_affine.mjs";
import { yearlyRowMeta } from "../data_supply/sqlite_supply.mjs";
import { AnalysisCoverage } from "./sqlite_wire.mjs";

const integrity = (e) =>
  new MarketError("integrity", `MARKET_INTEGRITY: ${e.message ?? e}`);
const invalid = (m) =>
  new MarketError("invalid", `INVALID_MARKET_COMMAND: ${m}`);

// Decimal::normalize().to_string() — trailing-zero trim (Display
// normalizes) — same projection as market_read's decStr.
const decStr = (v) => {
  if (v !== null && typeof v === "object" && typeof v.m === "bigint") {
    let { m, s } = v;
    while (s > 0 && m % 10n === 0n) { m /= 10n; s -= 1; }
    if (m === 0n) return "0";
    return _dec.toString({ m, s });
  }
  return String(v);
};

// NaiveDate::year — "YYYY-MM-DD" or {y}.
const yearOf = (d) =>
  Number((typeof d === "string" ? d : d.s).slice(0, 4));

/**
 * sqliteMetrics(market) — InputRepository::observations.
 * `market` exposes `.read(fn)` — the same connection-scope boundary as
 * MarketBackend::read in Rust.
 */
export const sqliteMetrics = (market) => ({
  observations(reference, range) {
    return market.read((c) => {
      const r = MR.readRecord(c, reference.version_id, true, true);
      const v = r.version;
      const sym = reference.entity === null || reference.entity === undefined
        ? null : `${reference.entity.exchange}.${reference.entity.code}`;
      if (reference.dataset !== "market.yearly" ||
          v.period !== "Year" ||
          sym !== v.symbol ||
          r.raw?.integrity_sha256 !== reference.integrity_sha256)
        throw integrity(new Error(
          "fixed computation version identity/digest mismatch"));
      const coverage = new Map(); // BTreeMap<i32, Coverage>
      // Derived-completeness (issue #225, D2=C): for production yearly
      // versions without fixture coverage, the embedded
      // local_yearly_derivation_v1.evidence is the only completion basis.
      let derived = null; // {years:Set<i32>, complete:bool} | null
      const raw = r.raw ?? {};
      if (raw.analysis_coverage_v1 !== undefined) {
        if (!String(v.source).startsWith("fixture:"))
          throw integrity(new Error(
            "fixture coverage cannot certify a production source"));
        let evidence;
        try { evidence = AnalysisCoverage(raw.analysis_coverage_v1); }
        catch (e) { throw integrity(e); }
        if (evidence.years.length > 1000)
          throw invalid("coverage evidence budget exceeded");
        for (const y of evidence.years) {
          if (y.evidence_ref.trim() === "" ||
              !v.records.some((rec) => yearOf(rec.date) === y.year) ||
              coverage.has(y.year))
            throw integrity(new Error("coverage evidence year/ref mismatch"));
          coverage.set(y.year, y.status);
        }
      } else if (raw.local_yearly_derivation_v1 !== undefined) {
        // EmbeddedEvidence { years: BTreeSet<i32>, coverage:
        // CoverageEvidence } — serde field decode; absent evidence is
        // Null -> decode error -> integrity.
        const d = raw.local_yearly_derivation_v1;
        let ev;
        try {
          const e0 = d?.evidence === undefined ? null : d.evidence;
          if (e0 === null || typeof e0 !== "object" || !Array.isArray(e0.years) ||
              e0.coverage === null || typeof e0.coverage !== "object" ||
              typeof e0.coverage.status !== "string")
            throw new Error("invalid embedded coverage evidence");
          ev = { years: new Set(e0.years), coverage: e0.coverage };
        } catch (e) { throw integrity(e); }
        if (ev.years.size > 1000)
          throw invalid("coverage evidence budget exceeded");
        if (ev.coverage.status !== "unknown")
          derived = { years: ev.years,
            complete: ev.coverage.status === "complete" };
      }
      const observations = [];
      for (const b of v.records) {
        const y = yearOf(b.date);
        if (y < range.start_year || y > range.end_year) continue;
        const [first, last, unfinished] = yearlyRowMeta(c, r, b);
        // Option<NaiveDate>::unwrap + NaiveDate::from_str — absent or
        // malformed stored lineage is integrity, not a silent null.
        const dateOf = (x) => {
          const s = x === null || x === undefined ? null
            : (typeof x === "string" ? x : x.s);
          if (!/^\d{4}-\d{2}-\d{2}$/.test(s ?? ""))
            throw integrity(new Error(`invalid stored lineage date: ${s}`));
          return s;
        };
        const firstS = dateOf(first);
        const lastS = dateOf(last);
        const finished = !unfinished;
        let completion;
        if (derived !== null) {
          // D1=A: Complete requires verifier-certified base coverage of
          // the year AND a finished year; certified-but-open or
          // explicitly partial reads Incomplete, absent years Unknown.
          if (derived.years.has(y)) {
            completion = derived.complete && finished ? "complete" : "incomplete";
          } else completion = "unknown";
        } else {
          const cov = coverage.get(y);
          completion = cov === "complete" ? "complete"
            : cov === "incomplete" ? "incomplete" : "unknown";
        }
        observations.push({
          year: y,
          first: firstS,
          last: lastS,
          close: decStr(b.close),
          unfinished,
          completion,
        });
        if (observations.length > 1000)
          throw invalid("observation budget exceeded");
      }
      return observations;
    });
  },
});
