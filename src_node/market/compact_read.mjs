// NM-054 — compact (schema-19) market read path: compact_v19 row reads
// + compact_versions materialization + compact_repo synthesize/dispatch.
// Digests recompute over stored rows; every mismatch fails closed.
import * as CM from "./compact_market.mjs";
import * as RC from "./record_codec.mjs";
import { MarketError } from "./record_codec.mjs";
import * as Y from "../algorithms/yearly.mjs";
import { _dec } from "../algorithms/qfq_affine.mjs";

const integrity = (m) => new MarketError("integrity", `MARKET_INTEGRITY: ${m}`);
const notFound = () => new MarketError("not_found", "MARKET_NOT_FOUND");
const storage = (e) => new MarketError("storage", `MARKET_STORAGE: ${e.message ?? e}`);
void storage; // kept for parity with the legacy helper set
// .map_err(super::integrity) on compact_market::Error — the whole Display
// string ("compact market data invalid: X") is carried into Integrity.
const wrap = (f) => {
  try { return f(); } catch (e) {
    if (e instanceof MarketError && e.kind === "invalid" &&
        e.message.startsWith("compact market data invalid: ")) {
      throw integrity(e.message);
    }
    throw e;
  }
};

const DIGEST_FORMAT = "compact-market-version-v1";
const MAX_DELTA_DEPTH = 256;
const MAX_CHAIN_NODES = 257;

// ---------- row reads ----------
const fixedDigest = (raw) => {
  const b = Buffer.from(raw);
  if (b.length !== 32) throw integrity("stored digest length mismatch");
  return b;
};

const readVersionRow = (db, key) => {
  const row = db.get(
    "SELECT version_key,version_id,entity,period,state,source_binding_digest,parent_version_key,manifest_mode,delta_depth,record_count,first_day,last_day,coverage_status,evidence_digest,projection_digest,published_at,integrity_digest,adjustment,digest_format,created_at FROM data_versions WHERE version_key=?",
    key);
  if (!row) return null;
  if (row.adjustment !== 1n || row.digest_format !== DIGEST_FORMAT) {
    throw integrity("version adjustment/digest format mismatch");
  }
  return {
    key: row.version_key,
    version_id: row.version_id,
    entity: row.entity,
    period: wrap(() => CM.decodePeriod(row.period)),
    state: Number(row.state),
    source_binding_digest: fixedDigest(row.source_binding_digest),
    parent_version_key: row.parent_version_key,
    manifest_mode: Number(row.manifest_mode),
    delta_depth: Number(row.delta_depth),
    record_count: row.record_count,
    first_day: row.first_day,
    last_day: row.last_day,
    coverage_status: Number(row.coverage_status),
    evidence_digest: fixedDigest(row.evidence_digest),
    projection_digest: fixedDigest(row.projection_digest),
    integrity_digest: row.integrity_digest === null ? null : fixedDigest(row.integrity_digest),
    published_at: row.published_at,
    created_at: row.created_at,
  };
};

const readVersionRowById = (db, id) => {
  const key = db.get("SELECT version_key FROM data_versions WHERE version_id=?", id)?.version_key;
  return key === undefined ? null : readVersionRow(db, key);
};

// ---------- segments / clips / deltas ----------
const readSegment = (db, key) => {
  const row = db.get(
    "SELECT segment_digest,entity,period,first_day,last_day,row_count FROM bar_segments WHERE segment_key=?",
    key);
  if (!row) throw integrity("segment missing");
  const digest = fixedDigest(row.segment_digest);
  const period = wrap(() => CM.decodePeriod(row.period));
  const bars = db.query(
    "SELECT trade_day,open_cents,high_cents,low_cents,close_cents,volume,amount,r7_raw FROM segment_bars WHERE segment_key=? ORDER BY trade_day",
    key).map((b) => ({
      trade_day: b.trade_day,
      open_cents: b.open_cents, high_cents: b.high_cents,
      low_cents: b.low_cents, close_cents: b.close_cents,
      volume: b.volume, amount: b.amount,
      r7_raw: b.r7_raw === null ? null : (() => {
        const v = b.r7_raw;
        if (v < 0n || v > 4294967295n) throw integrity("r7_raw out of u32 range");
        return Number(v);
      })(),
    }));
  if (BigInt(bars.length) !== row.row_count) throw integrity("segment row_count mismatch");
  if (bars.length > 0 &&
      (bars[0].trade_day !== row.first_day || bars[bars.length - 1].trade_day !== row.last_day)) {
    throw integrity("segment day bounds mismatch");
  }
  for (const b of bars) wrap(() => CM.compactBarToBar(b, period));
  const recomputed = wrap(() => CM.segmentDigest(row.entity, period, bars));
  if (!recomputed.equals(digest)) throw integrity("segment digest mismatch");
  return { key, digest, entity: row.entity, period, bars };
};

const checkClipAgainstSegment = (segment, clip, entity, period) => {
  if (segment.entity !== entity || segment.period !== period) {
    throw integrity("clip segment entity/period mismatch");
  }
  if (!Buffer.from(clip.segment_digest).equals(segment.digest)) {
    throw integrity("clip segment digest mismatch");
  }
  if (segment.bars.length === 0) throw integrity("segment is empty");
  if (clip.clip_start_day < segment.bars[0].trade_day ||
      clip.clip_end_day > segment.bars[segment.bars.length - 1].trade_day) {
    throw integrity("clip bounds outside segment");
  }
  const actual = segment.bars.filter(
    (b) => b.trade_day >= clip.clip_start_day && b.trade_day <= clip.clip_end_day).length;
  if (actual !== clip.row_count) {
    throw integrity("clip row_count does not match segment rows");
  }
};

