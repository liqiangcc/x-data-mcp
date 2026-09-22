/* x-data 治理台 mock —— 单页总览 + push/pop 下钻。
   约定：页面函数返回 HTML，一次性 innerHTML；长列表窗口化；
   下钻走 history.pushState，系统返回手势天然可用；
   总览页只浮异常，正常项折叠为数字。 */
"use strict";

const $ = (sel) => document.querySelector(sel);
const canvas = $("#canvas"), content = $("#content");
const PAGE_SIZE = 40;

/* ---------- 导航栈 ---------- */
let stack = [{ name: "home" }];
let shown = PAGE_SIZE;
const scrollMemo = {};
const peek = () => stack[stack.length - 1];
const keyOf = (e) => e.name + ":" + (e.param ?? "");
const nav = (name, param) => {
  scrollMemo[keyOf(peek())] = canvas.scrollTop;
  navDir = 1;
  stack.push({ name, param });
  history.pushState({ d: stack.length }, "");
  shown = PAGE_SIZE;
  render();
};
let navDir = 1; // 1 前进 -1 后退
const back = () => { if (stack.length > 1) { navDir = -1; stack.pop(); shown = PAGE_SIZE; render(); } };
window.addEventListener("popstate", back);

/* ---------- 工具 ---------- */
const fmtBytes = (n) => n >= 1 << 30 ? (n / (1 << 30)).toFixed(2) + " GiB"
  : n >= 1 << 20 ? (n / (1 << 20)).toFixed(1) + " MiB"
  : n >= 1024 ? (n / 1024).toFixed(1) + " KiB" : n + " B";
const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
const ago = (iso) => {
  const s = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
  if (s < 60) return `${s | 0}s 前`;
  if (s < 3600) return `${s / 60 | 0}min 前`;
  if (s < 86400) return `${s / 3600 | 0}h 前`;
  return `${s / 86400 | 0}d 前`;
};
const statusPill = (st) => {
  const map = { running: ["warn", "运行中"], succeeded: ["ok", "完成"], pending: ["dim", "排队"], partially_failed: ["fail", "部分失败"], failed: ["fail", "失败"] };
  const [c, t] = map[st] || ["dim", st];
  return `<span class="pill ${c}">${t}</span>`;
};
const clsPill = { integrity: ["fail", "integrity"], source_violation: ["warn", "source"], storage: ["fail", "storage"], conflict: ["warn", "conflict"] };

