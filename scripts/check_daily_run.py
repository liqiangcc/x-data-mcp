#!/usr/bin/env python3
"""Read-only audit of one daily sync against the x-data-daily-sync Skill.

Background (#304): the 2026-11-05 daily sync skipped Skill steps and the agent
still reported `completed`. This script answers, from durable facts only,
whether a run walked through every required step of
`.devin/skills/x-data-daily-sync/SKILL.md` (§2 and §3, plus §4 with --session).

Inputs
  --db <preview.sqlite>    opened as `file:...?mode=ro` (+ `PRAGMA query_only`);
                           the script never writes to it
  --generations <dir>      generation root: `active-generation.json`,
                           `acquire.sqlite` and `generations/<id>/` (read-only)
  --since <UTC time>       start of the run window (ISO 8601; no zone = UTC)
  --session <file.jsonl>   optional pi session of the daily-sync agent

Output: one JSON document on stdout, a text summary on stderr. Exit status:
0 every required step executed or checked with nothing to do; 1 at least one
step not executed (or `completed` claimed over missing steps); 2 unusable input.

Each step gets one of three statuses:
  executed           the DB (or session) shows the step was carried out
  checked_no_action  durable facts prove the step's trigger was absent
  not_executed       neither; also used for a task that did not reach a
                     successful terminal state

"checked_no_action" is never granted for lack of evidence. Rules per step are
in the `rule` field of the JSON and in the docstrings of the `step_*`
functions. Plans (`plan_market_update`) leave no DB row, so "plan found
nothing to do" is inferred from the resulting DB state, or read from the
session when one is given.
"""
from __future__ import annotations

import argparse
import json
import re
import sqlite3
import sys
from collections import Counter
from dataclasses import dataclass, field
from datetime import date, datetime, timezone
from pathlib import Path
from typing import Any

EXECUTED = "executed"
NO_ACTION = "checked_no_action"
MISSING = "not_executed"

TERMINAL_OK = {"succeeded", "partially_failed"}
TERMINAL_ALL = {"succeeded", "partially_failed", "failed", "cancelled", "interrupted"}

PLAN_RUN_CV = "market-plan-run-task-v1"
EXPLICIT_DAILY_CV = "explicit-range-task-v1"
EXPLICIT_YEARLY_CV = "explicit-yearly-task-v1"

G1_GATE_SECONDS = 600  # SLO row 2 / D-63
YEARLY_GATE_SECONDS = 600  # SLO row 3 / D-69
MAX_HOT_GENERATIONS = 6  # src/acquire/service.rs MAX_HOT_GENERATIONS
SAMPLE = 10  # entities listed per finding

REQUIRED_TABLES = (
    "tasks",
    "task_items",
    "source_refresh_runs",
    "market_series",
    "market_events",
    "yearly_series_lineage",
    "security_master",
    "proven_gaps",
    "catalog_head",
    "catalog_versions",
    "catalog_members",
)
M2_DATASETS = (
    "market.security_master",
    "market.risk_warnings",
    "market.suspensions",
    "market.price_limit_rules",
)
WRITE_TOOLS = {"run_market_update", "update_data", "start_collection"}


class InputError(Exception):
    """The inputs cannot be audited (exit status 2)."""


# --------------------------------------------------------------------------
# small helpers
# --------------------------------------------------------------------------


def parse_ts(value: Any) -> datetime | None:
    """ISO 8601 -> aware UTC datetime. Rust writes nanosecond fractions."""
    if not isinstance(value, str) or not value:
        return None
    text = re.sub(r"(\.\d{6})\d+", r"\1", value.strip())
    if text.endswith(("Z", "z")):
        text = text[:-1] + "+00:00"
    try:
        parsed = datetime.fromisoformat(text)
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def iso(value: datetime | None) -> str | None:
    return value.isoformat().replace("+00:00", "Z") if value else None


def epoch_day(text: str | None) -> int | None:
    try:
        return (date.fromisoformat(text or "") - date(1970, 1, 1)).days
    except ValueError:
        return None


def day_text(epoch: int | None) -> str | None:
    if epoch is None:
        return None
    return date.fromordinal(date(1970, 1, 1).toordinal() + epoch).isoformat()


def reason_code(error: str | None) -> str:
    """Mirror of `reason_code` in src/data_supply/task_v2.rs (contract v7 §5)."""
    text = error or ""
    if "RevisionUnverified" in text:
        return "REVISION_UNVERIFIED"
    if "PUBLISH_PRECONDITION" in text:
        return "PUBLISH_PRECONDITION"
    if "no bars inside the accepted range" in text:
        return "NO_BARS_IN_RANGE"
    return "OTHER"


def sample(items) -> list:
    return sorted(items)[:SAMPLE]


def open_readonly(path: Path) -> sqlite3.Connection:
    if not path.is_file():
        raise InputError(f"not a file: {path}")
    conn = sqlite3.connect(f"{path.resolve().as_uri()}?mode=ro", uri=True)
    conn.execute("PRAGMA query_only=ON")
    return conn


# --------------------------------------------------------------------------
# facts gathered from the databases
# --------------------------------------------------------------------------


@dataclass
class Task:
    task_id: str
    operation: str
    dataset: str
    contract_version: str
    status: str
    created_at: datetime
    last_progress_at: datetime
    item_count: int
    items_finished: int
    items_succeeded: int
    kind: str = "other"  # a_share | etf | yearly_run | explicit_daily | explicit_yearly | collection | other
    evidence: dict = field(default_factory=dict)

    @property
    def terminal_ok(self) -> bool:
        return self.status in TERMINAL_OK

    @property
    def write_loop_seconds(self) -> float:
        return round((self.last_progress_at - self.created_at).total_seconds(), 1)

    def brief(self) -> dict:
        return {
            "task_id": self.task_id,
            "status": self.status,
            "created_at": iso(self.created_at),
            "last_progress_at": iso(self.last_progress_at),
            "items": self.item_count,
            "succeeded": self.items_succeeded,
            "failed": self.items_finished - self.items_succeeded,
        }


def load_tasks(conn: sqlite3.Connection, since: datetime) -> list[Task]:
    """Tasks created at/after `since`, classified. `frozen_work_json` (multi-MB
    for a whole-market task) is only touched through json_extract."""
    tasks: list[Task] = []
    rows = conn.execute(
        "SELECT task_id,command_key_json,status,created_at,last_progress_at,"
        "item_count,items_finished,items_succeeded FROM tasks ORDER BY created_at,task_id"
    ).fetchall()
    for task_id, key_json, status, created, progress, count, finished, succeeded in rows:
        created_at, last_at = parse_ts(created), parse_ts(progress)
        if created_at is None or last_at is None or created_at < since:
            continue
        key = json.loads(key_json)
        task = Task(
            task_id, key.get("operation", ""), key.get("dataset", ""),
            key.get("contract_version", ""), status, created_at, last_at,
            count, finished, succeeded,
        )
        classify(conn, task)
        tasks.append(task)
    return tasks