const readCheckpoint = (db, versionKey, entity, period) => {
  const rows = db.query(
    "SELECT seq,segment_key,clip_start_day,clip_end_day,row_count FROM version_checkpoint_clips WHERE version_key=? ORDER BY seq",
    versionKey);
  if (rows.length === 0) throw integrity("checkpoint missing");
  const clips = [];
  rows.forEach((r, index) => {
    if (r.seq !== BigInt(index)) throw integrity("checkpoint seq not contiguous");
    if (r.row_count <= 0n || r.row_count > 4294967295n) {
      throw integrity("checkpoint clip row_count out of bounds");
    }
    const segment = readSegment(db, r.segment_key);
    const clip = {
      segment_digest: segment.digest,
      clip_start_day: r.clip_start_day, clip_end_day: r.clip_end_day,
      row_count: Number(r.row_count),
    };
    checkClipAgainstSegment(segment, clip, entity, period);
    clips.push({ segment_key: r.segment_key, clip });
  });
  const plain = clips.map((c) => c.clip);
  const digest = wrap(() => CM.checkpointProjectionDigest(entity, period, plain));
  return [clips, digest];
};

const readDelta = (db, versionKey, parentProjection) => {
  const rows = db.query(
    "SELECT seq,delta_kind,range_start_day,range_end_day,segment_key,clip_start_day,clip_end_day,row_count FROM version_delta_clips WHERE version_key=? ORDER BY seq",
    versionKey);
  if (rows.length === 0) throw integrity("delta missing");
  const ops = [], plain = [];
  rows.forEach((r, index) => {
    if (r.seq !== BigInt(index)) throw integrity("delta seq not contiguous");
    const kind = Number(r.delta_kind);
    if (kind === 1) {
      if (r.segment_key === null || r.clip_start_day === null || r.clip_end_day === null) {
        throw integrity("upsert delta missing clip fields");
      }
      if (r.row_count <= 0n || r.row_count > 4294967295n) {
        throw integrity("upsert delta row_count out of bounds");
      }
      const segment = readSegment(db, r.segment_key);
      const clip = {
        segment_digest: segment.digest,
        clip_start_day: r.clip_start_day, clip_end_day: r.clip_end_day,
        row_count: Number(r.row_count),
      };
      checkClipAgainstSegment(segment, clip, segment.entity, segment.period);
      ops.push({
        range_start_day: r.range_start_day, range_end_day: r.range_end_day,
        kind: { kind: "upsert", segment_key: r.segment_key, clip },
      });
      plain.push({
        range_start_day: r.range_start_day, range_end_day: r.range_end_day,
        kind: { kind: "upsert", clip },
      });
    } else if (kind === 2) {
      if (r.segment_key !== null || r.clip_start_day !== null ||
          r.clip_end_day !== null || r.row_count !== 0n) {
        throw integrity("remove delta carries clip fields");
      }
      ops.push({ range_start_day: r.range_start_day, range_end_day: r.range_end_day,
        kind: { kind: "remove" } });
      plain.push({ range_start_day: r.range_start_day, range_end_day: r.range_end_day,
        kind: { kind: "remove" } });
    } else {
      throw integrity("unknown delta_kind");
    }
  });
  const plainOps = plain.map((o) => o);
  const [delta, projection] = wrap(() => CM.deltaProjectionDigest(parentProjection, plainOps));
  return [ops, delta, projection];
};

// ---------- chain reconstruction ----------
const reconstructVersion = (db, leafKey) => {
  const chain = [];
  const visited = new Set();
  let key = leafKey;
  for (;;) {
    if (visited.has(key)) throw integrity("version chain contains a cycle");
    visited.add(key);
    const row = readVersionRow(db, key);
    if (row === null) throw integrity("version chain node missing");
    if (row.manifest_mode === 1) {
      if (row.parent_version_key !== null || row.delta_depth !== 0) {
        throw integrity("checkpoint version carries parent/depth");
      }
      chain.push(row);
      break;
    }
    if (row.manifest_mode === 2) {
      if (row.delta_depth < 1 || row.delta_depth > MAX_DELTA_DEPTH) {
        throw integrity("delta depth out of bounds");
      }
      if (row.parent_version_key === null) throw integrity("delta version lacks parent");
      chain.push(row);
      key = row.parent_version_key;
    } else {
      throw integrity("unknown manifest_mode");
    }
    if (chain.length > MAX_CHAIN_NODES) throw integrity("version chain exceeds depth bound");
  }
  chain.reverse();
  const projections = [];
  let bars = [];
  chain.forEach((row, index) => {
    if (index === 0) {
      const [clips, projection] = readCheckpoint(db, row.key, row.entity, row.period);
      if (!projection.equals(row.projection_digest)) {
        throw integrity("checkpoint projection digest mismatch");
      }
      bars = flattenClips(db, row.entity, row.period, clips);
    } else {
      const parentRow = projections[projections.length - 1][0];
      if (row.parent_version_key !== parentRow.key ||
          row.delta_depth !== parentRow.delta_depth + 1 ||
          row.entity !== parentRow.entity || row.period !== parentRow.period) {
        throw integrity("version chain linkage mismatch");
      }
      if (parentRow.state !== 3) throw integrity("delta parent is not published");
      const [ops, , projection] = readDelta(db, row.key, parentRow.projection_digest);
      if (!projection.equals(row.projection_digest)) {
        throw integrity("delta projection digest mismatch");
      }
      bars = applyDelta(db, bars, ops, row.entity, row.period);
      if (bars.length === 0) throw integrity("delta projection is empty");
    }
    projections.push([row, bars]);
  });
  return projections;
};