/* ---------- 总览（异常浮上来，正常折叠） ---------- */
function pageHome() {
  const t = FIX.tasks[0]; // 最新任务
  const issueN = FIX.failures.length + FIX.gaps.filter((g) => g.state === "open").length;
  const today = FIX.tasks[0].created_at.slice(0, 10);
  const hero = issueN
    ? `<div class="hero warn link" data-nav="failures:"><span class="dot"></span><div><div class="msg">${issueN} 项需关注</div><div class="sub">失败 ${FIX.failures.length} · 开放缺口 ${FIX.gaps.filter((g) => g.state === "open").length} · 点击查看 ›</div></div><span class="dim" style="font-size:11px">${today}</span></div>`
    : `<div class="hero ok"><span class="dot"></span><div><div class="msg">全部正常</div><div class="sub">最近检查 ${ago(FIX.generated_at)}</div></div><span class="dim" style="font-size:11px">${today}</span></div>`;
  const pct = t.items.total ? (t.items.finished / t.items.total) * 100 : 0;
  const anomalies = FIX.coverage.filter((r) => r.coverage !== "complete" || r.delta_depth > 200);
  const s = FIX.storage;
  const deepChains = s.delta_depth_hist.filter((h) => h.depth !== "0").reduce((a, b) => a + b.count, 0);
  return `${hero}
    <div class="section-title">今日任务</div>
    <div class="card link" data-nav="task:0">
      <div style="display:flex;justify-content:space-between;align-items:center">
        <div><span class="mono">${esc(t.task_id)}</span> <span class="dim">${t.operation}</span></div>
        ${statusPill(t.status)}
      </div>
      <div class="bar ${t.items.failed ? "" : "ok"}"><i style="width:${pct}%"></i></div>
      ${(() => { const it = FIX.task_items[t.task_id] || []; const dn = it.filter((x) => x.ms != null); if (!dn.length) return "";
        const mx = Math.max(...dn.map((x) => x.ms)), step = Math.max(1, Math.floor(dn.length / 48));
        return `<div class="spark" style="height:18px;margin:6px 0 2px">${dn.filter((_, i) => i % step === 0).map((x) => `<i style="height:${8 + (x.ms / mx) * 92}%"></i>`).join("")}</div>`; })()}
      <div class="kv"><span class="k">进度</span><span class="v nw">${t.items.finished}/${t.items.total} · 失败 ${t.items.failed} ›</span></div>
      <div class="kv"><span class="k">耗时</span><span class="v" id="live-elapsed">${t.elapsed_s}s · ${t.per_item_ms ?? "—"}ms/item</span></div>
      ${t.status === "running" && t.per_item_ms ? `<div class="kv"><span class="k">预计剩余</span><span class="v">~${Math.round((t.items.total - t.items.finished) * t.per_item_ms / 1000)}s</span></div>` : ""}
    </div>

    <div class="section-title">失败队列</div>
    <div class="card link" data-nav="failures:">
      <div class="kv"><span class="k">总数</span><span class="v nw">${FIX.failures.length} 条 ›</span></div>
      ${FIX.failures.slice(0, 2).map((f) => `<div class="kv"><span class="k mono">${esc(f.symbol)}</span><span class="v">${esc(f.class)}</span></div>`).join("")}
    </div>

    <div class="section-title">覆盖</div>
    <div class="card link" data-nav="coverage:">
      <div class="kv"><span class="k">实体</span><span class="v">${FIX.coverage.length}</span></div>
      <div class="kv"><span class="k">异常</span><span class="v nw">${anomalies.length} 条（缺口 ${FIX.gaps.filter((g) => g.state === "open").length}）›</span></div>
      ${anomalies.slice(0, 3).map((r) => `<div class="kv"><span class="k mono">${esc(r.entity)}</span><span class="v">${r.coverage} · depth ${r.delta_depth}</span></div>`).join("")}
    </div>

    <div class="section-title">历史任务</div>
    ${FIX.tasks.slice(1).map((x, i) => `<div class="card link" data-nav="task:${i + 1}">
      <div style="display:flex;justify-content:space-between;align-items:center">
        <div><span class="mono">${esc(x.task_id)}</span> <span class="dim">${x.operation}</span></div>
        ${statusPill(x.status)}
      </div>
      <div class="kv"><span class="k">进度</span><span class="v nw">${x.items.finished}/${x.items.total} · 失败 ${x.items.failed} ›</span></div>
      <div class="kv"><span class="k">耗时</span><span class="v">${x.elapsed_s}s · ${ago(x.created_at)}</span></div>
    </div>`).join("")}

    <div class="section-title">来源</div>
    <div class="card link" data-nav="generation:">
      <div class="kv"><span class="k">generation</span><span class="v">gen-2</span></div>
      <div class="kv"><span class="k">manifest</span><span class="v"><span class="chip">939971D0…AE8716</span></span></div>
      <div class="kv"><span class="k">绑定实体</span><span class="v">${FIX.coverage.length} ›</span></div>
    </div>

    <div class="section-title">存储</div>
    <div class="card link" data-nav="storage:">
      <div class="kv"><span class="k">DB / WAL</span><span class="v nw">${fmtBytes(s.db_bytes)} / ${fmtBytes(s.wal_bytes)} ›</span></div>
      <div class="kv"><span class="k">字节/bar</span><span class="v">${s.bytes_per_bar} · seg 利用率 ${(s.segment_utilization * 100) | 0}%</span></div>
      <div class="kv"><span class="k">delta>0 实体</span><span class="v">${deepChains}</span></div>
      <div class="spark" style="height:20px;margin-top:6px">${s.trend.map((t) => `<i style="height:${(t.db_mb / Math.max(...s.trend.map((x) => x.db_mb))) * 92 + 8}%"></i>`).join("")}</div>
    </div>
    <div class="foot dim">数据 ${ago(FIX.generated_at)} · mock fixture<br>
      <button class="chipbtn" id="theme-toggle" style="margin-top:8px">切换主题</button></div>`;
}

/* ---------- 列表页 ---------- */
function pageFailures() {
  if (!FIX.failures.length) return `<div class="card dim">当前无失败 item</div>`;
  const byCls = {};
  FIX.failures.forEach((f) => { byCls[f.class] = (byCls[f.class] || 0) + 1; });
  const sum = `<div class="card"><div class="kv"><span class="k">分类统计</span><span class="v nw">${Object.entries(byCls).map(([k, n]) => `${k} ${n}`).join(" · ")}</span></div></div>`;
  const groups = {};
  FIX.failures.forEach((f, i) => { (groups[f.class] = groups[f.class] || []).push([f, i]); });
  const order = ["integrity", "storage", "conflict", "source_violation"];
  const grouped = Object.entries(groups)
    .sort((a, b) => order.indexOf(a[0]) - order.indexOf(b[0]))
    .map(([cls, items]) => {
      const [c, t] = clsPill[cls] || ["dim", cls];
      return `<div class="section-title"><span class="pill ${c}">${t} · ${items.length}</span></div>` +
        items.map(([f, i]) => `<div class="row link" data-nav="failure:${i}">
          <div class="main">
            <div class="title">${esc(f.symbol)}</div>
            <div class="sub mono">${esc(f.error)}</div>
            <div class="sub">${ago(f.at)} · ${esc(f.task_id)}</div>
          </div>
          <span class="pill dim">›</span>
        </div>`).join("");
    }).join("");
  return sum + grouped;
}