def classify(conn: sqlite3.Connection, task: Task) -> None:
    if task.operation == "start_collection":
        task.kind = "collection"
        return
    if task.operation != "update_data":
        return
    if task.contract_version == PLAN_RUN_CV:
        raw = conn.execute(
            "SELECT json_extract(frozen_work_json,'$.shared_evidence') FROM tasks WHERE task_id=?",
            (task.task_id,),
        ).fetchone()[0]
        task.evidence = json.loads(raw) if raw else {}
        if task.dataset == "market.yearly":
            task.kind = "yearly_run"
        elif task.dataset == "market.daily":
            derived, etf, total = conn.execute(
                "SELECT sum(item_id LIKE 'derived:%'),"
                "sum(json_extract(frozen_inputs_json,'$.series')='etf_raw'),count(*) "
                "FROM task_items WHERE task_id=?",
                (task.task_id,),
            ).fetchone()
            derived, etf = derived or 0, etf or 0
            # ETF-only plans add no derived items (SKILL §3.3a).
            task.kind = "etf" if derived == 0 and etf > 0 and etf == total else "a_share"
    elif task.dataset == "market.daily" and task.contract_version == EXPLICIT_DAILY_CV:
        task.kind = "explicit_daily"
    elif task.dataset == "market.yearly" and task.contract_version == EXPLICIT_YEARLY_CV:
        task.kind = "explicit_yearly"


def failed_items(conn: sqlite3.Connection, task_id: str) -> list[dict]:
    rows = conn.execute(
        "SELECT item_id,json_extract(target_json,'$.symbol'),json_extract(outcome_json,'$.error') "
        "FROM task_items WHERE task_id=? AND stage='finished' "
        "AND json_extract(outcome_json,'$.success')=0 ORDER BY item_id",
        (task_id,),
    ).fetchall()
    return [
        {
            "item_id": item_id,
            "entity": None if item_id.startswith("derived:") else symbol,
            "reason_code": reason_code(error),
            "error": (error or "")[:240],
        }
        for item_id, symbol, error in rows
    ]


def item_entities(conn: sqlite3.Connection, task_id: str) -> dict[str, dict]:
    rows = conn.execute(
        "SELECT json_extract(target_json,'$.symbol'),stage,json_extract(outcome_json,'$.success'),"
        "json_extract(outcome_json,'$.error'),json_extract(frozen_inputs_json,'$.range.end') "
        "FROM task_items WHERE task_id=?",
        (task_id,),
    ).fetchall()
    return {
        symbol: {"stage": stage, "success": success, "error": error, "range_end": range_end}
        for symbol, stage, success, error, range_end in rows
        if symbol
    }


def universe(conn: sqlite3.Connection) -> set[str] | None:
    """Current catalog plus the previous version, as `resolve_facts` does."""
    head = conn.execute("SELECT version_id FROM catalog_head WHERE singleton=1").fetchone()
    if head is None:
        return None
    ids = [head[0]]
    previous = conn.execute(
        "SELECT version_id FROM catalog_versions WHERE rowid < "
        "(SELECT rowid FROM catalog_versions WHERE version_id=?) ORDER BY rowid DESC LIMIT 1",
        (head[0],),
    ).fetchone()
    if previous:
        ids.append(previous[0])
    marks = ",".join("?" * len(ids))
    rows = conn.execute(
        f"SELECT exchange||'.'||code FROM catalog_members WHERE version_id IN ({marks})", ids
    ).fetchall()
    return {r[0] for r in rows}


def a_share_daily(conn: sqlite3.Connection, members: set[str] | None) -> dict[str, tuple]:
    """entity -> (content_digest, last_event_seq, last_day) of forward daily series."""
    rows = conn.execute(
        "SELECT entity,content_digest,last_event_seq,last_day FROM market_series "
        "WHERE period=1 AND adjustment=1 AND instrument_class=0"
    ).fetchall()
    return {e: (d, s, ld) for e, d, s, ld in rows if members is None or e in members}


def etf_daily(conn: sqlite3.Connection) -> dict[str, int]:
    rows = conn.execute(
        "SELECT entity,last_day FROM market_series WHERE period=1 AND adjustment=0 AND instrument_class=2"
    ).fetchall()
    return dict(rows)


def yearly_states(conn: sqlite3.Connection, daily: dict[str, tuple]) -> dict[str, list[str]]:
    """Classify each daily entity the way `resolve_yearly_on` does (read-only
    mirror of `series::yearly_state`). Keys: already_current, eligible_missing,
    eligible_behind, daily_replaced_since_derivation, value_semantics_changed,
    head_unreadable."""
    states: dict[str, list[str]] = {
        k: []
        for k in (
            "already_current", "eligible_missing", "eligible_behind",
            "daily_replaced_since_derivation", "value_semantics_changed", "head_unreadable",
        )
    }
    lineage = {
        entity: (series_key, base_digest, base_seq)
        for entity, series_key, base_digest, base_seq in conn.execute(
            "SELECT y.entity,y.series_key,l.base_daily_digest,l.base_event_seq "
            "FROM market_series y LEFT JOIN yearly_series_lineage l ON l.series_key=y.series_key "
            "WHERE y.period=2 AND y.adjustment=1"
        )
    }
    for entity, (digest, seq, _last_day) in sorted(daily.items()):
        if entity not in lineage:
            states["eligible_missing"].append(entity)
            continue
        _key, base_digest, base_seq = lineage[entity]
        if base_digest is None:
            states["head_unreadable"].append(entity)
        elif base_digest == digest and base_seq == seq:
            states["already_current"].append(entity)
        else:
            tail = conn.execute(
                "SELECT kind,semantics_changed FROM market_events "
                "WHERE entity=? AND period=1 AND adjustment=1 AND seq>?",
                (entity, base_seq),
            ).fetchall()
            if not tail:
                states["head_unreadable"].append(entity)
            elif any(kind == "replace" for kind, _ in tail):
                states["daily_replaced_since_derivation"].append(entity)
            elif any(changed for _, changed in tail):
                states["value_semantics_changed"].append(entity)
            else:
                states["eligible_behind"].append(entity)
    return states


# --------------------------------------------------------------------------
# context shared by the steps
# --------------------------------------------------------------------------