const reconstructLeaf = (db, key) => {
  const chain = reconstructVersion(db, key);
  return chain[chain.length - 1];
};

// leaf bars restricted to [start,end] — decodes only overlapping segments
const leafBarsInRange = (db, leafKey, startDay, endDay) => {
  if (startDay > endDay) return [];
  const nodes = [];
  const visited = new Set();
  let key = leafKey;
  for (;;) {
    if (visited.has(key)) throw integrity("version chain contains a cycle");
    visited.add(key);
    const row = readVersionRow(db, key);
    if (row === null) throw integrity("version chain node missing");
    if (row.manifest_mode === 1) {
      if (row.parent_version_key !== null || row.delta_depth !== 0) {
        throw integrity("checkpoint version carries parent/depth");
      }
      nodes.push(row);
      break;
    }
    if (row.manifest_mode === 2) {
      if (row.delta_depth < 1 || row.delta_depth > MAX_DELTA_DEPTH) {
        throw integrity("delta depth out of bounds");
      }
      if (row.parent_version_key === null) throw integrity("delta version lacks parent");
      nodes.push(row);
      key = row.parent_version_key;
    } else {
      throw integrity("unknown manifest_mode");
    }
    if (nodes.length > MAX_CHAIN_NODES) throw integrity("version chain exceeds depth bound");
  }
  nodes.reverse();
  const map = new Map();
  const root = nodes[0];
  for (const r of db.query(
    "SELECT segment_key,clip_start_day,clip_end_day,row_count FROM version_checkpoint_clips WHERE version_key=? AND clip_start_day<=? AND clip_end_day>=? ORDER BY seq",
    root.key, endDay, startDay)) {
    if (r.row_count <= 0n || r.row_count > 4294967295n) {
      throw integrity("checkpoint clip row_count out of bounds");
    }
    const segment = readSegment(db, r.segment_key);
    const clip = {
      segment_digest: segment.digest,
      clip_start_day: r.clip_start_day, clip_end_day: r.clip_end_day,
      row_count: Number(r.row_count),
    };
    checkClipAgainstSegment(segment, clip, root.entity, root.period);
    for (const bar of segment.bars) {
      if (bar.trade_day >= (clip.clip_start_day > startDay ? clip.clip_start_day : startDay) &&
          bar.trade_day <= (clip.clip_end_day < endDay ? clip.clip_end_day : endDay)) {
        map.set(bar.trade_day, bar);
      }
    }
  }
  for (const row of nodes.slice(1)) {
    for (const r of db.query(
      "SELECT delta_kind,range_start_day,range_end_day,segment_key,clip_start_day,clip_end_day,row_count FROM version_delta_clips WHERE version_key=? AND range_start_day<=? AND range_end_day>=? ORDER BY seq",
      row.key, endDay, startDay)) {
      const kind = Number(r.delta_kind);
      if (kind === 1) {
        if (r.segment_key === null || r.clip_start_day === null || r.clip_end_day === null) {
          throw integrity("upsert delta missing clip fields");
        }
        if (r.row_count <= 0n || r.row_count > 4294967295n) {
          throw integrity("upsert delta row_count out of bounds");
        }
        const segment = readSegment(db, r.segment_key);
        const clip = {
          segment_digest: segment.digest,
          clip_start_day: r.clip_start_day, clip_end_day: r.clip_end_day,
          row_count: Number(r.row_count),
        };
        checkClipAgainstSegment(segment, clip, row.entity, row.period);
        for (const bar of segment.bars) {
          if (bar.trade_day >= (clip.clip_start_day > startDay ? clip.clip_start_day : startDay) &&
              bar.trade_day <= (clip.clip_end_day < endDay ? clip.clip_end_day : endDay)) {
            map.set(bar.trade_day, bar);
          }
        }
      } else if (kind === 2) {
        const [ds, de] = [
          r.range_start_day > startDay ? r.range_start_day : startDay,
          r.range_end_day < endDay ? r.range_end_day : endDay];
        for (const day of [...map.keys()]) {
          if (day >= ds && day <= de) map.delete(day);
        }
      } else {
        throw integrity("unknown delta_kind");
      }
    }
  }
  return [...map.values()].sort((a, b) => (a.trade_day < b.trade_day ? -1 : a.trade_day > b.trade_day ? 1 : 0));
};

const flattenClips = (db, entity, period, clips) => {
  if (clips.length === 0) throw integrity("checkpoint requires at least one clip");
  const bars = [];
  let prevEnd = null;
  for (const stored of clips) {
    const segment = readSegment(db, stored.segment_key);
    checkClipAgainstSegment(segment, stored.clip, entity, period);
    if (prevEnd !== null && stored.clip.clip_start_day <= prevEnd) {
      throw integrity("checkpoint clips overlap or unordered");
    }
    prevEnd = stored.clip.clip_end_day;
    for (const bar of segment.bars) {
      if (bar.trade_day >= stored.clip.clip_start_day && bar.trade_day <= stored.clip.clip_end_day) {
        bars.push(bar);
      }
    }
  }
  if (bars.length === 0) throw integrity("checkpoint projection is empty");
  return bars;
};