let covQuery = "";
let covFilter = "all";
let covMkt = "all";
let itemFilter = "all";
let itemSort = "seq";
function pageCoverage() {
  let pool = covQuery ? FIX.coverage.filter((r) => r.entity.includes(covQuery)) : FIX.coverage;
  if (covFilter === "warn") pool = pool.filter((r) => r.coverage !== "complete" || r.delta_depth > 200);
  if (covMkt !== "all") pool = pool.filter((r) => r.entity.startsWith(covMkt));
  const rows = pool.slice(0, shown).map((r) => `<div class="row link" data-nav="entity:${esc(r.entity)}">
      <div class="main">
        <div class="title mono">${esc(r.entity)}</div>
        <div class="sub">至 ${r.last_day}${r.days_behind ? ` · 滞后 ${r.days_behind}d` : ""} · ${r.records} 条 · depth ${r.delta_depth}</div>
      </div>
      <span class="pill ${r.coverage === "complete" ? "ok" : "warn"}">${r.coverage} ›</span>
    </div>`).join("");
  const gapPool = covMkt === "all" ? FIX.gaps : FIX.gaps.filter((g) => g.entity.startsWith(covMkt));
  const gaps = gapPool.map((g, gi) => `<div class="row link" data-nav="gap:${gi}">
      <div class="main"><div class="title mono">${esc(g.entity)}</div>
      <div class="sub">${g.start} ~ ${g.end} · ${esc(g.reason)}</div></div>
      <span class="pill ${g.state === "open" ? "warn" : "ok"}">${g.state === "open" ? "缺口" : "已覆盖"} ›</span>
    </div>`).join("");
  const more = shown < pool.length
    ? `<button class="load-more" id="load-more">显示更多（${shown}/${pool.length}）</button>` : "";
  const anomN = FIX.coverage.filter((r) => r.coverage !== "complete" || r.delta_depth > 200).length;
  return `<input id="cov-search" class="search" type="search" placeholder="搜索实体（如 sh.600）" value="${esc(covQuery)}">
          <div class="chips" style="margin-top:6px">
            ${[["all", "全部 " + FIX.coverage.length], ["warn", "异常 " + anomN]].map(([k, l]) => `<button class="chipbtn${covFilter === k ? " on" : ""}" data-covfilter="${k}">${l}</button>`).join("")}
            <span style="flex:1"></span>
            ${[["all", "全市场"], ["sh.", "沪"], ["sz.", "深"]].map(([k, l]) => `<button class="chipbtn${covMkt === k ? " on" : ""}" data-mkt="${k}">${l}</button>`).join("")}
          </div>
          <div class="section-title">缺口</div>${gaps || `<div class="card dim">无缺口</div>`}
          <div class="section-title">实体覆盖（${pool.length}）</div>${rows}${more || (pool.length ? "" : `<div class="card dim">无匹配</div>`)}`;
}

function pageStorage() {
  const s = FIX.storage;
  const stat = (k, v) => `<div class="stat"><div class="v">${v}</div><div class="k">${k}</div></div>`;
  const maxDb = Math.max(...s.trend.map((t) => t.db_mb));
  const trend = s.trend.map((t) => `<div class="hbar"><span class="lbl">${t.day}</span><span class="track"><i style="width:${(t.db_mb / maxDb) * 100}%"></i></span><span class="num">${t.db_mb}M</span></div>`).join("");
  const maxD = Math.max(...s.delta_depth_hist.map((h) => h.count));
  const hist = s.delta_depth_hist.map((h) => `<div class="hbar${h.depth.startsWith("≥") ? " warn" : ""}"><span class="lbl">${h.depth}</span><span class="track"><i style="width:${Math.max(1.5, (h.count / maxD) * 100)}%"></i></span><span class="num">${h.count}</span></div>`).join("");
  return `
    <div class="stat-grid">
      ${stat("数据库", fmtBytes(s.db_bytes))}
      ${stat("WAL", fmtBytes(s.wal_bytes))}
      ${stat("字节/bar", s.bytes_per_bar)}
      ${stat("RSS", fmtBytes(s.rss_bytes))}
    </div>
    <div class="section-title">规模</div>
    <div class="card">
      <div class="kv"><span class="k">实体 / bars</span><span class="v">${s.entities} / ${(s.bars / 1e6).toFixed(1)}M</span></div>
      <div class="kv"><span class="k">segments / 利用率</span><span class="v">${s.segments} / ${(s.segment_utilization * 100) | 0}%</span></div>
      <div class="bar${s.segment_utilization > 0.9 ? " warn" : " ok"}" style="margin:6px 0"><i style="width:${s.segment_utilization * 100}%"></i></div>
      <div class="kv"><span class="k">版本数</span><span class="v">${s.versions}</span></div>
      <div class="kv"><span class="k">24h WAL 峰值</span><span class="v">${fmtBytes(s.wal_peak_24h)}</span></div>
      <div class="kv"><span class="k">reader 占用</span><span class="v">${s.readers_active}/${s.readers_max} · 待帧 ${s.pending_frames}</span></div>
      <div class="kv"><span class="k">checkpoint</span><span class="v">${s.last_checkpoint_ms}ms · 无滞留</span></div>
    </div>
    <div class="section-title">写入成本（B/item · #259 门）</div>
    <div class="card">
      ${[["no-change", s.write_cost.no_change_bytes_per_item], ["append", s.write_cost.append_bytes_per_item]].map(([k, v]) => `<div class="hbar"><span class="lbl">${k}</span><span class="track"><i style="width:${(v / s.write_cost.append_bytes_per_item) * 100}%"></i></span><span class="num">${v}</span></div>`).join("")}
      <div class="kv"><span class="k">采样</span><span class="v">${s.write_cost.sampled_items} items · 最新任务</span></div>
    </div>
    <div class="section-title">DB 趋势（采样）</div>
    <div class="card">${trend}</div>
    <div class="section-title">delta 链深度分布</div>
    <div class="card">${hist}</div>`;
}