@dataclass
class Context:
    conn: sqlite3.Connection
    since: datetime
    active: dict | None
    active_day_file: dict | None
    acquire: sqlite3.Connection | None
    session: "Session | None"
    tasks: list[Task] = field(default_factory=list)
    members: set[str] | None = None
    daily: dict = field(default_factory=dict)
    warnings: list[str] = field(default_factory=list)
    activated: bool = False
    required: bool = True  # False on a proven "no update" day
    # Lower-case `sh510300`-style symbols in the active generation's day archive,
    # or None when the archive could not be read (then nothing is inferred from it).
    archive_members: set[str] | None = None

    def of(self, kind: str) -> list[Task]:
        return [t for t in self.tasks if t.kind == kind]

    def primary(self, kind: str) -> Task | None:
        found = self.of(kind)
        return max(found, key=lambda t: (t.item_count, -t.created_at.timestamp())) if found else None

    def order(self, earlier: str, earlier_at: datetime, later: str, later_at: datetime) -> None:
        if later_at < earlier_at:
            self.warnings.append(f"顺序：{later} 早于 {earlier} 结束（{iso(later_at)} < {iso(earlier_at)}）")

    def target_day(self) -> int | None:
        run = self.primary("a_share")
        if run and epoch_day(run.evidence.get("target_day")) is not None:
            return epoch_day(run.evidence.get("target_day"))
        days = [v[2] for v in self.daily.values()]
        return max(days) if days else None


def step(step_id: str, section: str, title: str, status: str, reason: str, basis: str = "db", **details) -> dict:
    return {
        "id": step_id, "section": section, "title": title, "status": status,
        "basis": basis, "reason": reason, "details": details,
    }


def not_required(step_id: str, section: str, title: str) -> dict:
    return step(step_id, section, title, NO_ACTION,
                "当日无新 generation 且无任何更新任务（§1 无更新分支，Skill 到此结束）",
                basis="inferred")


def task_problem(task: Task) -> str | None:
    if task.status in TERMINAL_OK:
        return None
    return f"任务 {task.task_id} 状态为 {task.status}，未走完"


# --------------------------------------------------------------------------
# steps
# --------------------------------------------------------------------------


def validators_equal(seen: dict, manifest: dict) -> bool:
    """Same comparison as `active_change` in src/acquire/service.rs."""
    if seen.get("etag") and manifest.get("etag"):
        return seen["etag"] == manifest["etag"]
    if seen.get("etag") or manifest.get("etag"):
        return False
    return bool(
        seen.get("last_modified") and manifest.get("last_modified")
        and seen.get("content_length") is not None and manifest.get("bytes") is not None
        and seen["last_modified"] == manifest["last_modified"]
        and seen["content_length"] == manifest["bytes"]
    )


def step_generation(ctx: Context) -> dict:
    """§2. executed: the active pointer was activated after `since` and moved
    off another generation. checked_no_action: the acquire store recorded a
    `day` probe after `since` whose validators equal the active manifest's
    `day` file, i.e. `day` was not pending (§1 no-update branch). Anything
    else is not_executed."""
    title = "§2 新 generation 获取、验证并激活"
    active = ctx.active
    if active:
        at = parse_ts(active.get("activated_at"))
        if at and at >= ctx.since and active.get("previous_generation_id") != active.get("generation_id"):
            ctx.activated = True
            verdict = None
            if ctx.acquire is not None:
                row = ctx.acquire.execute(
                    "SELECT state,consistency_status FROM generations WHERE generation_id=?",
                    (active["generation_id"],),
                ).fetchone()
                verdict = {"state": row[0], "consistency_status": row[1]} if row else None
            return step("2", "§2", title, EXECUTED, "active-generation.json 在窗口内切到新 generation",
                        generation_id=active["generation_id"],
                        previous_generation_id=active.get("previous_generation_id"),
                        activated_at=iso(at), acquire_store=verdict)
    if ctx.acquire is None:
        return step("2", "§2", title, MISSING,
                    "窗口内没有激活，且缺少 acquire.sqlite，无法证明 day 未 pending")
    seen = ctx.acquire.execute(
        "SELECT etag,last_modified,content_length,seen_at FROM seen WHERE role='day'"
    ).fetchone()
    seen_at = parse_ts(seen[3]) if seen else None
    if seen_at is None or seen_at < ctx.since:
        return step("2", "§2", title, MISSING,
                    "窗口内没有激活，也没有窗口内的 day 远端探测记录（check_official_updates 未执行）")
    if ctx.active_day_file and validators_equal(
        {"etag": seen[0], "last_modified": seen[1], "content_length": seen[2]}, ctx.active_day_file
    ):
        ctx.required = False
        return step("2", "§2", title, NO_ACTION,
                    "窗口内探测过 day，其 validator 与 active manifest 一致：day 未 pending",
                    basis="inferred", probed_at=iso(seen_at))
    return step("2", "§2", title, MISSING,
                "day 的远端 validator 与 active manifest 不同（day pending），窗口内却没有激活新 generation",
                probed_at=iso(seen_at))


def step_tail(ctx: Context) -> dict:
    """§3.0. executed: a `tail` row in source_refresh_runs started after
    `since`. Failed or unfinished runs still count as executed but are listed
    in `warnings` (the Skill says to report them, not to stop)."""
    title = "§3.0 refresh_exchange_sources tail"
    if not ctx.required and not ctx.tasks:
        return not_required("3.0", "§3.0", title)
    rows = ctx.conn.execute(
        "SELECT run_id,mode,status,started_at,finished_at,requests,failed_windows,unobserved_windows "
        "FROM source_refresh_runs ORDER BY started_at,run_id"
    ).fetchall()
    runs = [
        {"run_id": r[0], "mode": r[1], "status": r[2], "started_at": r[3], "finished_at": r[4],
         "requests": r[5], "failed_windows": r[6], "unobserved_windows": r[7]}
        for r in rows if (parse_ts(r[3]) or ctx.since) >= ctx.since
    ]
    tails = [r for r in runs if r["mode"] == "tail"]
    if not tails:
        return step("3.0", "§3.0", title, MISSING, "窗口内没有 tail 刷新记录",
                    other_runs=[r for r in runs if r["mode"] != "tail"])
    last = tails[-1]
    if last["status"] != "succeeded" or last["failed_windows"]:
        ctx.warnings.append(
            f"来源刷新失败：run {last['run_id']} status={last['status']} failed_windows={last['failed_windows']}"
        )
    run = ctx.primary("a_share")
    started = parse_ts(tails[0]["started_at"])
    if run and started:
        ctx.order("tail 开始", started, "A 股 run 创建", run.created_at)
        if run.created_at < started:
            ctx.warnings.append("tail 晚于 A 股 plan/run 开始（Skill：先 tail 再 plan/run）")
    return step("3.0", "§3.0", title, EXECUTED, f"{len(tails)} 个 tail run", tails=tails,
                full_runs=[r for r in runs if r["mode"] == "full"])