const applyDelta = (db, base, ops, entity, period) => {
  const plain = ops.map((op) => ({
    range_start_day: op.range_start_day, range_end_day: op.range_end_day,
    kind: op.kind.kind === "upsert" ? { kind: "upsert", clip: op.kind.clip } : { kind: "remove" },
  }));
  if (plain.length !== 0) wrap(() => CM.deltaDigest(plain));
  const map = new Map();
  for (const b of base) map.set(b.trade_day, b);
  for (const op of ops) {
    if (op.kind.kind === "upsert") {
      const stored = op.kind;
      const segment = readSegment(db, stored.segment_key);
      checkClipAgainstSegment(segment, stored.clip, entity, period);
      for (const bar of segment.bars) {
        if (bar.trade_day >= stored.clip.clip_start_day &&
            bar.trade_day <= stored.clip.clip_end_day) {
          map.set(bar.trade_day, bar);
        }
      }
    } else {
      for (const day of [...map.keys()]) {
        if (day >= op.range_start_day && day <= op.range_end_day) map.delete(day);
      }
    }
  }
  return [...map.values()].sort((a, b) => (a.trade_day < b.trade_day ? -1 : a.trade_day > b.trade_day ? 1 : 0));
};

// ---------- evidence / bindings / lineage ----------
const readEvidence = (db, digest) => {
  const row = db.get(
    "SELECT evidence_kind,canonical_json FROM evidence_bundles WHERE evidence_digest=?",
    Buffer.from(digest));
  if (!row) throw integrity("evidence bundle missing");
  const kind = wrap(() => CM.parseEvidenceKind(row.evidence_kind));
  wrap(() => CM.verifyEvidence(kind, row.canonical_json, digest));
  return [kind, row.canonical_json];
};

const verifyVersionEvidence = (db, digest) => {
  const [kind, canonical] = readEvidence(db, digest);
  if (kind !== "version") throw integrity("version evidence kind mismatch");
  let v;
  try { v = JSON.parse(canonical); }
  catch { throw integrity("expected value at line 1 column 1"); }
  return v; // {source, coverage_reason, unit_currency, adjustment_evidence}
};

const readSourceBinding = (db, digest) => {
  const row = db.get(
    "SELECT generation_id,generation_manifest_sha256,day_member_sha256,security_event_sha256,evidence_digest FROM source_bindings WHERE source_binding_digest=?",
    Buffer.from(digest));
  if (!row) throw integrity("source binding missing");
  const binding = {
    generation_id: row.generation_id,
    generation_manifest_sha256: fixedDigest(row.generation_manifest_sha256),
    day_member_sha256: fixedDigest(row.day_member_sha256),
    security_event_sha256: fixedDigest(row.security_event_sha256),
    evidence_digest: fixedDigest(row.evidence_digest),
  };
  const recomputed = wrap(() => CM.sourceBindingDigest(binding));
  if (!recomputed.equals(Buffer.from(digest))) throw integrity("source binding digest mismatch");
  const [kind] = readEvidence(db, binding.evidence_digest);
  if (kind !== "generation") throw integrity("source binding evidence is not generation");
  return binding;
};

const storedLineage = (db, key) => {
  const r = db.get(
    "SELECT base_daily_version_key,algorithm_version,first_trade_day,last_trade_day,as_of_day FROM yearly_lineage WHERE version_key=?",
    key);
  return r === undefined ? null : {
    base_daily_version_key: r.base_daily_version_key,
    algorithm_version: r.algorithm_version,
    first_trade_day: r.first_trade_day, last_trade_day: r.last_trade_day,
    as_of_day: r.as_of_day,
  };
};

const verifyStoredLineage = (db, row) => {
  if (row.period === "Day") {
    if (storedLineage(db, row.key) !== null) {
      throw integrity("daily version carries yearly lineage");
    }
    return null;
  }
  const lineage = storedLineage(db, row.key);
  if (lineage === null) throw integrity("yearly version lacks lineage");
  const base = readVersionRow(db, lineage.base_daily_version_key);
  if (base === null) throw integrity("yearly base version missing");
  if (base.state !== 3 || base.period !== "Day" || base.entity !== row.entity ||
      !base.source_binding_digest.equals(row.source_binding_digest) ||
      lineage.algorithm_version !== Y.ALGORITHM_VERSION ||
      lineage.first_trade_day > lineage.last_trade_day ||
      lineage.last_trade_day > lineage.as_of_day) {
    throw integrity("yearly lineage metadata mismatch");
  }
  wrap(() => CM.epochDate(Number(lineage.first_trade_day)));
  wrap(() => CM.epochDate(Number(lineage.last_trade_day)));
  wrap(() => CM.epochDate(Number(lineage.as_of_day)));
  return {
    base_daily_version_id: base.version_id,
    algorithm_version: lineage.algorithm_version,
    first_trade_day: lineage.first_trade_day,
    last_trade_day: lineage.last_trade_day,
    as_of_day: lineage.as_of_day,
  };
};