/* ---------- 下钻详情页 ---------- */
function pageTaskDetail(id) {
  const t = FIX.tasks.find((x) => x.task_id === id);
  const items = FIX.task_items[id] || [];
  const failed = items.filter((i) => i.success === false);
  let pool = itemFilter === "failed" ? failed
    : itemFilter === "running" ? items.filter((i) => i.stage === "running")
    : itemFilter === "pending" ? items.filter((i) => i.stage === "pending")
    : items;
  if (itemSort === "slow") pool = [...pool].sort((a, b) => (b.ms ?? 0) - (a.ms ?? 0));
  const list = pool.slice(0, shown).map((i) => {
    const st = i.success === false ? ["fail", "失败"] : i.success === true ? ["ok", "ok"] : i.stage === "running" ? ["warn", "运行"] : ["dim", "等待"];
    return `<div class="row link${i.stage === "running" ? " live" : ""}" data-nav="item:${esc(t.task_id)}|${esc(i.item_id)}">
      <div class="main"><div class="title mono">${esc(i.symbol)}</div>
      <div class="sub">${i.ms != null ? i.ms + "ms" : "—"}</div></div>
      <span class="pill ${st[0]}">${st[1]} ›</span></div>`;
  }).join("");
  const more = shown < pool.length ? `<button class="load-more" id="load-more">显示更多（${shown}/${pool.length}）</button>` : "";
  const done = items.filter((i) => i.ms != null);
  const spark = done.length ? (() => {
    const mx = Math.max(...done.map((i) => i.ms)), p95 = done.map((i) => i.ms).sort((a, b) => a - b)[Math.floor(done.length * .95)];
    const step = Math.max(1, Math.floor(done.length / 120));
    const bars = done.filter((_, i) => i % step === 0).map((i) => `<i class="${i.ms > p95 ? "hot" : ""}" style="height:${8 + (i.ms / mx) * 92}%"></i>`).join("");
    return `<div class="spark">${bars}</div><div class="sub dim" style="font-size:11px">item 耗时分布 · p95 ${p95}ms · 峰值 ${mx}ms</div>`;
  })() : "";
  const tpct = t.items.total ? (t.items.finished / t.items.total) * 100 : 0;
  return `<div class="card">
      <div style="display:flex;justify-content:space-between"><span class="mono">${esc(t.task_id)}</span>${statusPill(t.status)}</div>
      <div class="bar ${t.items.failed ? "" : "ok"}"><i style="width:${tpct}%"></i></div>
      ${spark}
      <div class="kv"><span class="k">操作 / 数据集</span><span class="v">${t.operation} · ${t.dataset}</span></div>
      <div class="kv"><span class="k">进度</span><span class="v">${t.items.finished}/${t.items.total} · 失败 ${t.items.failed}</span></div>
      <div class="kv"><span class="k">耗时</span><span class="v"${t.status === "running" ? ` id="live-elapsed"` : ""}>${t.elapsed_s}s · ${t.per_item_ms ?? "—"}ms/item</span></div>
    </div>
    ${t.phases ? `<div class="section-title">相位耗时（ms/item）</div>
    <div class="card">${Object.entries(t.phases).map(([k, v]) => {
      const mx = Math.max(...Object.values(t.phases));
      return `<div class="hbar"><span class="lbl">${k.replace("_ms", "")}</span><span class="track"><i style="width:${(v / mx) * 100}%"></i></span><span class="num">${v}</span></div>`;
    }).join("")}</div>` : ""}
    ${failed.length ? `<div class="section-title">失败 item（${failed.length}）</div>` +
      failed.map((f) => `<div class="row link" data-nav="failure-item:${esc(f.item_id)}"><div class="main"><div class="title mono">${esc(f.symbol)}</div></div><span class="pill fail">失败 ›</span></div>`).join("") : ""}
    <div class="section-title">items（${pool.length}）</div>
    <div class="chips">
      ${[["all", "全部 " + items.length], ["failed", "失败 " + failed.length], ["running", "运行中"], ["pending", "等待"]]
        .map(([k, l]) => `<button class="chipbtn${itemFilter === k ? " on" : ""}" data-filter="${k}">${l}</button>`).join("")}
      <span style="flex:1"></span>
      <button class="chipbtn${itemSort === "slow" ? " on" : ""}" data-sort="slow">最慢</button>
    </div>
    ${list}${more}`;
}

function pageFailureDetail(itemId) {
  const d = FIX.failure_detail(itemId);
  const clsHint = {
    integrity: "integrity 失败：先查存储一致性（verify_versions）——可能是 delta 链损坏，不要盲目重试。",
    source_violation: "source_violation：上游数据违规，重试不改变结果。检查 .day member 或等待下一交易日。",
    storage: "storage 失败：检查 WAL 滞留/disk 空间/reader 占用，然后重试 item。",
    conflict: "conflict：写入协调器检测到并发冲突，通常 retry 即可自愈。",
  };
  return `<div class="card">
      <h3>${esc(d.symbol)} · ${esc(d.item_id)}</h3>
      <div class="errblk">${esc(d.error)}</div>
    </div>
    <div class="section-title">证据</div>
    <div class="card">
      <div class="kv"><span class="k">result_id</span><span class="v">${esc(d.evidence.result_id)}</span></div>
      <div class="kv"><span class="k">task_revision</span><span class="v">${d.evidence.task_revision}</span></div>
      <div class="kv"><span class="k">source_binding</span><span class="v"><span class="chip">${esc(d.evidence.source_binding)}</span></span></div>
      <div class="kv"><span class="k">range</span><span class="v">${esc(d.evidence.range)}</span></div>
      <div class="kv"><span class="k">continuity</span><span class="v">${esc(d.evidence.continuity)}</span></div>
    </div>
    <div class="section-title">raw evidence</div>
    <div class="card"><pre class="jsonblk">${esc(d.evidence_json)}</pre></div>
    <div class="card dim">${esc(clsHint[d.class] || d.hint)}</div>
    <div class="card link" data-nav="entity:${esc(d.symbol)}"><div class="kv"><span class="k">查看实体</span><span class="v nw">${esc(d.symbol)} ›</span></div></div>
    <div class="card link" data-nav="task:0"><div class="kv"><span class="k">所属任务</span><span class="v nw">${esc(d.task_id)} ›</span></div></div>`;
}