def step_a_run(ctx: Context) -> dict:
    """§3.1/§3.2. executed: an A-share plan/run task (items without
    `series=etf_raw`, with the derived items) reached succeeded or
    partially_failed. No task: checked_no_action only if the session shows an
    A-share plan with eligible=0 (or the no-update branch applies, see
    step_generation). Never inferred from the daily heads: they cannot tell a
    skipped run from an up-to-date database."""
    title = "§3.2 A 股日线 plan/run 并读出 failed_by_reason"
    runs = ctx.of("a_share")
    if not runs:
        if not ctx.required and not ctx.tasks:
            return not_required("3.2", "§3.2", title)
        if ctx.session:
            plan = ctx.session.plan("market.daily", None)
            if plan is not None and plan.get("eligible") == 0:
                return step("3.2", "§3.2", title, NO_ACTION, "会话中的 A 股 plan eligible=0",
                            basis="session", counts=plan)
        # No head-based inference here: the newest day would be read from the
        # same heads a skipped run leaves behind, so "all current" would pass
        # a day on which nothing ran.
        return step("3.2", "§3.2", title, MISSING,
                    "窗口内没有 A 股 run_market_update 任务，也没有会话里 eligible=0 的 plan 作证据")
    run = ctx.primary("a_share")
    failures = failed_items(ctx.conn, run.task_id)
    by_reason = dict(sorted(Counter(f["reason_code"] for f in failures).items()))
    derived = [f for f in failures if f["entity"] is None]
    details = dict(
        task=run.brief(), all_a_share_tasks=[t.task_id for t in runs],
        plan_id=run.evidence.get("plan_id"), target_day=run.evidence.get("target_day"),
        plan_counts=run.evidence.get("counts"), failed_by_reason=by_reason,
        failed_derived=[{"item_id": f["item_id"], "reason_code": f["reason_code"]} for f in derived],
    )
    if len(runs) > 1:
        ctx.warnings.append(f"出现 {len(runs)} 个 A 股 plan/run 任务（D-63 口径是 1 个 task）")
    problem = task_problem(run)
    if problem:
        return step("3.2", "§3.2", title, MISSING, problem, **details)
    return step("3.2", "§3.2", title, EXECUTED,
                f"任务 {run.task_id}：{run.item_count} 项，失败 {run.item_count - run.items_succeeded}",
                **details)


def step_revision_backfill(ctx: Context) -> dict:
    """§3.3. The required set is every non-derived item of the A-share run
    that failed with REVISION_UNVERIFIED. executed: explicit `update_data`
    daily tasks created after that run, all terminal, cover the whole set.
    No such failure: checked_no_action."""
    title = "§3.3 REVISION_UNVERIFIED 前向窗口补漏"
    run = ctx.primary("a_share")
    if run is None:
        if not ctx.required and not ctx.tasks:
            return not_required("3.3", "§3.3", title)
        return step("3.3", "§3.3", title, MISSING, "没有 A 股 run，无法确定补漏集合")
    required = {
        f["entity"] for f in failed_items(ctx.conn, run.task_id)
        if f["entity"] and f["reason_code"] == "REVISION_UNVERIFIED"
    }
    backfills = [t for t in ctx.of("explicit_daily") if t.created_at >= run.created_at]
    if not required:
        extra = f"（另有 {len(backfills)} 个显式日线任务）" if backfills else ""
        return step("3.3", "§3.3", title, NO_ACTION, "A 股 run 没有 REVISION_UNVERIFIED 失败项" + extra,
                    basis="db", required=0, tasks=[t.task_id for t in backfills])
    covered: dict[str, dict] = {}
    outcomes: Counter = Counter()
    target = run.evidence.get("target_day")
    end_mismatch: list[str] = []
    open_tasks = [t for t in backfills if not t.terminal_ok]
    for task in backfills:
        for symbol, item in item_entities(ctx.conn, task.task_id).items():
            if task.terminal_ok:
                covered[symbol] = item
            if item["range_end"] and target and item["range_end"] != target:
                end_mismatch.append(symbol)
            if item["stage"] == "finished":
                outcomes["success" if item["success"] else reason_code(item["error"])] += 1
    missing = required - set(covered)
    extra = set(covered) - required
    if extra:
        ctx.warnings.append(f"补漏任务含 {len(extra)} 个不在 REVISION_UNVERIFIED 集合里的实体")
    if end_mismatch:
        ctx.warnings.append(f"{len(set(end_mismatch))} 个补漏项的 end_date 不是 target_day {target}")
    details = dict(required=len(required), covered=len(required & set(covered)),
                   extra=sample(extra), tasks=[t.brief() for t in backfills],
                   item_outcomes=dict(outcomes))
    if missing:
        why = f"{len(missing)}/{len(required)} 个 REVISION_UNVERIFIED 实体没有对应的补漏项"
        if open_tasks:
            why += f"；{len(open_tasks)} 个补漏任务未到终态"
        return step("3.3", "§3.3", title, MISSING, why, uncovered=sample(missing), **details)
    return step("3.3", "§3.3", title, EXECUTED,
                f"{len(required)} 个实体全部有补漏项", **details)


def step_etf_batch(ctx: Context) -> dict:
    """§3.3a ETF daily batch. executed: an ETF-only plan/run (all items
    `series=etf_raw`, no derived items) reached a successful terminal state.
    No task: checked_no_action if the session shows an ETF plan with
    eligible=0, there is no ETF raw daily series at all (Skill: "eligible 为 0
    -> 不发 run"), or every ETF series is already on the target day."""
    title = "§3.3a ETF 日线独立批次"
    runs = ctx.of("etf")
    if runs:
        run = ctx.primary("etf")
        failures = failed_items(ctx.conn, run.task_id)
        problem = task_problem(run)
        details = dict(
            task=run.brief(), write_loop_seconds=run.write_loop_seconds,
            failed_by_reason=dict(sorted(Counter(f["reason_code"] for f in failures).items())),
            failed_prefixes=dict(Counter(
                (f["error"].split(":", 1)[0] if ":" in f["error"] else "")[:40] for f in failures)),
        )
        a_run = ctx.primary("a_share")
        if a_run:
            ctx.order("A 股 run 终态", a_run.last_progress_at, "ETF run 创建", run.created_at)
        for task in ctx.of("explicit_daily"):
            ctx.order("日线补漏终态", task.last_progress_at, "ETF run 创建", run.created_at)
        if problem:
            return step("3.3a", "§3.3a", title, MISSING, problem, **details)
        return step("3.3a", "§3.3a", title, EXECUTED, f"ETF 任务 {run.task_id}：{run.item_count} 项", **details)
    if not ctx.required and not ctx.tasks:
        return not_required("3.3a", "§3.3a", title)
    if ctx.session:
        plan = ctx.session.plan("market.daily", "sh_sz_etf_current")
        if plan is not None and plan.get("eligible") == 0:
            return step("3.3a", "§3.3a", title, NO_ACTION, "会话中的 ETF plan eligible=0",
                        basis="session", counts=plan)
    etf = etf_daily(ctx.conn)
    target = ctx.target_day()
    if not etf:
        return step("3.3a", "§3.3a", title, NO_ACTION, "库里没有 ETF 不复权日线序列，ETF plan 的 eligible 为 0",
                    basis="inferred", etf_series=0)
    lag = [e for e, d in etf.items() if target is not None and d < target]
    if not lag:
        return step("3.3a", "§3.3a", title, NO_ACTION, "所有 ETF 序列已到目标日", basis="inferred",
                    etf_series=len(etf), target_day=day_text(target))
    return step("3.3a", "§3.3a", title, MISSING,
                f"窗口内没有 ETF run；{len(lag)}/{len(etf)} 个 ETF 序列落后于 {day_text(target)}",
                lagging=sample(lag))