// verify_lineage — full base re-aggregation equality for staged/validated
const verifyLineage = (db, entity, period, sourceBinding, bars, lineage) => {
  const base = readVersionRow(db, lineage.base_daily_version_key);
  if (base === null) throw integrity("yearly base version missing");
  if (base.state !== 3) throw integrity("yearly base version is not published");
  if (base.period !== "Day" || base.entity !== entity) {
    throw integrity("yearly base entity/period mismatch");
  }
  if (!base.source_binding_digest.equals(sourceBinding)) {
    throw integrity("yearly source binding differs from base");
  }
  if (lineage.algorithm_version !== Y.ALGORITHM_VERSION) {
    throw integrity("yearly lineage algorithm mismatch");
  }
  if (lineage.first_trade_day > lineage.last_trade_day ||
      lineage.last_trade_day > lineage.as_of_day) {
    throw integrity("yearly lineage bounds inverted");
  }
  wrap(() => CM.epochDate(Number(lineage.first_trade_day)));
  wrap(() => CM.epochDate(Number(lineage.last_trade_day)));
  const asOf = wrap(() => CM.epochDate(Number(lineage.as_of_day)));
  if (storedLineage(db, base.key) !== null) {
    throw integrity("yearly base carries a lineage row");
  }
  const records = bars.map((b) => wrap(() => CM.compactBarToBar(b, period)));
  const years = [...new Set(records.map((r) => Number(r.date.slice(0, 4))))].sort((a, b) => a - b);
  if (base.integrity_digest === null) {
    throw integrity("published base lacks integrity digest");
  }
  const baseBars = leafBarsInRange(
    db, lineage.base_daily_version_key, lineage.first_trade_day, lineage.last_trade_day);
  const baseEvidence = verifyVersionEvidence(db, base.evidence_digest);
  const baseVersion = toEffectiveVersion(db, base, baseBars, baseEvidence);
  // project to the ported aggregate's evidence shape ({confirmed,value})
  const ev = (e) => ({ confirmed: e.status === "Confirmed", value: e.value ?? null });
  const baseView = {
    period: "day", yearly_derivation: null,
    version_id: baseVersion.version_id, symbol: baseVersion.symbol,
    source: baseVersion.source,
    adjustment_evidence: ev(baseVersion.adjustment_evidence),
    unit_currency: Object.fromEntries(
      ["price_currency", "price_unit", "volume_unit", "amount_currency", "amount_unit"]
        .map((k) => [k, ev(baseVersion.unit_currency[k])])),
    records: baseVersion.records,
  };
  let annual;
  try {
    annual = Y.aggregate(baseView, years, asOf);
  } catch (e) {
    throw integrity(`yearly aggregation failed: ${e.message}`);
  }
  const annualBars = annual.records.map((r) => r.bar);
  const eq = annualBars.length === records.length && annualBars.every((b, i) =>
    b.date === records[i].date &&
    decEqDec(b.open, records[i].open) && decEqDec(b.high, records[i].high) &&
    decEqDec(b.low, records[i].low) && decEqDec(b.close, records[i].close) &&
    decEqOpt(b.volume, records[i].volume) && decEqOpt(b.amount, records[i].amount));
  if (!eq) throw integrity("yearly projection does not match base aggregation");
  if ((annual.records[0]?.lineage.first_trading_date ?? null) !==
        wrap(() => CM.epochDate(Number(lineage.first_trade_day))) ||
      (annual.records[annual.records.length - 1]?.lineage.last_trading_date ?? null) !==
        wrap(() => CM.epochDate(Number(lineage.last_trade_day)))) {
    throw integrity("yearly lineage bounds do not match aggregation");
  }
  return {
    base_daily_version_id: base.version_id,
    algorithm_version: lineage.algorithm_version,
    first_trade_day: lineage.first_trade_day,
    last_trade_day: lineage.last_trade_day,
    as_of_day: lineage.as_of_day,
  };
};

const decEqDec = (a, b) => {
  const s = Math.max(a.s, b.s);
  return a.m * 10n ** BigInt(s - a.s) === b.m * 10n ** BigInt(s - b.s);
};
const decEqOpt = (a, b) => (a === null || a === undefined) === (b === null || b === undefined) &&
  (a === null || a === undefined || decEqDec(a, b));

const checkBarsUnits = (evidence, bars) => {
  const unit = evidence.unit_currency;
  const confirmed = (e) => e.status === "Confirmed" && e.value !== null && e.value !== "";
  if (!confirmed(unit.price_currency) || !confirmed(unit.price_unit)) {
    throw integrity("price unit/currency evidence is not confirmed");
  }
  if (bars.some((b) => b.volume !== null && b.volume !== undefined) && !confirmed(unit.volume_unit)) {
    throw integrity("volume unit evidence missing for volume bars");
  }
  if (bars.some((b) => b.amount !== null && b.amount !== undefined) &&
      !(confirmed(unit.amount_currency) && confirmed(unit.amount_unit))) {
    throw integrity("amount unit evidence missing for amount bars");
  }
};

// ---------- node verification ----------
const verifyPublishedFields = (db, row) => {
  if (row.state !== 3) throw integrity("version chain parent is not published");
  const published = row.published_at;
  if (published === null) throw integrity("published version lacks published_at");
  if (row.integrity_digest === null) {
    throw integrity("published version lacks integrity digest");
  }
  const identity = buildIdentity(db, row, published);
  const recomputed = wrap(() => CM.versionIntegrityDigest(identity));
  if (!recomputed.equals(row.integrity_digest)) {
    throw integrity("version integrity digest mismatch");
  }
};