function pageEntityDetail(entity) {
  const d = FIX.entity_detail(entity);
  const chain = d.chain.map((c) => `<div class="row link${c.mode === "checkpoint" ? " cp" : ""}" data-nav="version:${esc(c.version_id)}">
      <div class="main">
        <div class="title mono">${esc(c.version_id)}</div>
        <div class="sub">${c.mode} · depth ${c.depth} · ${c.records} 条 · ${ago(c.published_at)}</div>
        <div class="sub"><span class="chip">proj ${esc(c.projection)}</span></div>
      </div>
      <span class="pill ok">${c.state} ›</span></div>`).join("");
  return `<div class="card">
      <h3>${esc(d.entity)} · ${d.period}</h3>
      <div class="kv"><span class="k">head</span><span class="v">${esc(d.head)}</span></div>
      <div class="kv"><span class="k">覆盖</span><span class="v">${d.first_day} ~ ${d.last_day}</span></div>
      <div class="kv"><span class="k">coverage</span><span class="v">${d.coverage}</span></div>
      <div class="chips" style="margin-top:8px">${Object.entries(d.periods).map(([p, x]) => `<button class="chipbtn" title="${x.first} ~ ${x.last}">${p} · ${x.bars}</button>`).join("")}</div>
    </div>
    ${(FIX._lastCandles = d.candles, "")}
    <div class="section-title">近 60 日 K 线</div>
    <div class="card"><div class="kchart">${d.candles.map((c) => {
      const lo = Math.min(...d.candles.map((x) => x.l)), hi = Math.max(...d.candles.map((x) => x.h));
      const y = (v) => ((v - lo) / (hi - lo || 1)) * 100;
      const up = c.c >= c.o;
      const bTop = 100 - y(Math.max(c.o, c.c)), bH = Math.max(2, y(Math.max(c.o, c.c)) - y(Math.min(c.o, c.c)));
      return `<i class="k${up ? " up" : " dn"}" style="--wt:${100 - y(c.h)}%;--wb:${y(c.h) - y(c.l)}%;--bt:${bTop}%;--bh:${bH}%"></i>`;
    }).join("")}</div>
    <div class="sub dim" style="font-size:11px;margin-top:6px">${d.closes[0].toFixed(2)} → ${d.closes[d.closes.length - 1].toFixed(2)} · 60 日（mock）</div></div>
    <div class="section-title">年线（period=2）</div>
    <div class="card">
      <div class="spark">${d.yearly.bars.map((b) => `<i style="height:${(b.close / Math.max(...d.yearly.bars.map((x) => x.close))) * 92 + 8}%"></i>`).join("")}</div>
      <div class="kv"><span class="k">基座 / 算法</span><span class="v">${esc(d.yearly.base_daily.slice(0, 22))}… · ${d.yearly.algo}</span></div>
      <div class="kv"><span class="k">as_of</span><span class="v">${d.yearly.as_of}</span></div>
      <div class="kv"><span class="k">日线对照</span><span class="v">${d.yearly.cross_check.consistent
        ? `<span class="pill ok">一致</span> 年末收盘 ${d.yearly.cross_check.yearly_last} vs 日线聚合 ${d.yearly.cross_check.daily_agg_last}`
        : `<span class="pill fail">不一致</span>`}</span></div>
    </div>
    <div class="section-title">更新审计（近 7 日）</div>
    <div class="card">${d.updates.map((u) => {
      const mc = { unchanged: ["dim", "unchanged"], append: ["ok", "append"], full: ["warn", "full"] }[u.mode];
      return `<div class="kv"><span class="k mono">${u.day}</span>
        <span class="v"><span class="pill ${mc[0]}">${mc[1]}</span> ${u.delta_digest ? `<span class="chip">Δ ${esc(u.delta_digest)}</span>` : `<span class="dim">复用基座</span>`}</span></div>`;
    }).join("")}</div>
    <div class="section-title">版本链（新→旧，尾部窗口）</div><div class="timeline">${chain}</div>
    <div class="section-title">验证结果</div>
    <div class="card">${d.verifications.map((v) => {
      const [c, t] = v.outcome === "verified" ? ["ok", "通过"] : ["fail", "fail-closed"];
      return `<div class="kv"><span class="k">${ago(v.at)} · ${v.scope}</span>
        <span class="v"><span class="pill ${c}">${t}</span> <span class="chip">${esc(v.digest_checked)}</span></span></div>
        ${v.reason ? `<div class="kv"><span class="k"></span><span class="v" style="color:var(--fail)">${esc(v.reason)}</span></div>` : ""}`;
    }).join("")}</div>
    <div class="section-title">来源绑定</div>
    <div class="card">
      <div class="kv"><span class="k">generation</span><span class="v">${esc(d.binding.generation_id)}</span></div>
      <div class="kv"><span class="k">manifest</span><span class="v"><span class="chip">${esc(d.binding.manifest_sha256)}</span></span></div>
      <div class="kv"><span class="k">day member</span><span class="v"><span class="chip">${esc(d.binding.day_member_sha256)}</span></span></div>
      <div class="kv"><span class="k">gbbq events</span><span class="v"><span class="chip">${esc(d.binding.security_event_sha256)}</span></span></div>
    </div>`;
}