def step_etf_new_listing(ctx: Context) -> dict:
    """§3.3a new members. Inferred only from the DB: a current ETF member in
    `security_master` (class 2, sh/sz, open interval, no delisting) with no raw
    daily series, already listed on or before the newest day, and not a known
    gap. A known gap is a `proven_gaps` day row for the symbol or a
    first-collection item that failed with `member absent or ambiguous`
    (the official snapshot lacks the member). A member that is not in the
    active generation's day archive at all is a known gap too: no collection can
    produce a series for it, so it is listed apart and never flagged. Any other
    such member needs the operator's one-off ETF first collection (Skill §3.3a,
    not a daily step)."""
    title = "§3.3a ETF 新上市成员补采"
    if not ctx.required and not ctx.tasks:
        return not_required("3.3a-new", "§3.3a", title)
    members = ctx.conn.execute(
        "SELECT exchange||'.'||code,listing_date FROM security_master "
        "WHERE instrument_class=2 AND exchange IN ('sh','sz') AND effective_to IS NULL "
        "AND delisting_source='unknown'"
    ).fetchall()
    series = set(etf_daily(ctx.conn))
    target = ctx.target_day()
    gaps = {r[0] for r in ctx.conn.execute("SELECT symbol FROM proven_gaps WHERE period='day'")}
    gaps |= {
        r[0] for r in ctx.conn.execute(
            "SELECT DISTINCT json_extract(i.target_json,'$.symbol') FROM task_items i "
            "JOIN tasks t ON t.task_id=i.task_id WHERE t.command_key_json LIKE '%start_collection%' "
            "AND i.stage='finished' AND json_extract(i.outcome_json,'$.success')=0 "
            "AND json_extract(i.outcome_json,'$.error') LIKE '%member absent or ambiguous%'"
        ) if r[0]
    }
    lacking, not_yet, known, absent = [], [], [], []
    for entity, listing in members:
        if entity in series:
            continue
        if entity in gaps:
            known.append(entity)
        elif ctx.archive_members is not None and entity.replace(".", "") not in ctx.archive_members:
            absent.append(entity)
        elif listing is not None and target is not None and listing > target:
            not_yet.append(entity)
        else:
            lacking.append(entity)
    details = dict(current_members=len(members), with_series=len(series & {m[0] for m in members}),
                   known_gap=sample(known), listed_after_target=sample(not_yet),
                   absent_from_official_archive=len(absent), absent_sample=sample(absent))
    if lacking:
        return step("3.3a-new", "§3.3a", title, MISSING,
                    f"{len(lacking)} 个 ETF 成员在 security_master 里有、没有序列、也不是已知缺段",
                    basis="inferred", missing=sample(lacking), **details)
    return step("3.3a-new", "§3.3a", title, NO_ACTION, "没有缺序列的新上市 ETF 成员", basis="inferred", **details)


def step_yearly_run(ctx: Context) -> dict:
    """§3.4/§3.5. executed: a yearly plan/run task reached a successful
    terminal state. No task: checked_no_action if the session shows a yearly
    plan with eligible=0, or every catalog member's yearly series is already
    current (no missing/behind entity)."""
    title = "§3.5 年线 plan/run"
    run = ctx.primary("yearly_run")
    if run:
        failures = failed_items(ctx.conn, run.task_id)
        details = dict(
            task=run.brief(), plan_id=run.evidence.get("plan_id"),
            plan_counts=run.evidence.get("counts"),
            ineligible_reasons=run.evidence.get("ineligible_reasons"),
            failed_by_reason=dict(sorted(Counter(f["reason_code"] for f in failures).items())),
        )
        if len(ctx.of("yearly_run")) > 1:
            ctx.warnings.append(f"出现 {len(ctx.of('yearly_run'))} 个年线 plan/run 任务（D-69 口径是 1 个 task）")
        for label, tasks in (("A 股 run", ctx.of("a_share")), ("ETF run", ctx.of("etf")),
                             ("日线补漏", ctx.of("explicit_daily"))):
            for task in tasks:
                ctx.order(f"{label}终态", task.last_progress_at, "年线 run 创建", run.created_at)
        problem = task_problem(run)
        if problem:
            return step("3.5", "§3.5", title, MISSING, problem, **details)
        return step("3.5", "§3.5", title, EXECUTED, f"任务 {run.task_id}：{run.item_count} 项", **details)
    if not ctx.required and not ctx.tasks:
        return not_required("3.5", "§3.5", title)
    if ctx.session:
        plan = ctx.session.plan("market.yearly", None)
        if plan is not None and plan.get("eligible") == 0:
            return step("3.5", "§3.5", title, NO_ACTION, "会话中的年线 plan eligible=0", basis="session", counts=plan)
    states = yearly_states(ctx.conn, ctx.daily)
    pending = states["eligible_missing"] + states["eligible_behind"]
    if not pending:
        return step("3.5", "§3.5", title, NO_ACTION, "没有缺年线或落后的实体，年线 plan 的 eligible 为 0",
                    basis="inferred", state_counts={k: len(v) for k, v in states.items()})
    return step("3.5", "§3.5", title, MISSING, f"窗口内没有年线 run；{len(pending)} 个实体年线缺失或落后",
                pending=sample(pending))