const verifyNodeCommon = (db, row, bars) => {
  const evidence = verifyVersionEvidence(db, row.evidence_digest);
  readSourceBinding(db, row.source_binding_digest);
  checkBarsUnits(evidence, bars);
  if (bars.length === 0 || BigInt(bars.length) !== row.record_count ||
      bars[0].trade_day !== row.first_day ||
      bars[bars.length - 1].trade_day !== row.last_day) {
    throw integrity("version metadata does not match projection");
  }
  if (row.period === "Day") {
    if (storedLineage(db, row.key) !== null) {
      throw integrity("daily version carries yearly lineage");
    }
  } else {
    const lineage = storedLineage(db, row.key);
    if (lineage === null) throw integrity("yearly version lacks lineage");
    verifyLineage(db, row.entity, row.period, row.source_binding_digest, bars, lineage);
  }
  return evidence;
};

const verifyPublishedNode = (db, row, bars) => {
  verifyPublishedFields(db, row);
  return verifyNodeCommon(db, row, bars);
};

const verifyStaged = (db, leaf) => {
  const chain = reconstructVersion(db, leaf.key);
  chain.forEach(([row, bars], index) => {
    verifyNodeCommon(db, row, bars);
    if (index + 1 < chain.length) verifyPublishedFields(db, row);
  });
};

const fixedByKey = (db, key) => {
  const chain = reconstructVersion(db, key);
  let leafEvidence = null;
  for (const [row, bars] of chain) {
    leafEvidence = verifyPublishedNode(db, row, bars);
  }
  const [leaf, bars] = chain[chain.length - 1];
  const version = toEffectiveVersion(db, leaf, bars, leafEvidence);
  if (leaf.integrity_digest === null) {
    throw integrity("published version lacks integrity digest");
  }
  return { key: leaf.key, integrity_digest: leaf.integrity_digest, version };
};

const buildIdentity = (db, row, publishedAt) => {
  let parentVersionId = null;
  if (row.parent_version_key !== null) {
    const parent = readVersionRow(db, row.parent_version_key);
    if (parent === null) throw integrity("parent version missing");
    if (parent.state !== 3 || parent.entity !== row.entity ||
        parent.period !== row.period || parent.delta_depth + 1 !== row.delta_depth) {
      throw integrity("version parent linkage mismatch");
    }
    parentVersionId = parent.version_id;
  }
  const yearlyLineage = verifyStoredLineage(db, row);
  const manifestMode = row.manifest_mode === 1 ? "checkpoint"
    : row.manifest_mode === 2 ? "delta" : (() => { throw integrity("unknown manifest_mode"); })();
  const coverageStatus = row.coverage_status === 1 ? "Complete"
    : row.coverage_status === 2 ? "Incomplete"
    : row.coverage_status === 3 ? "Unknown"
    : (() => { throw integrity("unknown coverage_status"); })();
  if (row.delta_depth < 0 || row.delta_depth > 4294967295) {
    throw integrity("delta_depth out of u32 range");
  }
  if (row.record_count < 0n) throw integrity("record_count out of u64 range");
  return {
    version_id: row.version_id, entity: row.entity, period: row.period,
    source_binding_digest: row.source_binding_digest,
    parent_version_id: parentVersionId,
    manifest_mode: manifestMode,
    delta_depth: row.delta_depth,
    projection_digest: row.projection_digest,
    record_count: row.record_count,
    first_day: row.first_day, last_day: row.last_day,
    coverage_status: coverageStatus,
    evidence_digest: row.evidence_digest,
    published_at: publishedAt,
    yearly_lineage: yearlyLineage,
  };
};

// ---------- EffectiveVersion projection ----------
const toEffectiveVersion = (db, row, bars, evidence) => {
  const records = bars.map((b) => wrap(() => CM.compactBarToBar(b, row.period)));
  const lin = verifyStoredLineage(db, row);
  const yearlyDerivation = lin === null ? null : {
    base_daily_version_id: lin.base_daily_version_id,
    algorithm_version: lin.algorithm_version,
    first_trading_date: wrap(() => CM.epochDate(Number(lin.first_trade_day))),
    last_trading_date: wrap(() => CM.epochDate(Number(lin.last_trade_day))),
  };
  if (row.published_at === null) throw integrity("published version lacks published_at");
  // DateTime::parse_from_rfc3339().with_timezone(&Utc) — non-canonical
  // offsets accepted here; the stored integrity digest binds the string.
  const publishedAt = wrap(() => CM.parseRfc3339Utc(row.published_at));
  return {
    version_id: row.version_id, symbol: row.entity, period: row.period,
    adjustment: "Forward", source: evidence.source, records,
    published_at: publishedAt,
    validation: {
      record_count: bars.length, unique_record_count: bars.length, duplicate_count: 0,
    },
    unit_currency: evidence.unit_currency,
    adjustment_evidence: evidence.adjustment_evidence,
    yearly_derivation: yearlyDerivation,
  };
};