function pageGeneration() {
  const g = FIX.generation_detail;
  return `<div class="card">
      <div style="display:flex;justify-content:space-between;align-items:center">
        <h3>${esc(g.generation_id)}</h3>
        <span class="pill ${g.audit.outcome === "verified" ? "ok" : "fail"}">${g.audit.outcome === "verified" ? "审计通过" : "审计失败"}</span>
      </div>
      <div class="kv"><span class="k">manifest</span><span class="v"><span class="chip">${esc(g.manifest_sha256)}</span></span></div>
      <div class="kv"><span class="k">member / 绑定实体</span><span class="v">${g.member_count} / ${g.bound_entities}</span></div>
      <div class="kv"><span class="k">审计</span><span class="v">${ago(g.audit.at)} · ${esc(g.audit.by)}</span></div>
    </div>
    <div class="section-title">manifest members（${g.member_count}）</div>
    <div class="card">${g.members.map((m) => `<div class="kv"><span class="k mono" style="font-size:11px">${esc(m.path)}</span><span class="v"><span class="chip">${esc(m.sha256)}</span></span></div>`).join("")}</div>
    <div class="card dim">字节完整性链：manifest_sha → member_sha → member 字节 → 解析记录 → segment。逐项可复核。</div>`;
}

function pageAbout() {
  return `<div class="card">
      <h3>x-data 治理台（mock）</h3>
      <div class="kv"><span class="k">形态</span><span class="v">只读观测 · MCP 为唯一操作通道</span></div>
      <div class="kv"><span class="k">数据源</span><span class="v">SQLite 唯一事实源（fixture）</span></div>
      <div class="kv"><span class="k">Issue</span><span class="v"><a href="https://github.com/liqiangcc/x-data-mcp/issues/268" style="color:var(--accent)">#268</a> · 阻塞于 #259 硬门 + S6</span></div>
      <div class="kv"><span class="k">渲染</span><span class="v">vanilla JS · 零依赖 · ${innerWidth}×${innerHeight}</span></div>
      <div class="kv"><span class="k">主题</span><span class="v">${matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light"}（系统）</span></div>
    </div>
    <div class="card dim">异常浮上来，正常的折叠 —— 巡检第一性问题：今天有没有异常。</div>`;
}

function pageItemDetail(tid, iid) {
  const d = FIX.item_detail_of(tid, iid);
  const oc = { failed: "fail", succeeded: "ok", running: "warn", pending: "dim" }[d.outcome] || "dim";
  return `<div class="card">
      <div style="display:flex;justify-content:space-between;align-items:center">
        <h3 class="mono">${esc(d.symbol)}</h3>
        <span class="pill ${oc}">${d.outcome}</span>
      </div>
      <div class="kv"><span class="k">item / task</span><span class="v">${esc(d.item_id)}</span></div>
      <div class="kv"><span class="k">耗时</span><span class="v">${d.ms}ms</span></div>
      ${d.error ? `<div class="errblk">${esc(d.error)}</div>` : ""}
    </div>
    <div class="section-title">写入证据</div>
    <div class="card">
      <div class="kv"><span class="k">continuity</span><span class="v">${esc(d.evidence.continuity)}</span></div>
      <div class="kv"><span class="k">range</span><span class="v">${esc(d.evidence.range)}</span></div>
      <div class="kv"><span class="k">bytes_in / bars</span><span class="v">${d.evidence.bytes_in} / ${d.evidence.bars_written}</span></div>
    </div>
    <div class="card link" data-nav="entity:${esc(d.symbol)}"><div class="kv"><span class="k">查看实体</span><span class="v nw">${esc(d.symbol)} ›</span></div></div>`;
}