def step_yearly_republish(ctx: Context) -> dict:
    """§3.6. Expected count = daily_replaced_since_derivation +
    value_semantics_changed in the yearly run's recorded `ineligible_reasons`.
    executed: terminal explicit yearly `update_data` tasks created after the
    yearly run cover at least that many distinct entities. Expected 0 (or, with
    no yearly run, no invalid entity in the DB now): checked_no_action."""
    title = "§3.6 年线失效实体显式重推"
    run = ctx.primary("yearly_run")
    if run is None:
        if not ctx.required and not ctx.tasks:
            return not_required("3.6", "§3.6", title)
        states = yearly_states(ctx.conn, ctx.daily)
        invalid = states["daily_replaced_since_derivation"] + states["value_semantics_changed"]
        if invalid:
            return step("3.6", "§3.6", title, MISSING, f"{len(invalid)} 个实体年线已失效，窗口内没有重推",
                        invalid=sample(invalid))
        return step("3.6", "§3.6", title, NO_ACTION, "没有年线失效实体", basis="inferred")
    expected = sum(
        entry.get("count", 0) for entry in (run.evidence.get("ineligible_reasons") or [])
        if entry.get("reason") in ("daily_replaced_since_derivation", "value_semantics_changed")
    )
    tasks = [t for t in ctx.of("explicit_yearly") if t.created_at >= run.created_at]
    for task in tasks:
        ctx.order("年线 run 终态", run.last_progress_at, "年线重推创建", task.created_at)
    covered: set[str] = set()
    for task in tasks:
        if task.terminal_ok:
            covered |= set(item_entities(ctx.conn, task.task_id))
    details = dict(expected=expected, covered=len(covered), tasks=[t.brief() for t in tasks])
    if expected == 0:
        return step("3.6", "§3.6", title, NO_ACTION if not tasks else EXECUTED,
                    "年线 plan 没有失效实体" + (f"，仍提交了 {len(tasks)} 个重推任务" if tasks else ""),
                    **details)
    if len(covered) < expected:
        return step("3.6", "§3.6", title, MISSING,
                    f"年线 plan 报告 {expected} 个失效实体，重推任务只覆盖 {len(covered)} 个", **details)
    return step("3.6", "§3.6", title, EXECUTED, f"重推覆盖 {len(covered)} 个实体（需要 {expected}）", **details)


def step_converged(ctx: Context) -> dict:
    """End state (§3.6 last paragraph, §4 last bullet). Not converged when a
    catalog member still has a yearly series that is missing, behind, invalid
    or unreadable, or when a daily head lags the target day *and* its last
    failure in the A-share run was not an expected fail-closed
    (NO_BARS_IN_RANGE) and no successful backfill item exists for it. A lagging
    head whose run item succeeded is a suspended security: Skill §3.1 treats
    it as done."""
    title = "收尾：最后一次 plan 是否收敛"
    if not ctx.required and not ctx.tasks:
        return not_required("converge", "§3.6/§4", title)
    states = yearly_states(ctx.conn, ctx.daily)
    yearly_open = {k: v for k, v in states.items() if k != "already_current" and v}
    run = ctx.primary("a_share")
    unresolved: list[str] = []
    target = ctx.target_day()
    if run and target is not None:
        failed = {f["entity"]: f["reason_code"] for f in failed_items(ctx.conn, run.task_id) if f["entity"]}
        ok_backfill = {
            s for t in ctx.of("explicit_daily") if t.created_at >= run.created_at
            for s, i in item_entities(ctx.conn, t.task_id).items() if i["success"] == 1
        }
        for entity, reason in failed.items():
            if (reason != "NO_BARS_IN_RANGE" and entity not in ok_backfill
                    and ctx.daily.get(entity, (0, 0, target))[2] < target):
                unresolved.append(entity)
    details = dict(
        yearly={k: len(v) for k, v in states.items()},
        yearly_open={k: sample(v) for k, v in yearly_open.items()},
        daily_unresolved=sample(unresolved), target_day=day_text(target),
    )
    if yearly_open or unresolved:
        parts = []
        if yearly_open:
            parts.append("年线未收敛 " + ", ".join(f"{k}={len(v)}" for k, v in yearly_open.items()))
        if unresolved:
            parts.append(f"{len(unresolved)} 个日线失败实体仍落后且没有成功补漏")
        return step("converge", "§3.6/§4", title, MISSING, "；".join(parts), basis="inferred", **details)
    return step("converge", "§3.6/§4", title, NO_ACTION,
                "年线全部 already_current，日线失败实体已补齐或属预期 fail-closed", basis="inferred", **details)


# --------------------------------------------------------------------------
# session (pi JSONL)
# --------------------------------------------------------------------------


@dataclass
class Call:
    index: int
    call_id: str
    tool: str
    text: str  # JSON text of the arguments; scripts (codemode) are matched on text too


@dataclass
class Session:
    calls: list[Call]
    results: dict[str, str]
    final_text: str

    def plan(self, dataset: str, scope: str | None) -> dict | None:
        """`counts` of the last matching plan_market_update result, if any."""
        found = None
        for call in self.calls:
            if call.tool != "plan_market_update" or f'"{dataset}"' not in call.text:
                continue
            has_scope = "scope" in call.text and "sh_sz" in call.text
            if (scope is None) == has_scope or (scope and scope not in call.text):
                continue
            counts = find_key(parse_json_text(self.results.get(call.call_id, "")), "counts")
            if isinstance(counts, dict):
                found = counts
        return found


def parse_json_text(text: str) -> Any:
    start = text.find("{")
    if start < 0:
        return None
    try:
        return json.JSONDecoder().raw_decode(text[start:])[0]
    except ValueError:
        return None


def find_key(obj: Any, key: str) -> Any:
    if isinstance(obj, dict):
        if key in obj:
            return obj[key]
        for value in obj.values():
            found = find_key(value, key)
            if found is not None:
                return found
    elif isinstance(obj, list):
        for value in obj:
            found = find_key(value, key)
            if found is not None:
                return found
    return None


def tool_names(name: str, args_text: str) -> list[str]:
    """`mcp__x_data_mcp__get_task` -> `get_task`. Calls routed through a
    script (codemode) name their MCP tools inside the arguments."""
    flat = re.sub(r"[^A-Za-z0-9_]", "_", name)
    if flat.startswith("mcp__"):
        return [flat.rpartition("__")[2]]
    return re.findall(r"mcp__[A-Za-z0-9_]*?__([a-z][a-z_]*[a-z])", re.sub(r"-", "_", args_text))