const unpublishedVersion = (db, row, bars, evidence) => {
  const records = bars.map((b) => wrap(() => CM.compactBarToBar(b, row.period)));
  const lin = verifyStoredLineage(db, row);
  const yearlyDerivation = lin === null ? null : {
    base_daily_version_id: lin.base_daily_version_id,
    algorithm_version: lin.algorithm_version,
    first_trading_date: wrap(() => CM.epochDate(Number(lin.first_trade_day))),
    last_trading_date: wrap(() => CM.epochDate(Number(lin.last_trade_day))),
  };
  // DateTime::parse_from_rfc3339(created_at).with_timezone(&Utc)
  const publishedAt = wrap(() => CM.parseRfc3339Utc(row.created_at));
  return {
    version_id: row.version_id, symbol: row.entity, period: row.period,
    adjustment: "Forward", source: evidence.source, records,
    published_at: publishedAt,
    validation: {
      record_count: bars.length, unique_record_count: bars.length, duplicate_count: 0,
    },
    unit_currency: evidence.unit_currency,
    adjustment_evidence: evidence.adjustment_evidence,
    yearly_derivation: yearlyDerivation,
  };
};

// ---------- materialize ----------
const STATE = { 1: "staged", 2: "validated", 3: "published" };

export const materialize = (db, versionId) => {
  const row = readVersionRowById(db, versionId);
  if (row === null) throw notFound();
  const state = STATE[row.state];
  if (state === undefined) throw integrity("malformed version state");
  const evidence = verifyVersionEvidence(db, row.evidence_digest);
  let version;
  if (state === "published") {
    version = fixedByKey(db, row.key).version;
  } else {
    verifyStaged(db, row);
    const [leaf, bars] = reconstructLeaf(db, row.key);
    version = unpublishedVersion(db, leaf, bars, evidence);
  }
  const sourceBinding = readSourceBinding(db, row.source_binding_digest);
  const [kind, canonical] = readEvidence(db, sourceBinding.evidence_digest);
  if (kind !== "generation") throw integrity("source binding evidence is not generation");
  let generationEvidence;
  try { generationEvidence = JSON.parse(canonical); }
  catch { throw integrity("expected value at line 1 column 1"); }
  let parentVersionId = null;
  if (row.parent_version_key !== null) {
    const parent = readVersionRow(db, row.parent_version_key);
    if (parent === null) throw integrity("parent version missing");
    parentVersionId = parent.version_id;
  }
  const coverage = row.coverage_status === 1 ? "Complete"
    : row.coverage_status === 2 ? "Incomplete"
    : row.coverage_status === 3 ? "Unknown"
    : (() => { throw integrity("unknown coverage_status"); })();
  return {
    key: row.key, state, version,
    coverage_status: coverage,
    coverage_reason: evidence.coverage_reason,
    source_binding: sourceBinding,
    generation_evidence: generationEvidence,
    parent_version_id: parentVersionId,
    projection_digest: row.projection_digest,
    integrity_digest: row.integrity_digest,
  };
};

// serde Vec<SnapshotInputBinding> — {role,member,archive_sha256,member_sha256}
// `undefined` (absent key) -> None; `null` -> serde error (Vec != null).
const parseSnapshotInputs = (v) => {
  if (v === undefined) return null;
  if (!Array.isArray(v)) throw integrity("snapshot_inputs shape mismatch");
  return v.map((b) => {
    if (b === null || typeof b !== "object" || Array.isArray(b) ||
        typeof b.role !== "string" || typeof b.member !== "string" ||
        typeof b.archive_sha256 !== "string" || typeof b.member_sha256 !== "string") {
      throw integrity("snapshot_inputs shape mismatch");
    }
    return { role: b.role, member: b.member,
      archive_sha256: b.archive_sha256, member_sha256: b.member_sha256 };
  });
};

// ---------- synthesize (serde-order raw JSON) ----------
const serdeBar = (b) => ({
  date: b.date,
  open: _dec.toString(b.open), high: _dec.toString(b.high),
  low: _dec.toString(b.low), close: _dec.toString(b.close),
  volume: b.volume === null ? null : _dec.toString(b.volume),
  amount: b.amount === null ? null : _dec.toString(b.amount),
  r7_raw: b.r7_raw ?? null,
});

const serdeEvidence = (e) => ({ status: e.status, value: e.value ?? null, reason: e.reason });
const serdeUnitCurrency = (u) => ({
  price_currency: serdeEvidence(u.price_currency),
  price_unit: serdeEvidence(u.price_unit),
  volume_unit: serdeEvidence(u.volume_unit),
  amount_currency: serdeEvidence(u.amount_currency),
  amount_unit: serdeEvidence(u.amount_unit),
});
const serdeLineage = (l) => l === null ? null : ({
  base_daily_version_id: l.base_daily_version_id,
  algorithm_version: l.algorithm_version,
  first_trading_date: l.first_trading_date,
  last_trading_date: l.last_trading_date,
});
const serdeProvenance = (p) => ({
  source: p.source, source_period: p.source_period,
  source_adjustment: p.source_adjustment, symbol: p.symbol,
  acquired_at: CM.serdeDateTime(p.acquired_at), adapter_version: p.adapter_version,
  yearly_derivation: serdeLineage(p.yearly_derivation ?? null),
  snapshot_inputs: p.snapshot_inputs ?? null,
});
const serdeBatch = (b) => ({
  symbol: b.symbol, period: b.period, adjustment: b.adjustment,
  bars: b.bars.map(serdeBar),
  provenance: serdeProvenance(b.provenance),
  coverage: { status: b.coverage.status, reason: b.coverage.reason },
  unit_currency: serdeUnitCurrency(b.unit_currency),
  adjustment_evidence: serdeEvidence(b.adjustment_evidence),
});
const serdeVersion = (v) => ({
  version_id: v.version_id, symbol: v.symbol, period: v.period,
  adjustment: v.adjustment, source: v.source,
  records: v.records.map(serdeBar),
  published_at: CM.serdeDateTime(v.published_at),
  validation: {
    record_count: v.validation.record_count,
    unique_record_count: v.validation.unique_record_count,
    duplicate_count: v.validation.duplicate_count,
  },
  unit_currency: serdeUnitCurrency(v.unit_currency),
  adjustment_evidence: serdeEvidence(v.adjustment_evidence),
  yearly_derivation: serdeLineage(v.yearly_derivation),
});