function pageVersionDetail(vid) {
  const v = FIX.version_detail_of(vid);
  const dig = (k, val) => val ? `<div class="kv"><span class="k">${k}</span><span class="v"><span class="chip">${esc(val)}</span></span></div>` : "";
  return `<div class="card">
      <div style="display:flex;justify-content:space-between;align-items:center">
        <h3 class="mono">${esc(v.version_id)}</h3>
        <span class="pill ok">${v.state}</span>
      </div>
      <div class="kv"><span class="k">manifest</span><span class="v">${v.manifest_mode} · ${v.record_count} 条</span></div>
      <div class="kv"><span class="k">覆盖</span><span class="v">${v.first_day} ~ ${v.last_day}</span></div>
      <div class="kv"><span class="k">发布</span><span class="v">${ago(v.published_at)} · ${esc(v.task)}</span></div>
      <div class="kv"><span class="k">最近验证</span><span class="v"><span class="pill ok">通过</span> head+chain</span></div>
    </div>
    ${v.parent_version_id ? `<div class="card link" data-nav="version:${esc(v.parent_version_id)}"><div class="kv"><span class="k">父版本</span><span class="v nw">${esc(v.parent_version_id)} ›</span></div></div>` : ""}
    ${v.child_version_id ? `<div class="card link" data-nav="version:${esc(v.child_version_id)}"><div class="kv"><span class="k">子版本</span><span class="v nw">${esc(v.child_version_id)} ›</span></div></div>` : ""}
    <div class="section-title">Digests</div>
    <div class="card">
      ${dig("projection", v.projection_digest)}
      ${dig("integrity", v.integrity_digest)}
      ${dig("delta", v.delta_digest)}
    </div>
    <div class="section-title">存储段</div>
    <div class="card">
      <div class="kv"><span class="k">pages / 利用率</span><span class="v">${v.segment.pages} / ${(v.segment.util * 100) | 0}%</span></div>
      <div class="kv"><span class="k">覆盖天数</span><span class="v">${v.segment.span_days}</span></div>
    </div>
    <div class="section-title">证据</div>
    <div class="card"><pre class="jsonblk">${esc(v.evidence_json)}</pre></div>`;
}

function pageGapDetail(gi) {
  const g = FIX.gaps[gi];
  const d = g.detail;
  return `<div class="card">
      <div style="display:flex;justify-content:space-between;align-items:center">
        <h3 class="mono">${esc(g.entity)}</h3>
        <span class="pill ${g.state === "open" ? "warn" : "ok"}">${g.state === "open" ? "开放缺口" : "已覆盖"}</span>
      </div>
      <div class="kv"><span class="k">区间</span><span class="v">${g.start} ~ ${g.end}</span></div>
      <div class="kv"><span class="k">原因</span><span class="v">${esc(g.reason)}</span></div>
    </div>
    <div class="section-title">详情</div>
    <div class="card">
      <div class="kv"><span class="k">首次发现</span><span class="v">${ago(d.first_seen)}</span></div>
      <div class="kv"><span class="k">重试次数</span><span class="v">${d.attempts}</span></div>
      ${d.last_error ? `<div class="kv"><span class="k">最近错误</span><span class="v">${esc(d.last_error)}</span></div>` : ""}
      <div class="kv"><span class="k">覆盖任务</span><span class="v">${esc(d.covering_task)}</span></div>
    </div>
    <div class="card dim">${esc(d.hint)}</div>
    <div class="card link" data-nav="entity:${esc(g.entity)}"><div class="kv"><span class="k">查看实体</span><span class="v nw">${esc(g.entity)} ›</span></div></div>`;
}

/* ---------- 路由 ---------- */
function route(e) {
  switch (e.name) {
    case "home": return ["总览", pageHome()];
    case "failures": return ["失败队列", pageFailures()];
    case "coverage": return ["覆盖", pageCoverage()];
    case "storage": return ["存储健康", pageStorage()];
    case "task": return ["任务明细", pageTaskDetail(FIX.tasks[e.param].task_id)];
    case "failure": return ["失败详情", pageFailureDetail(FIX.failures[e.param].item_id)];
    case "failure-item": return ["失败详情", pageFailureDetail(e.param)];
    case "gap": return ["缺口", pageGapDetail(+e.param)];
    case "version": return ["版本", pageVersionDetail(e.param)];
    case "item": { const [tid, iid] = e.param.split("|"); return ["item 详情", pageItemDetail(tid, iid)]; }
    case "generation": return ["来源对账", pageGeneration()];
    case "about": return ["关于", pageAbout()];
    case "entity": return ["实体", pageEntityDetail(e.param)];
    default: return ["?", ""];
  }
}

function render() {
  const e = peek();
  const [title, html] = route(e);
  $("#page-title").textContent = title;
  const parent = stack.length > 1 ? route(stack[stack.length - 2])[0] : "";
  $("#menu-btn").textContent = stack.length > 1 ? `‹ ${parent}` : "ⓘ";
  $("#menu-btn").style.width = stack.length > 1 ? "auto" : "44px";
  content.innerHTML = html;
  content.classList.toggle("rev", navDir < 0); navDir = 1;
  canvas.scrollTop = scrollMemo[keyOf(e)] ?? 0;
  const toTop = $("#to-top") || (() => {
    const b = document.createElement("button");
    b.id = "to-top"; b.textContent = "↑"; b.setAttribute("aria-label", "回顶");
    b.onclick = () => canvas.scrollTo({ top: 0, behavior: "smooth" });
    document.body.appendChild(b); return b;
  })();
  const lm = $("#load-more");
  if (lm) {
    const grow = () => { scrollMemo[keyOf(e)] = canvas.scrollTop; shown += PAGE_SIZE; render(); };
    lm.onclick = grow;
    new IntersectionObserver((ev, o) => { if (ev[0].isIntersecting) { o.disconnect(); grow(); } }).observe(lm);
  }
}

