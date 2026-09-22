/* x-data 治理台 mock —— 渲染层 + push/pop 下钻栈。
   约定：页面函数返回 HTML 字符串，一次性 innerHTML；长列表窗口化；
   下钻走 history.pushState，Safari 返回手势天然可用。 */
"use strict";

const $ = (sel) => document.querySelector(sel);
const stage = $("#stage"), canvas = $("#canvas"), content = $("#content");
const PAGE_SIZE = 40;

/* ---------- 导航栈 ---------- */
let stack = [{ name: "tasks" }];
let shown = PAGE_SIZE;
const scrollMemo = {}; // 每层自己的滚动位置

const peek = () => stack[stack.length - 1];
const nav = (name, param) => {
  scrollMemo[keyOf(peek())] = canvas.scrollTop;
  stack.push({ name, param });
  history.pushState({ d: stack.length }, "");
  shown = PAGE_SIZE;
  render();
};
const back = () => { if (stack.length > 1) { stack.pop(); shown = PAGE_SIZE; render(); } };
window.addEventListener("popstate", back);
const keyOf = (e) => e.name + ":" + (e.param ?? "");

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
/* ---------- 列表页 ---------- */
function pageTasks() {
  return FIX.tasks.map((t, i) => {
    const pct = t.items.total ? (t.items.finished / t.items.total) * 100 : 0;
    return `<div class="card link" data-nav="task:${i}">
      <div style="display:flex;justify-content:space-between;align-items:center">
        <div><span class="mono">${esc(t.task_id)}</span> <span class="dim">${t.operation}</span></div>
        ${statusPill(t.status)}
      </div>
      <div class="bar ${t.items.failed ? "" : "ok"}"><i style="width:${pct}%"></i></div>
      <div class="kv"><span class="k">进度</span><span class="v nw">${t.items.finished}/${t.items.total} · 失败 ${t.items.failed} ›</span></div>
      <div class="kv"><span class="k">耗时</span><span class="v">${t.elapsed_s}s · ${t.per_item_ms ?? "—"}ms/item</span></div>
      <div class="kv"><span class="k">开始</span><span class="v">${ago(t.created_at)}</span></div>
    </div>`;
  }).join("");
}

function pageFailures() {
  if (!FIX.failures.length) return `<div class="card dim">当前无失败 item</div>`;
  const clsPill = { integrity: ["fail", "integrity"], source_violation: ["warn", "source"], storage: ["fail", "storage"], conflict: ["warn", "conflict"] };
  return FIX.failures.map((f, i) => {
    const [c, t] = clsPill[f.class] || ["dim", f.class];
    return `<div class="row link" data-nav="failure:${i}">
      <div class="main">
        <div class="title">${esc(f.symbol)}</div>
        <div class="sub mono">${esc(f.error)}</div>
        <div class="sub">${ago(f.at)} · ${esc(f.task_id)}</div>
      </div>
      <span class="pill ${c}">${t} ›</span>
    </div>`;
  }).join("");
}

function pageCoverage() {
  const rows = FIX.coverage.slice(0, shown).map((r) => `<div class="row link" data-nav="entity:${esc(r.entity)}">
      <div class="main">
        <div class="title mono">${esc(r.entity)}</div>
        <div class="sub">至 ${r.last_day} · ${r.records} 条 · depth ${r.delta_depth}</div>
      </div>
      <span class="pill ${r.coverage === "complete" ? "ok" : "warn"}">${r.coverage} ›</span>
    </div>`).join("");
  const gaps = FIX.gaps.map((g) => `<div class="row link" data-nav="entity:${esc(g.entity)}">
      <div class="main"><div class="title mono">${esc(g.entity)}</div>
      <div class="sub">${g.start} ~ ${g.end} · ${esc(g.reason)}</div></div>
      <span class="pill ${g.state === "open" ? "warn" : "ok"}">${g.state === "open" ? "缺口" : "已覆盖"} ›</span>
    </div>`).join("");
  const more = shown < FIX.coverage.length
    ? `<button class="load-more" id="load-more">显示更多（${shown}/${FIX.coverage.length}）</button>` : "";
  return `<div class="section-title">缺口</div>${gaps}
          <div class="section-title">实体覆盖（${FIX.coverage.length}）</div>${rows}${more}`;
}

