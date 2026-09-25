/* Fixture 数据 —— 形状即 API 契约。
   字段对齐 schema-19 读口产物：task snapshot、item outcome、
   data_versions 元数据、effective_heads、dbstat 采样。 */
const FIX = {
  generated_at: "2026-10-01T09:30:00Z",
  tasks: [
    {
      task_id: "task-7f3a2c91", operation: "update_data", dataset: "market.daily",
      status: "running", created_at: "2026-10-01T09:00:12Z",
      items: { total: 5221, finished: 4310, succeeded: 4308, failed: 2, running: 1 },
      elapsed_s: 248, per_item_ms: 57,
      phases: { parse_ms: 4, member_hash_ms: 9, base_read_ms: 8, diff_ms: 3, stage_ms: 12, commit_ms: 18, fsync_ms: 3 },
    },
    {
      task_id: "task-4b1e88d0", operation: "collect_data", dataset: "market.daily",
      status: "succeeded", created_at: "2026-09-22T01:00:00Z",
      items: { total: 11801, finished: 11801, succeeded: 11783, failed: 18, running: 0 },
      elapsed_s: 174, per_item_ms: 15,
    },
    {
      task_id: "task-9c02aa17", operation: "yearly_publication", dataset: "market.yearly",
      status: "pending", created_at: "2026-10-01T08:45:00Z",
      items: { total: 11801, finished: 0, succeeded: 0, failed: 0, running: 0 },
      elapsed_s: 0, per_item_ms: null,
    },
  ],
  failures: [
    { task_id: "task-7f3a2c91", item_id: "item-sh.600519", symbol: "sh.600519", class: "integrity", error: "delta projection digest mismatch", at: "2026-10-01T09:14:02Z" },
    { task_id: "task-7f3a2c91", item_id: "item-sz.300750", symbol: "sz.300750", class: "source_violation", error: ".day member malformed: non-increasing date at record 1822", at: "2026-10-01T09:22:41Z" },
    { task_id: "task-4b1e88d0", item_id: "item-sh.000001", symbol: "sh.000001", class: "source_violation", error: "invalid market data record: close is outside low/high", at: "2026-09-22T01:02:11Z" },
    { task_id: "task-4b1e88d0", item_id: "item-sz.131804", symbol: "sz.131804", class: "source_violation", error: "invalid source response: .day member carries no records", at: "2026-09-22T01:04:55Z" },
    { task_id: "task-4b1e88d0", item_id: "item-sz.399001", symbol: "sz.399001", class: "source_violation", error: "invalid market data record: close is outside low/high", at: "2026-09-22T01:58:03Z" },
  ],
  coverage: (() => {
    // 生成 240 条实体覆盖行做窗口化列表压测；首尾各放几条"真实感"数据
    const rows = [
      { entity: "sh.600519", last_day: "2026-09-30", records: 8420, coverage: "complete", delta_depth: 3, head: "v1-sh.600519-d217" },
      { entity: "sz.300750", last_day: "2026-09-29", records: 3050, coverage: "incomplete", delta_depth: 11, head: "v1-sz.300750-d198" },
      { entity: "sh.000001", last_day: "2026-09-30", records: 8521, coverage: "complete", delta_depth: 1, head: "v1-sh.000001-d244" },
    ];
    for (let i = 0; i < 240; i++) {
      const ent = (i % 2 ? "sz" : "sh") + "." + String(300000 + i).padStart(6, "0");
      rows.push({
        entity: ent,
        last_day: i % 17 === 0 ? "2026-09-28" : "2026-09-30",
      days_behind: i % 17 === 0 ? 2 : 0,
        records: 7000 + (i * 37) % 1600,
        coverage: i % 17 === 0 ? "incomplete" : "complete",
        delta_depth: (i * 13) % 250,
        head: `v1-${ent}-d${(i * 7) % 255}`,
      });
    }
    return rows;
  })(),
  gaps: [
    { entity: "sz.300750", start: "2026-09-25", end: "2026-09-29", reason: "member read timeout", state: "open",
      detail: { first_seen: "2026-09-25T09:02:11Z", attempts: 3, last_error: "member read timeout", covering_task: "task-7f3a2c91", hint: "缺口会在下一日更任务按 range 重试；若连续失败检查上游 .day member" } },
    { entity: "sh.600050", start: "2026-09-24", end: "2026-09-24", reason: "trading halt", state: "covered",
      detail: { first_seen: "2026-09-24T09:01:44Z", attempts: 1, covering_task: "task-4b1e88d0", hint: "停牌日，已按交易日历覆盖" } },
  ],
  storage: {
    db_bytes: 1405128704, wal_bytes: 0, rss_bytes: 561135616,
    bytes_per_bar: 48.2, segment_utilization: 0.88,
    segments: 63143, bars: 29136493, versions: 11783, entities: 11783,
    wal_peak_24h: 4202432,
    readers_active: 1, readers_max: 8, last_checkpoint_ms: 412, pending_frames: 0,
    write_cost: { no_change_bytes_per_item: 1721, append_bytes_per_item: 2426, sampled_items: 5221 },
    trend: [
      { day: "09-25", db_mb: 1380, wal_mb: 0 },
      { day: "09-26", db_mb: 1391, wal_mb: 4.1 },
      { day: "09-27", db_mb: 1391, wal_mb: 0 },
      { day: "09-28", db_mb: 1399, wal_mb: 3.8 },
      { day: "09-29", db_mb: 1399, wal_mb: 0 },
      { day: "09-30", db_mb: 1405, wal_mb: 4.3 },
      { day: "10-01", db_mb: 1405, wal_mb: 0 },
    ],
    delta_depth_hist: [
      { depth: "0", count: 11783 },
      { depth: "1-31", count: 8210 },
      { depth: "32-127", count: 3410 },
      { depth: "128-255", count: 163 },
      { depth: "≥256 rollover", count: 12 },
    ],
  },
  // 任务明细：item 列表（摘要级）
  task_items: {
    "task-7f3a2c91": Array.from({ length: 5221 }, (_, i) => {
      const sym = (i % 2 ? "sz" : "sh") + "." + String(300000 + i).padStart(6, "0");
      const failed = i === 1204 || i === 3887;
      return {
        item_id: `item-${sym}`, symbol: sym, stage: i < 4310 ? "finished" : (i === 4310 ? "running" : "pending"),
        success: failed ? false : (i < 4310 ? true : null),
        ms: i < 4310 ? 40 + (i * 17) % 60 : null,
      };
    }),
    "task-4b1e88d0": Array.from({ length: 11801 }, (_, i) => {
      const sym = (i % 2 ? "sz" : "sh") + "." + String(100000 + i).padStart(6, "0");
      const bad = [11, 402, 1204, 3887, 5001, 6200, 7311, 8420, 9001, 9100, 10100, 10200, 10300, 10400, 10500, 10600, 10700, 11800].includes(i);
      return { item_id: `item-${sym}`, symbol: sym, stage: "finished", success: !bad, ms: 9 + (i * 13) % 30 };
    }),
    "task-9c02aa17": [],
  },
  // 单版本详情：完整 digest + evidence JSON 预览
  version_detail_of: (version_id) => ({
    version_id,
    parent_version_id: version_id.includes("-d")
      ? version_id.replace(/-d(\d+)$/, (m, n) => +n > 1 ? `-d${n - 1}` : "")
      : null,
    child_version_id: version_id.includes("-d") && +version_id.match(/-d(\d+)$/)[1] < 206
      ? version_id.replace(/-d(\d+)$/, (m, n) => `-d${+n + 1}`)
      : null,
    state: "published", manifest_mode: version_id.includes("-d") ? "delta" : "checkpoint",
    projection_digest: "C12A20F1…8D3E", integrity_digest: "EEDE00B4…7A21",
    delta_digest: version_id.includes("-d") ? "9F33AC10…55B2" : null,
    segment: { pages: 182, util: 0.91, span_days: 250 },
    record_count: 7358 + (version_id.length * 7) % 300,
    first_day: "1994-12-06", last_day: "2026-09-30",
    published_at: "2026-09-30T09:03:02Z", task: "task-7f3a2c91",
    evidence_json: '{\n  "kind": "continuity",\n  "mode": "append",\n  "day_member_sha256": "eb66bb8b…",\n  "prefix_bytes": 245312\n}',
  }),
  // generation 详情：来源对账面
  generation_detail: {
    generation_id: "gen-2",
    manifest_sha256: "939971D0…AE8716",
    member_count: 3, bound_entities: 11783,
    members: [
      { path: "SHA/day/20260930.day", sha256: "1165802a…", kind: "day" },
      { path: "SHA/gbbq/security_event.json", sha256: "2af27e6a…", kind: "gbbq" },
      { path: "SZA/day/20260930.day", sha256: "44c19f02…", kind: "day" },
    ],
    audit: { outcome: "verified", at: "2026-09-30T08:59:44Z", by: "gen-audit task-9c02aa17" },
  },
  // item 详情：task item 的 outcome + 证据
  item_detail_of: (task_id, item_id) => {
    const items = FIX.task_items[task_id] || [];
    const it = items.find((x) => x.item_id === item_id) || { symbol: item_id, ms: 61, success: true };
    return {
      item_id, symbol: it.symbol, task_id, ms: it.ms ?? 0,
      outcome: it.success === false ? "failed" : it.success === true ? "succeeded" : it.stage ?? "pending",
      error: it.success === false ? "day member malformed: non-increasing trade_date at record 1822" : null,
      evidence: it.success === false
        ? { continuity: "中断于 stage", failed_phase: "diff", bytes_in: 245408, bars_written: 0 }
        : { continuity: "append", range: "2026-09-30", bytes_in: 245408, bars_written: 1 },
    };
  },
  // 失败明细：错误原文 + evidence + result 绑定
  failure_detail: (item_id) => ({
    item_id, class: item_id.endsWith("19") ? "integrity" : "source_violation",
    symbol: item_id.replace("item-", ""),
    error: item_id.includes("600519")
      ? "integrity: delta projection digest mismatch — staged ops projection C87F… does not match committed result"
      : "source_violation: .day member malformed: non-increasing date at record 1822",
    evidence: {
      result_id: `upd-${item_id}`,
      task_revision: 3,
      source_binding: "14AA1E62…89B8CA15",
      range: "2026-09-25 ~ 2026-09-30",
      continuity: "append",
    },
    hint: "source_violation 类失败为上游数据违规，重试不改结果；integrity 类需先查存储一致性。",
    task_id: "task-7f3a2c91",
    evidence_json: '{\n  "result_id": "upd-sh.600519-0930",\n  "binding": "sb-9f2e",\n  "prefix_bytes": 245312,\n  "integrity_digest": "7B21E09C…44F0"\n}', 
  }),
  // 实体详情：按 entity 派生（真实环境由 /api/entities/:id 提供）
  entity_detail: (entity) => {
    let seed = 0; for (const ch of entity) seed = (seed * 31 + ch.charCodeAt(0)) & 0xffff;
    const depth = seed % 250;
    const mk = (suffix, mode, d, day) => ({
      version_id: `v1-${entity}${suffix}`, mode, depth: d,
      records: 7000 + (seed % 1500) + d, state: "published",
      projection: ((seed * 2654435761) >>> 0).toString(16).toUpperCase().padStart(8, "0") + "…",
      published_at: `2026-09-${String(10 + (day % 20)).padStart(2, "0")}T09:0${d % 10}:00Z`,
    });
    const chain = [mk("", "checkpoint", 0, 1)];
    for (let d = Math.max(1, depth - 2); d <= depth; d++) chain.push(mk(`-d${d}`, "delta", d, d));
    chain.reverse();
    const closes = Array.from({ length: 60 }, (_, i) => 8 + Math.sin(i / 7 + seed % 9) * 1.6 + (i / 60) * 2 + ((seed * (i + 3)) % 11) / 14);
    const candles = closes.map((c, i) => {
      const o = i ? closes[i - 1] : c * 0.99;
      const hi = Math.max(o, c) * (1 + ((seed + i) % 7) / 300);
      const lw = Math.min(o, c) * (1 - ((seed + i * 2) % 5) / 300);
      return { o, h: hi, l: lw, c, v: 800 + ((seed * (i + 7)) % 900) };
    });
    const updates = Array.from({ length: 7 }, (_, i) => {
      const day = 30 - i;
      const mode = i === 3 ? "full" : (i % 4 === 0 ? "append" : "unchanged");
      return {
        day: `2026-09-${String(day).padStart(2, "0")}`,
        mode, result_id: `upd-${entity}-d${day}`,
        prefix_bytes: 245312 + i * 96,
        delta_digest: mode === "unchanged" ? null : `9F${((seed + i) % 0xffff).toString(16).toUpperCase().padStart(4, "0")}…`,
        task_revision: 3 + (i % 3),
      };
    });
    return {
      yearly: {
        bars: [ { year: 2023, close: 9.1 }, { year: 2024, close: 9.8 }, { year: 2025, close: 10.25 } ],
        base_daily: `v1-${entity}-d${depth}`, algo: "yearly-v1", as_of: "2026-09-30",
        cross_check: { yearly_last: 10.25, daily_agg_last: 10.25, consistent: true },
      },
      entity, period: "day", closes, candles,
      head: `v1-${entity}-d${depth}`,
      coverage: depth % 17 === 0 ? "incomplete" : "complete",
      first_day: "1994-12-06", last_day: "2026-09-30",
      periods: {
        day: { bars: 7358, first: "1994-12-06", last: "2026-09-30" },
        week: { bars: 1402, first: "1994-12-09", last: "2026-09-30" },
        month: { bars: 378, first: "1994-12", last: "2026-09" },
      },
      chain, updates,
      verifications: [
        { at: "2026-09-30T09:05:11Z", outcome: "verified", digest_checked: "C12A20F1…8D3E", scope: "head+chain" },
        { at: "2026-09-28T03:12:40Z", outcome: "verified", digest_checked: "C12A20F1…8D3E", scope: "full-history" },
        { at: "2026-09-20T11:48:02Z", outcome: "failed_closed", digest_checked: "7B21E09C…44F0", scope: "head", reason: "projection digest mismatch" },
      ],
      binding: {
        generation_id: "gen-2",
        manifest_sha256: "939971D0…AE8716",
        day_member_sha256: ((seed * 40503) >>> 0).toString(16).padStart(8, "0") + "…",
        security_event_sha256: ((seed * 99991) >>> 0).toString(16).padStart(8, "0") + "…",
      },
      // 惰性更早版本窗口：版本链只带尾部，"加载更早"时 API 分页取
      chain_older: (below_depth, n) => Array.from({ length: Math.min(n, below_depth) }, (_, i) => {
        const d = below_depth - 1 - i;
        return {
          version_id: `v1-${entity}-d${d}`, mode: "delta", depth: d,
          records: 7000 + (seed % 1500) + d, state: "published",
          projection: ((seed * 2654435761 * (d + 1)) >>> 0).toString(16).toUpperCase().padStart(8, "0") + "…",
          published_at: `2026-09-${String(1 + (d % 28)).padStart(2, "0")}T09:00:00Z`,
        };
      }),
    };
  },
  version_detail: {
    entity: "sh.600519", period: "day", head_version: "v1-sh.600519-d217",
    chain: [
      { version_id: "v1-sh.600519", mode: "checkpoint", depth: 0, records: 8420, state: "published", projection: "C12A20…", published_at: "2026-09-22T01:12:03Z" },
      { version_id: "v1-sh.600519-d215", mode: "delta", depth: 215, records: 8435, state: "published", projection: "8F3C41…", published_at: "2026-09-28T09:02:11Z" },
      { version_id: "v1-sh.600519-d216", mode: "delta", depth: 216, records: 8436, state: "published", projection: "77BE10…", published_at: "2026-09-29T09:01:48Z" },
      { version_id: "v1-sh.600519-d217", mode: "delta", depth: 217, records: 8437, state: "published", projection: "E90AA4…", published_at: "2026-09-30T09:03:02Z" },
    ],
    binding: {
      generation_id: "gen-2",
      manifest_sha256: "939971D0…AE8716",
      day_member_sha256: "EB66BB8B…0D3BA29",
      security_event_sha256: "AF5570F5…E0E83DFC",
    },
  },
};