def read_session(path: Path) -> Session:
    if not path.is_file():
        raise InputError(f"session file not found: {path}")
    calls: list[Call] = []
    results: dict[str, str] = {}
    final_text = ""
    index = 0
    for line in path.read_text(encoding="utf-8").splitlines():
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        message = entry.get("message") if entry.get("type") == "message" else None
        if not isinstance(message, dict):
            continue
        role, content = message.get("role"), message.get("content")
        if role == "toolResult":
            blocks = content if isinstance(content, list) else []
            results[message.get("toolCallId", "")] = "\n".join(
                b.get("text", "") for b in blocks if isinstance(b, dict))
        elif role == "assistant" and isinstance(content, list):
            texts = [b.get("text", "") for b in content if isinstance(b, dict) and b.get("type") == "text"]
            if "".join(texts).strip():
                final_text = "\n".join(texts)
            for block in content:
                if isinstance(block, dict) and block.get("type") == "toolCall":
                    args_text = json.dumps(block.get("arguments", {}), ensure_ascii=False)
                    for tool in tool_names(block.get("name", ""), args_text):
                        calls.append(Call(index, block.get("id", ""), tool, args_text))
                        index += 1
    return Session(calls, results, final_text)


CLAIM = re.compile(
    r"(?<!not )(?<!partially )(?<!un)\bcompleted\b|(?<![未部分不])(已完成|全部完成|日更完成)",
    re.IGNORECASE,
)


def step_session_readback(ctx: Context, etf_involved: bool) -> dict:
    """§4. After the last write call of the session (run_market_update,
    update_data, start_collection) there must be: get_data_availability and
    query_data on market.daily, list_data_versions for the four M2 datasets,
    and, when ETF data is involved, a market.daily read with adjustment=raw."""
    title = "§4 回读（会话）"
    session = ctx.session
    if not ctx.required and not ctx.tasks:
        return not_required("4", "§4", title)
    writes = [c.index for c in session.calls if c.tool in WRITE_TOOLS]
    after = [c for c in session.calls if c.index > (max(writes) if writes else -1)]

    def has(tools: set[str], needle: str) -> bool:
        return any(c.tool in tools and needle in c.text for c in after)

    missing = []
    if not has({"get_data_availability"}, "market.daily"):
        missing.append("get_data_availability(market.daily)")
    if not has({"query_data"}, "market.daily"):
        missing.append("query_data(market.daily)")
    for dataset in M2_DATASETS:
        if not has({"list_data_versions"}, dataset):
            missing.append(f"list_data_versions({dataset})")
    if etf_involved and not any(
        c.tool in {"query_data", "get_data_availability", "list_data_versions"}
        and "market.daily" in c.text and re.search(r"adjustment[\"']?\s*:\s*\\?[\"']raw", c.text)
        for c in after
    ):
        missing.append("ETF 回读（market.daily, adjustment=raw）")
    counts = dict(Counter(c.tool for c in session.calls))
    if missing:
        return step("4", "§4", title, MISSING, "最后一次写调用之后缺少回读：" + "；".join(missing),
                    basis="session", missing=missing, tool_calls=counts)
    return step("4", "§4", title, EXECUTED, "回读调用齐全", basis="session", tool_calls=counts)


# --------------------------------------------------------------------------
# readings (D-63 / D-69 / hot generations)
# --------------------------------------------------------------------------


def readings(ctx: Context) -> dict:
    out: dict[str, Any] = {}
    run = ctx.primary("a_share")
    if run:
        seconds = run.write_loop_seconds
        out["d63_g1"] = {
            "task_id": run.task_id, "items": run.item_count, "succeeded": run.items_succeeded,
            "write_loop_seconds": seconds, "gate_seconds": G1_GATE_SECONDS,
            "under_gate": seconds < G1_GATE_SECONDS, "cross_generation": ctx.activated,
            "terminal": run.status in TERMINAL_ALL, "a_share_tasks": len(ctx.of("a_share")),
            "counts_toward_d63": ctx.activated and run.terminal_ok and seconds < G1_GATE_SECONDS,
            "resets_count": seconds >= G1_GATE_SECONDS,
            "note": "D-63 只数有新 generation 激活的生产日更；>=600s 清零并附原因分析。"
                    "脚本给出本次读数，不累计，也不判断机器是不是生产。",
        }
    else:
        out["d63_g1"] = None
    yearly = ctx.primary("yearly_run")
    if yearly:
        seconds = yearly.write_loop_seconds
        out["d69_yearly"] = {
            "task_id": yearly.task_id, "items": yearly.item_count, "succeeded": yearly.items_succeeded,
            "write_loop_seconds": seconds, "gate_seconds": YEARLY_GATE_SECONDS,
            "under_gate": seconds < YEARLY_GATE_SECONDS, "yearly_run_tasks": len(ctx.of("yearly_run")),
            "single_task_form": len(ctx.of("yearly_run")) == 1,
            "counts_toward_d69": yearly.terminal_ok and seconds < YEARLY_GATE_SECONDS
            and len(ctx.of("yearly_run")) == 1,
            "resets_count": seconds >= YEARLY_GATE_SECONDS,
            "note": "D-69 只数生产日更、单任务形态；>=600s 清零并附原因分析。脚本不累计。",
        }
    else:
        out["d69_yearly"] = None
    etf = ctx.primary("etf")
    out["etf_timing"] = (
        {"task_id": etf.task_id, "write_loop_seconds": etf.write_loop_seconds,
         "note": "ETF 耗时单独汇报，不计入 D-63"} if etf else None
    )
    out["hot_generations"] = hot_generations(ctx)
    return out


def hot_generations(ctx: Context) -> dict | None:
    if ctx.acquire is None:
        return None
    by_state = dict(ctx.acquire.execute("SELECT state,count(*) FROM generations GROUP BY state").fetchall())
    hot = sum(n for state, n in by_state.items() if state != "evidence_only")
    return {"hot": hot, "max": MAX_HOT_GENERATIONS, "by_state": by_state,
            "at_capacity": hot >= MAX_HOT_GENERATIONS,
            "note": "hot = 状态不是 evidence_only 的代；到 6 后 acquire_generation 被拒"}


# --------------------------------------------------------------------------
# driver
# --------------------------------------------------------------------------


def read_archive_members(root: Path, active: dict | None) -> set[str] | None:
    """Symbols (`sh510300`) in the active generation's `hsjday.zip`, read from the
    zip directory only. None when there is no active generation or no archive."""
    if not active:
        return None
    path = root / "generations" / active.get("generation_id", "") / "hsjday.zip"
    if not path.is_file():
        return None
    import zipfile
    try:
        with zipfile.ZipFile(path) as archive:
            return {n.replace("\\", "/").rsplit("/", 1)[-1][:-4].lower()
                    for n in archive.namelist() if n.lower().endswith(".day")}
    except (OSError, zipfile.BadZipFile):
        return None