function pageStorage() {
  const s = FIX.storage;
  const stat = (k, v) => `<div class="stat"><div class="v">${v}</div><div class="k">${k}</div></div>`;
  const trend = s.trend.map((t) => `<div class="kv"><span class="k">${t.day}</span><span class="v">${t.db_mb} MiB · wal ${t.wal_mb}MB</span></div>`).join("");
  const hist = s.delta_depth_hist.map((h) => `<div class="kv"><span class="k">${h.depth}</span><span class="v">${h.count}</span></div>`).join("");
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
      <div class="kv"><span class="k">版本数</span><span class="v">${s.versions}</span></div>
      <div class="kv"><span class="k">24h WAL 峰值</span><span class="v">${fmtBytes(s.wal_peak_24h)}</span></div>
    </div>
    <div class="section-title">DB 趋势（采样）</div>
    <div class="card">${trend}</div>
    <div class="section-title">delta 链深度分布</div>
    <div class="card">${hist}</div>`;
}

/* ---------- 下钻详情页 ---------- */
function pageTaskDetail(id) {
  const t = FIX.tasks.find((x) => x.task_id === id);
  const items = (FIX.task_items[id] || []);
  const failed = items.filter((i) => i.success === false);
  const list = items.slice(0, shown).map((i) => {
    const st = i.success === false ? ["fail", "失败"] : i.success === true ? ["ok", "ok"] : i.stage === "running" ? ["warn", "运行"] : ["dim", "等待"];
    return `<div class="row link" data-nav="entity:${esc(i.symbol)}">
      <div class="main"><div class="title mono">${esc(i.symbol)}</div>
      <div class="sub">${i.ms != null ? i.ms + "ms" : "—"}</div></div>
      <span class="pill ${st[0]}">${st[1]} ›</span></div>`;
  }).join("");
  const more = shown < items.length ? `<button class="load-more" id="load-more">显示更多（${shown}/${items.length}）</button>` : "";
  return `<div class="card">
      <div style="display:flex;justify-content:space-between"><span class="mono">${esc(t.task_id)}</span>${statusPill(t.status)}</div>
      <div class="kv"><span class="k">操作 / 数据集</span><span class="v">${t.operation} · ${t.dataset}</span></div>
      <div class="kv"><span class="k">进度</span><span class="v">${t.items.finished}/${t.items.total} · 失败 ${t.items.failed}</span></div>
      <div class="kv"><span class="k">耗时</span><span class="v">${t.elapsed_s}s · ${t.per_item_ms ?? "—"}ms/item</span></div>
    </div>
    ${failed.length ? `<div class="section-title">失败 item（${failed.length}）</div>` +
      failed.map((f) => `<div class="row link" data-nav="failure-item:${esc(f.item_id)}"><div class="main"><div class="title mono">${esc(f.symbol)}</div></div><span class="pill fail">失败 ›</span></div>`).join("") : ""}
    <div class="section-title">items（${items.length}）</div>${list}${more}`;
}

function pageFailureDetail(itemId) {
  const d = FIX.failure_detail(itemId);
  return `<div class="card">
      <h3>${esc(d.symbol)} · ${esc(d.item_id)}</h3>
      <div class="mono" style="color:var(--fail);word-break:break-all">${esc(d.error)}</div>
    </div>
    <div class="section-title">证据</div>
    <div class="card">
      <div class="kv"><span class="k">result_id</span><span class="v">${esc(d.evidence.result_id)}</span></div>
      <div class="kv"><span class="k">task_revision</span><span class="v">${d.evidence.task_revision}</span></div>
      <div class="kv"><span class="k">source_binding</span><span class="v">${esc(d.evidence.source_binding)}</span></div>
      <div class="kv"><span class="k">range</span><span class="v">${esc(d.evidence.range)}</span></div>
      <div class="kv"><span class="k">continuity</span><span class="v">${esc(d.evidence.continuity)}</span></div>
    </div>
    <div class="card dim">${esc(d.hint)}</div>`;
}

function pageEntityDetail(entity) {
  const d = FIX.entity_detail(entity);
  const chain = d.chain.map((c) => `<div class="row">
      <div class="main">
        <div class="title mono">${esc(c.version_id)}</div>
        <div class="sub">${c.mode} · depth ${c.depth} · ${c.records} 条 · ${ago(c.published_at)}</div>
        <div class="sub mono">proj ${esc(c.projection)}</div>
      </div>
      <span class="pill ok">${c.state}</span></div>`).join("");
  return `<div class="card">
      <h3>${esc(d.entity)} · ${d.period}</h3>
      <div class="kv"><span class="k">head</span><span class="v">${esc(d.head)}</span></div>
      <div class="kv"><span class="k">覆盖</span><span class="v">${d.first_day} ~ ${d.last_day}</span></div>
      <div class="kv"><span class="k">coverage</span><span class="v">${d.coverage}</span></div>
    </div>
    <div class="section-title">版本链（新→旧，尾部窗口）</div>${chain}
    <div class="section-title">来源绑定</div>
    <div class="card">
      <div class="kv"><span class="k">generation</span><span class="v">${esc(d.binding.generation_id)}</span></div>
      <div class="kv"><span class="k">manifest</span><span class="v">${esc(d.binding.manifest_sha256)}</span></div>
      <div class="kv"><span class="k">day member</span><span class="v">${esc(d.binding.day_member_sha256)}</span></div>
      <div class="kv"><span class="k">gbbq events</span><span class="v">${esc(d.binding.security_event_sha256)}</span></div>
    </div>`;
}

/* ---------- 路由 ---------- */
function route(e) {
  switch (e.name) {
    case "tasks": return ["任务巡检", pageTasks()];
    case "failures": return ["失败队列", pageFailures()];
    case "coverage": return ["覆盖看板", pageCoverage()];
    case "storage": return ["存储健康", pageStorage()];
    case "task": return ["任务明细", pageTaskDetail(FIX.tasks[e.param].task_id)];
    case "failure": return ["失败详情", pageFailureDetail(FIX.failures[e.param].item_id)];
    case "failure-item": return ["失败详情", pageFailureDetail(e.param)];
    case "entity": return ["实体", pageEntityDetail(e.param)];
    default: return ["?", ""];
  }
}

function render() {
  const e = peek();
  const [title, html] = route(e);
  $("#page-title").textContent = title;
  $("#menu-btn").textContent = stack.length > 1 ? "‹" : "≡";
  content.innerHTML = html;
  canvas.scrollTop = scrollMemo[keyOf(e)] ?? 0;
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
  else if (kind === "entity") nav("entity", param);
});
$("#menu-btn").addEventListener("click", () => {
  if (stack.length > 1) { history.back(); return; }
  stage.scrollTo({ left: 0, behavior: "smooth" });
});
function syncDrawer() {
  const open = stage.scrollLeft < 120;
  document.querySelectorAll(".nav-item").forEach((b) => (b.tabIndex = open ? 0 : -1));
  canvas.style.pointerEvents = open ? "none" : "auto";
}
stage.addEventListener("scroll", () => requestAnimationFrame(syncDrawer), { passive: true });
document.querySelectorAll(".nav-item").forEach((b) => {
  b.addEventListener("click", () => {
    stack = [{ name: b.dataset.page }]; history.pushState({ d: 1 }, "");
    document.querySelectorAll(".nav-item").forEach((x) => x.classList.toggle("active", x === b));
    shown = PAGE_SIZE; render();
    stage.scrollTo({ left: 240, behavior: "smooth" });
  });
});

/* ---------- 模拟轮询（detail 页不自动刷） ---------- */
let pollTick = 15;
setInterval(() => {
  pollTick -= 1;
  if (pollTick <= 0) { pollTick = 15; if (stack.length === 1) render(); }
  $("#poll-state").textContent = `刷新 ${pollTick}s`;
}, 1000);
$("#poll-state").addEventListener("click", () => { pollTick = 15; render(); });

/* ---------- 启动 ---------- */
history.replaceState({ d: 1 }, "");
stage.scrollLeft = 240;
syncDrawer();
render();