/* ---------- 事件 ---------- */
content.addEventListener("click", (ev) => {
  const el = ev.target.closest("[data-nav]");
  if (!el) return;
  const [kind, param] = el.dataset.nav.split(/:(.*)/);
  if (kind === "task") nav("task", +param);
  else if (kind === "failure") nav("failure", +param);
  else if (kind === "failure-item") nav("failure-item", param);
  else if (kind === "gap") nav("gap", param);
  else if (kind === "version") nav("version", param);
  else if (kind === "item") nav("item", param);
  else if (kind === "entity") nav("entity", param);
  else nav(kind);
});
content.addEventListener("input", (ev) => {
  if (ev.target.id === "cov-search") { covQuery = ev.target.value; shown = PAGE_SIZE;
    const pos = canvas.scrollTop; render(); canvas.scrollTop = pos;
    const el = $("#cov-search"); el.focus(); el.setSelectionRange(el.value.length, el.value.length); }
});
content.addEventListener("click", (ev) => {
  const chip = ev.target.closest(".chip");
  if (chip && navigator.clipboard) {
    navigator.clipboard.writeText(chip.textContent.trim()).then(() => {
      chip.classList.add("copied"); setTimeout(() => chip.classList.remove("copied"), 900);
    });
  }
  const f = ev.target.closest("[data-filter]");
  if (f) { itemFilter = f.dataset.filter; shown = PAGE_SIZE; render(); }
  const cf = ev.target.closest("[data-covfilter]");
  if (cf) { covFilter = cf.dataset.covfilter; shown = PAGE_SIZE; render(); }
  const so = ev.target.closest("[data-sort]");
  if (so) { itemSort = itemSort === "slow" ? "seq" : "slow"; shown = PAGE_SIZE; render(); }
  const mk = ev.target.closest("[data-mkt]");
  if (mk) { covMkt = mk.dataset.mkt; shown = PAGE_SIZE; render(); }
});
$("#menu-btn").addEventListener("click", () => { stack.length > 1 ? history.back() : nav("about"); });
content.addEventListener("click", (ev) => {
  if (ev.target.id === "theme-toggle") {
    const cur = document.documentElement.dataset.theme;
    document.documentElement.dataset.theme = cur === "light" ? "dark" : cur === "dark" ? "" : "light";
    // 显式 dark 覆盖 prefers light；空值回系统
  }
});
$("#poll-state").addEventListener("click", () => { pollTick = 15; render(); });

/* ---------- 模拟轮询（详情页不自动刷） ---------- */
let pollTick = 15;
setInterval(() => {
  if (document.hidden) return;
  pollTick -= 1;
  const live = $("#live-elapsed");
  if (live) { const m = live.textContent.match(/^(\d+)s/); if (m) live.textContent = live.textContent.replace(/^\d+s/, `${+m[1] + 1}s`); }
  if (pollTick <= 0) { pollTick = 15; if (stack.length === 1) render(); }
  $("#poll-state").textContent = `刷新 ${pollTick}s`;
}, 1000);

// 离线横幅
const offline = () => $("#offline").classList.toggle("on", !navigator.onLine);
window.addEventListener("offline", offline); window.addEventListener("online", offline); offline();

/* ---------- K 线触摸 tooltip ---------- */
content.addEventListener("touchmove", (ev) => {
  const kc = ev.target.closest(".kchart"); if (!kc) return;
  const ks = kc.querySelectorAll(".k"); if (!ks.length) return;
  const rect = kc.getBoundingClientRect();
  const idx = Math.min(ks.length - 1, Math.max(0, Math.floor((ev.touches[0].clientX - rect.left) / rect.width * ks.length)));
  ks.forEach((k, i) => k.classList.toggle("sel", i === idx));
  const d = FIX._lastCandles && FIX._lastCandles[idx];
  let tip = $("#ktip");
  if (!tip) { tip = document.createElement("div"); tip.id = "ktip"; kc.appendChild(tip); }
  if (d) {
    const day = new Date(Date.now() - (ks.length - 1 - idx) * 864e5).toISOString().slice(5, 10);
    tip.textContent = `${day}  O ${d.o.toFixed(2)}  H ${d.h.toFixed(2)}  L ${d.l.toFixed(2)}  C ${d.c.toFixed(2)}`;
  }
}, { passive: true });
content.addEventListener("touchend", () => {
  const tip = $("#ktip"); if (tip) tip.remove();
  document.querySelectorAll(".k.sel").forEach((k) => k.classList.remove("sel"));
}, { passive: true });

/* ---------- 回顶悬浮钮 ---------- */
canvas.addEventListener("scroll", () => {
  const b = $("#to-top"); if (b) b.classList.toggle("on", canvas.scrollTop > 700);
}, { passive: true });

/* ---------- 下拉刷新（触顶下拉显示指示器） ---------- */
let pullY = null;
canvas.addEventListener("touchstart", (e) => { if (canvas.scrollTop <= 0) pullY = e.touches[0].clientY; }, { passive: true });
canvas.addEventListener("touchmove", (e) => {
  if (pullY == null) return;
  const dy = e.touches[0].clientY - pullY;
  const el = $("#pullhint");
  if (dy > 60) { el.classList.add("on"); el.textContent = "松开刷新"; }
}, { passive: true });
canvas.addEventListener("touchend", () => {
  const el = $("#pullhint");
  if (el.classList.contains("on")) { el.textContent = "刷新中…"; setTimeout(() => { render(); el.classList.remove("on"); el.textContent = ""; }, 400); }
  pullY = null;
}, { passive: true });

/* ---------- 启动 ---------- */
history.replaceState({ d: 1 }, "");
content.innerHTML = `<div class="skel card"></div><div class="skel card"></div><div class="skel card" style="height:120px"></div>`;
setTimeout(render, 320); // mock 模拟首载延迟；真实环境为首个 fetch