def read_generation_root(root: Path) -> tuple[dict | None, dict | None, sqlite3.Connection | None]:
    if not root.is_dir():
        raise InputError(f"not a directory: {root}")
    active, day_file = None, None
    pointer = root / "active-generation.json"
    if pointer.is_file():
        active = json.loads(pointer.read_text(encoding="utf-8"))
        manifest = root / "generations" / active.get("generation_id", "") / "snapshot-manifest.json"
        if manifest.is_file():
            for entry in json.loads(manifest.read_text(encoding="utf-8")).get("files", []):
                if entry.get("role") == "day":
                    day_file = entry
    store = root / "acquire.sqlite"
    return active, day_file, open_readonly(store) if store.is_file() else None


def run_checks(
    conn: sqlite3.Connection,
    since: datetime,
    active: dict | None,
    active_day_file: dict | None,
    acquire: sqlite3.Connection | None,
    session: Session | None = None,
    archive_members: set[str] | None = None,
) -> dict:
    """Pure function over open connections; the tests drive it directly."""
    present = {r[0] for r in conn.execute("SELECT name FROM sqlite_master WHERE type='table'")}
    absent = [t for t in REQUIRED_TABLES if t not in present]
    if absent:
        raise InputError("database lacks tables (schema too old or not a market DB): " + ", ".join(absent))
    ctx = Context(conn, since, active, active_day_file, acquire, session)
    ctx.archive_members = archive_members
    ctx.tasks = load_tasks(conn, since)
    ctx.members = universe(conn)
    ctx.daily = a_share_daily(conn, ctx.members)

    steps = [step_generation(ctx)]
    work_tasks = [t for t in ctx.tasks if t.kind != "other"]
    if ctx.required is False and work_tasks:
        ctx.required = True  # someone worked on a no-update day; judge it normally
    ctx.tasks = work_tasks if not ctx.required else ctx.tasks
    steps += [
        step_tail(ctx), step_a_run(ctx), step_revision_backfill(ctx), step_etf_batch(ctx),
        step_etf_new_listing(ctx), step_yearly_run(ctx), step_yearly_republish(ctx), step_converged(ctx),
    ]
    session_report = None
    if session is not None:
        etf_involved = bool(ctx.of("etf")) or bool(etf_daily(conn))
        steps.append(step_session_readback(ctx, etf_involved))
        missing = [s for s in steps if s["status"] == MISSING]
        claims = bool(CLAIM.search(session.final_text))
        session_report = {
            "final_message_excerpt": session.final_text.strip()[:400],
            "claims_completed": claims,
            "false_completed": claims and bool(missing),
        }
    missing_ids = [s["id"] for s in steps if s["status"] == MISSING]
    false_completed = bool(session_report and session_report["false_completed"])
    return {
        "since": iso(since),
        "ok": not missing_ids and not false_completed,
        "missing_steps": missing_ids,
        "steps": steps,
        "failed_by_reason": next((s["details"].get("failed_by_reason") for s in steps if s["id"] == "3.2"), None),
        "readings": readings(ctx),
        "session": session_report,
        "warnings": ctx.warnings,
    }


MARK = {EXECUTED: "✔ 已执行", NO_ACTION: "○ 已检查无需动作", MISSING: "✘ 未执行"}


def summary(report: dict, color: bool) -> str:
    def red(text: str) -> str:
        return f"\033[31;1m{text}\033[0m" if color else text

    lines = [f"日更核对（窗口起点 {report['since']}）"]
    for s in report["steps"]:
        mark = MARK[s["status"]]
        mark = red(mark) if s["status"] == MISSING else mark
        lines.append(f"  {mark}  {s['section']:<8} {s['title']}  [{s['basis']}]")
        lines.append(f"      {s['reason']}")
    fbr = report["failed_by_reason"]
    if fbr is not None:
        lines.append("failed_by_reason: " + (json.dumps(fbr, ensure_ascii=False) if fbr else "无失败项"))
    r = report["readings"]
    if r["d63_g1"]:
        g = r["d63_g1"]
        lines.append(
            f"D-63 G1：{g['write_loop_seconds']}s（门槛 <{g['gate_seconds']}s，"
            f"{'低于' if g['under_gate'] else '超过'}），{g['items']} 项/{g['succeeded']} 成功，"
            f"跨代={'是' if g['cross_generation'] else '否'}，本次计入={'是' if g['counts_toward_d63'] else '否'}")
    if r["d69_yearly"]:
        y = r["d69_yearly"]
        lines.append(
            f"D-69 年线：{y['write_loop_seconds']}s（门槛 <{y['gate_seconds']}s，"
            f"{'低于' if y['under_gate'] else '超过'}），{y['items']} 项，单任务={'是' if y['single_task_form'] else '否'}，"
            f"本次计入={'是' if y['counts_toward_d69'] else '否'}")
    if r["etf_timing"]:
        lines.append(f"ETF 批次：{r['etf_timing']['write_loop_seconds']}s（不计入 D-63）")
    hot = r["hot_generations"]
    lines.append(
        f"热 generation：{hot['hot']}/{hot['max']} {hot['by_state']}" if hot else "热 generation：无 acquire.sqlite")
    for warning in report["warnings"]:
        lines.append(f"注意：{warning}")
    sess = report["session"]
    if sess and sess["false_completed"]:
        lines.append(red("!! 会话结论声称 completed，但有未执行步骤：" + ", ".join(report["missing_steps"])))
    elif sess:
        lines.append("会话结论：" + ("声称完成" if sess["claims_completed"] else "未声称完成"))
    if report["ok"]:
        lines.append("结果：通过（每个必做步骤都已执行或已检查无需动作）")
    else:
        lines.append(red("结果：未通过，未执行步骤 " + ", ".join(report["missing_steps"])))
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--db", required=True, type=Path, help="preview.sqlite (opened read-only)")
    parser.add_argument("--generations", required=True, type=Path, help="generation root directory")
    parser.add_argument("--since", required=True, help="UTC start of the run window (ISO 8601)")
    parser.add_argument("--session", type=Path, help="pi session JSONL of the daily-sync agent")
    args = parser.parse_args(argv)
    try:
        since = parse_ts(args.since)
        if since is None:
            raise InputError(f"--since is not an ISO 8601 time: {args.since!r}")
        conn = open_readonly(args.db)
        active, day_file, acquire = read_generation_root(args.generations)
        session = read_session(args.session) if args.session else None
        report = run_checks(conn, since, active, day_file, acquire, session,
                            read_archive_members(args.generations, active))
        report["db"] = {"path": str(args.db), "schema_version": conn.execute("PRAGMA user_version").fetchone()[0]}
    except (InputError, sqlite3.Error, ValueError, OSError) as error:
        print(f"check_daily_run: {error}", file=sys.stderr)
        return 2
    print(json.dumps(report, ensure_ascii=False, indent=2, sort_keys=False))
    print(summary(report, color=sys.stderr.isatty()), file=sys.stderr)
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