// compact_repo::synthesize — typed columns -> legacy-shaped raw envelope
const synthesize = (m) => {
  const version = m.version;
  const provenance = {
    source: version.source,
    source_period: version.yearly_derivation !== null ? "day" : version.period === "Day" ? "day" : "year",
    source_adjustment: "qfq",
    symbol: version.symbol,
    acquired_at: version.published_at,
    adapter_version: "effective-publish",
    yearly_derivation: version.yearly_derivation,
    snapshot_inputs: parseSnapshotInputs(m.generation_evidence?.snapshot_inputs),
  };
  const batch = {
    symbol: version.symbol, period: version.period, adjustment: "Forward",
    bars: version.records, provenance,
    coverage: { status: m.coverage_status, reason: m.coverage_reason },
    unit_currency: version.unit_currency,
    adjustment_evidence: version.adjustment_evidence,
  };
  let raw;
  if (m.state === "published") {
    const slim = { ...version, records: [] };
    raw = serdeVersion(slim);
    raw.batch = serdeBatch({ ...batch, bars: [] });
  } else {
    raw = serdeVersion(version);
    raw.batch = serdeBatch(batch);
  }
  raw.compact_source = {
    generation_id: m.source_binding.generation_id,
    generation_manifest_sha256: CM.digestHex(m.source_binding.generation_manifest_sha256),
    day_member_sha256: CM.digestHex(m.source_binding.day_member_sha256),
    security_event_sha256: CM.digestHex(m.source_binding.security_event_sha256),
    generation_evidence: m.generation_evidence,
  };
  if (m.parent_version_id !== null && m.parent_version_id !== undefined) {
    raw.compact_parent_version_id = m.parent_version_id;
  }
  if (m.state === "published") {
    if (m.integrity_digest === null) {
      throw integrity("published version lacks integrity digest");
    }
    raw.integrity_sha256 = CM.digestHex(m.integrity_digest);
  } else {
    raw.integrity_sha256 = wrap(() => legacyDigestCompat(raw));
  }
  return { raw, version, batch };
};

// compat::legacy_digest — sha256 over canonical JSON minus integrity_sha256
const legacyDigestCompat = (raw) => RC.legacyDigest(raw);

export const materializeChecked = (db, versionId, staged) => {
  const m = materialize(db, versionId);
  const allowed = m.state === "published" ? !staged
    : m.state === "validated" ? true
    : staged;
  if (!allowed) throw notFound();
  return synthesize(m);
};

// ---------- compact head / versions / range ----------
export const headId = (db, scope) =>
  db.get(
    "SELECT v.version_id FROM effective_heads h JOIN data_versions v ON v.version_key=h.version_key WHERE h.entity=? AND h.period=? AND h.adjustment=1",
    scope.symbol, scope.period === "Day" ? 1 : 2)?.version_id ?? null;

export const compactVersions = (db, scope) => {
  const orphan = db.get(
    "SELECT EXISTS(SELECT 1 FROM task_publications tp LEFT JOIN data_versions v ON v.version_key=tp.version_key WHERE v.version_key IS NULL) AS x").x === 1n;
  if (orphan) throw integrity("task publication references missing version");
  const keys = db.query(
    "SELECT version_key FROM data_versions WHERE entity=? AND period=? AND adjustment=1 AND state=3 ORDER BY published_at DESC,version_key DESC",
    scope.symbol, scope.period === "Day" ? 1 : 2).map((r) => r.version_key);
  return keys.map((key) => {
    const row = readVersionRow(db, key);
    if (row === null) throw integrity("version row missing");
    if (row.state !== 3) throw integrity("published index references unpublished version");
    const evidence = verifyVersionEvidence(db, row.evidence_digest);
    readSourceBinding(db, row.source_binding_digest);
    const lineage = verifyStoredLineage(db, row);
    if (row.published_at === null) throw integrity("published version lacks published_at");
    if (row.integrity_digest === null) throw integrity("published version lacks integrity digest");
    const identity = buildIdentity(db, row, row.published_at);
    const recomputed = wrap(() => CM.versionIntegrityDigest(identity));
    if (!recomputed.equals(row.integrity_digest)) {
      throw integrity("version integrity digest mismatch");
    }
    void lineage;
    // materialize each (CompactRepo::versions maps through materialize)
    return materialize(db, row.version_id).version;
  });
};

// CompactRepo::range via SqliteMarket::range dispatch semantics:
// read_record (materialize) + range_rows (materialize + filter)
export const compactRangeRows = (db, id, p, range) => {
  const record = materializeChecked(db, id, false);
  if (record.version.period !== p) {
    throw integrity("range version period mismatch");
  }
  const start = CM.epochDay(range.start), end = CM.epochDay(range.end);
  const after = range.after === null || range.after === undefined ? null : CM.epochDay(range.after);
  return record.version.records
    .filter((b) => {
      const d = CM.epochDay(b.date);
      return d >= start && d <= end && (after === null || d > after);
    })
    .slice(0, range.limit + 1);
};
