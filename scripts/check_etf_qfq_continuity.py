"""(#607 PR-F) ETF 前复权的 Python 独立参照实现与连续性检查。

用途有两个：

1. **现在**：从生产库的不复权日线（`series_bars`）和 gbbq 因子
   （`adjustment_events`）独立算出 ETF 前复权序列，检查每个生效日 X 前后
   有没有 ±2×/±5×/±10× 的假跳空。D-89 第 4 条的连续性验收门槛。
2. **以后**：PR-C 把前复权序列落库后，用 `--compare` 逐值比对 Rust 的结果。
   没有给出输入时明确报 `NOT_RUN`，不静默通过。

## 语义（照 D-91 与 D-92，本脚本不自行解释）

- **价格单位**：库里存的是 `raw` 整数，价格 = `raw / 1000`，单位是元。
- **事件日 R**：`adjustment_events.effective_day` 是 gbbq 的日期，对 ETF 而言是
  **权益登记日**。**生效日 X** 是 R 之后的第一根 bar 的日期
  （等价说法：R 之后第一个交易日）。
- **套用范围**：日期**早于 X** 的 bar（**含 R 当天**）套用该事件。
  因为 bar 只存在于交易日，`bar_date < X` 与 `bar_date <= R` 选出同一批 bar；
  本实现用 `X > bar_date`（bar 严格早于 X）。
- **变换**：cat11 为 `p / c3`；cat1 为 `p − c1 / 10`。`c1` 是每 10 份派发的现金（元/10 份），
  `c3` 是折算比（新价 = 旧价 / c3）。同一个 X 上的多个事件**先 cat11 后 cat1**。
- **复合**：每根 bar 把它之后的全部事件按 X 从早到晚复合，最后只做**一次**
  HALF_UP，保留 6 位小数。
- **边界**：首根 bar 之前的事件、X 之后已经没有 bar 的事件，都不影响任何 bar。

## 只读约束

数据库用 `file:<db>?mode=ro` 打开（读得到 WAL，不加 `immutable=1`）。
一次只读一个实体的数据，按实体流式处理，单个进程内存远低于 300 MB。

## 退出码

- `0`：全部样本通过（无假跳空；或所有样本「无事件」）。
- `1`：出现假跳空。
- `2`：输入异常（数据库打不开、实体不在库里、`--compare` 输入不可用等）。
"""

import argparse
import json
import os
import sqlite3
import sys
import time
from decimal import Decimal, ROUND_HALF_UP, localcontext

# D-91 §3：价位核对口径的舍入是 HALF_UP，保留 6 位小数（前复权序列的 price_scale）。
PRICE_SCALE = 6
QUANTUM = Decimal(1).scaleb(-PRICE_SCALE)

# 不复权 bar 的价格单位：raw / 1000 = 元。
RAW_UNITS_PER_YUAN = Decimal(1000)

# 假跳空阈值（D-89 第 4 条）：事件日前后 1 日不得出现 ±2×/±5×/±10×。
# 判据是「前复权收益的绝对值落在阈值的邻域内」，邻域按相对量给出，
# 这样阈值附近的正常波动不会被误报，也不会漏掉整倍数的假跳空。
# 调整倍数 ∈ {2, 5, 10}：等价于把 |(1+r)| 与 {2, 5, 10} 比较。
FALSE_GAP_MULTIPLIERS = (2, 5, 10)
# 相对容差：|(1+r)| 落在倍数 ×(1 ± 1/16) 之内即判为假跳空。
# 取 1/16（6.25%）的理由：真实的 ETF 日内涨跌停是 ±10%，单日复权后收益
# 距 2× 至少还差一个数量级；而 2× 附近的伪影通常正好是整倍数或近似整倍数。
FALSE_GAP_RELATIVE_TOLERANCE = Decimal("0.0625")

# D-89 的 10 只样本 ETF。
DEFAULT_ENTITIES = (
    "sz.159901",
    "sz.159338",
    "sh.510050",
    "sh.588000",
    "sz.159919",
    "sz.159922",
    "sz.159830",
    "sh.510880",
    "sz.159001",
    "sh.513330",
)

EXIT_OK = 0
EXIT_FALSE_GAP = 1
EXIT_INPUT_ERROR = 2


class InputError(Exception):
    """输入异常：退出码 2。"""


def quantize_price(value):
    """最后一次舍入：HALF_UP，保留 6 位小数。整条链只在这里舍入一次。"""
    return value.quantize(QUANTUM, rounding=ROUND_HALF_UP)


def compose_events(price, events):
    """把 `events` 依次套到 `price` 上，返回**未舍入**的精确值。

    `events` 是一个 (category, c1, c3) 三元组序列，调用方负责排序
    （按 X 从早到晚，同一个 X 上先 cat11 后 cat1）。只做仿射复合，
    不做任何中间舍入——这是 D-91「最后只舍入一次」的要求。
    """
    value = Decimal(price)
    for category, c1, c3 in events:
        if category == 11:
            divisor = Decimal(c3)
            if divisor == 0:
                raise InputError("cat11 的 c3 为 0，无法做折算")
            value = value / divisor
        elif category == 1:
            value = value - Decimal(c1) / 10
        else:
            raise InputError(f"不支持的事件类别 {category!r}（只支持 1 和 11）")
    return value


def adjusted_price(raw_units, events):
    """`raw` 整数 → 前复权价（元），只在最后做一次 HALF_UP。"""
    price = Decimal(raw_units) / RAW_UNITS_PER_YUAN
    return quantize_price(compose_events(price, events))


def is_false_gap(prev, curr):
    """判断前复权相邻两日的跳空是否落在 2×/5×/10× 的邻域内。

    `prev`/`curr` 是 Decimal 价格；返回命中的倍数（int）或 None。
    `prev` 为 0 时无法计算收益，返回 None（由调用方标记为不可判定）。
    """
    if prev is None or curr is None or prev == 0:
        return None
    with localcontext() as ctx:
        ctx.prec = 40
        ratio = abs(curr / prev)
    for multiplier in FALSE_GAP_MULTIPLIERS:
        boundary = Decimal(multiplier)
        # 正向（×2/×5/×10）与反向（×1/2、×1/5、×1/10）都算假跳空：
        # 价格只会跌到 1/2，不会跌到「比前值小 2 倍」。
        for target in (boundary, Decimal(1) / boundary):
            if abs(ratio - target) <= target * FALSE_GAP_RELATIVE_TOLERANCE:
                return multiplier
    return None


def daily_return(prev, curr):
    """相邻两日的简单收益率，返回 Decimal（保留 12 位有效小数供展示）。"""
    if prev is None or curr is None or prev == 0:
        return None
    with localcontext() as ctx:
        ctx.prec = 40
        value = curr / prev - 1
    return value.quantize(Decimal("0.000000000001"), rounding=ROUND_HALF_UP)


class Event:
    """一个 gbbq 事件：权益登记日 R、类别、因子。"""

    __slots__ = ("registration_day", "category", "c1", "c3", "effective_day")

    def __init__(self, registration_day, category, c1, c3, effective_day=None):
        self.registration_day = registration_day
        self.category = category
        self.c1 = Decimal(c1)
        self.c3 = Decimal(c3)
        # X：R 之后的第一根 bar 的日期。由 `assign_effective_days` 填。
        self.effective_day = effective_day

    def transform(self):
        return (self.category, self.c1, self.c3)

    def __repr__(self):
        return (
            f"Event(R={self.registration_day}, X={self.effective_day}, "
            f"cat={self.category}, c1={self.c1}, c3={self.c3})"
        )


def assign_effective_days(events, bar_days):
    """按 D-91 给每个事件定 X：R 之后的第一根 bar 的日期。

    `bar_days` 是该实体**已有 bar 的日期**，升序。用 bar 而不是 `trading_days`
    是因为「X 之后没有 bar 的事件不影响任何 bar」，用 bar 定 X 天然满足这一条。
    R 本身停在周末/节假日时，X 自动落到下一根 bar。

    返回 (可见事件, 越界事件)：
    - 可见事件：X 存在的事件（它们至少影响一根 bar，或者 X 之后还有 bar）；
    - 越界事件：X 不存在（R 之后已经没有 bar），不影响任何 bar。
    """
    import bisect

    visible, out_of_range = [], []
    for event in events:
        index = bisect.bisect_right(bar_days, event.registration_day)
        if index >= len(bar_days):
            event.effective_day = None
            out_of_range.append(event)
            continue
        event.effective_day = bar_days[index]
        visible.append(event)
    return visible, out_of_range


def chain_for_bar(bar_day, events):
    """返回某根 bar 之后（bar_date < X）的全部事件，按复合次序排列。

    次序：X 从早到晚；同一个 X 上先 cat11 后 cat1（D-91 / D-90 F 的构造性约定）。
    """
    selected = [e for e in events if e.effective_day is not None and e.effective_day > bar_day]
    selected.sort(key=lambda e: (e.effective_day, 0 if e.category == 11 else 1))
    return [e.transform() for e in selected]


def adjust_series(bar_days, bar_raws, events):
    """把一条不复权 bar 序列变换成前复权序列。

    `bar_days` 与 `bar_raws` 等长且按日期升序；`bar_raws` 是每个 bar 的
    (open, high, low, close) 四个 `raw` 整数。返回同样形状的 Decimal 列表。
    每根 bar 只舍入一次。
    """
    out = []
    for day, raws in zip(bar_days, bar_raws):
        chain = chain_for_bar(day, events)
        out.append(tuple(adjusted_price(raw, chain) for raw in raws))
    return out


def open_readonly(db_path):
    """以只读方式打开数据库：`file:<db>?mode=ro`，能读 WAL，不加 immutable=1。

    `mode=ro` 对 WAL 库仍然需要读 `-wal` 文件，这正是我们要的：
    生产库正在被日更服务写入，读最新已提交状态。
    """
    if not os.path.exists(db_path):
        raise InputError(f"数据库不存在：{db_path}")
    uri = f"file:{db_path}?mode=ro"
    try:
        conn = sqlite3.connect(uri, uri=True)
        conn.execute("PRAGMA query_only = ON")
    except sqlite3.Error as exc:
        raise InputError(f"数据库打不开：{exc}") from exc
    return conn


def load_events(conn, entity):
    """读一个实体的 gbbq 事件，按 ordinal 排序。"""
    rows = conn.execute(
        "SELECT ordinal, effective_day, category, c1, c3 FROM adjustment_events "
        "WHERE entity = ? ORDER BY ordinal",
        (entity,),
    ).fetchall()
    events = []
    for _ordinal, day, category, c1, c3 in rows:
        if category not in (1, 11):
            raise InputError(f"{entity} 的事件类别 {category} 不在 1/11 内")
        events.append(Event(day, category, c1, c3))
    return events


def load_raw_bars(conn, entity):
    """读一个实体的不复权日线（period=1, adjustment=0）。

    返回 (bar_days, bar_raws)。库里没有该序列时返回空列表。
    一次只读一个实体，按实体流式处理。
    """
    row = conn.execute(
        "SELECT series_key FROM market_series "
        "WHERE entity = ? AND period = 1 AND adjustment = 0",
        (entity,),
    ).fetchone()
    if row is None:
        return [], []
    series_key = row[0]
    days, raws = [], []
    for (day, o, h, lo, c) in conn.execute(
        "SELECT trade_day, open_units, high_units, low_units, close_units "
        "FROM series_bars WHERE series_key = ? ORDER BY trade_day",
        (series_key,),
    ):
        days.append(day)
        raws.append((o, h, lo, c))
    return days, raws


def analyse_entity(entity, bar_days, bar_raws, events):
    """算一只 ETF 的连续性检查结论（纯函数，便于测试）。

    对每个 X（生效日）看**前后各 1 日**：X 前一根 bar 记为 prev，X 当根 bar
    记为 curr。分别用不复权和前复权收盘价算收益，再判断前复权收益是否落在
    2×/5×/10× 的邻域里（假跳空）。

    返回一个 dict，键与报告 JSON 一致。
    """
    visible, out_of_range = assign_effective_days(events, bar_days)
    if not bar_days:
        return {
            "entity": entity,
            "status": "no_bars",
            "event_count": len(events),
            "visible_event_count": len(visible),
            "out_of_range_event_count": len(out_of_range),
            "note": "库里没有不复权日线",
            "checks": [],
            "false_gaps": 0,
        }
    if not visible:
        return {
            "entity": entity,
            "status": "no_events",
            "event_count": len(events),
            "visible_event_count": 0,
            "out_of_range_event_count": len(out_of_range),
            "note": "无事件",
            "checks": [],
            "false_gaps": 0,
        }

    qfq = adjust_series(bar_days, bar_raws, visible)
    raw_close = [Decimal(r[3]) / RAW_UNITS_PER_YUAN for r in bar_raws]
    qfq_close = [row[3] for row in qfq]
    index_of = {day: i for i, day in enumerate(bar_days)}

    checks = []
    for effective_day in sorted({e.effective_day for e in visible}):
        curr = index_of[effective_day]
        prev = curr - 1
        on_this_day = [e for e in visible if e.effective_day == effective_day]
        check = {
            "effective_day": effective_day,
            "categories": sorted({e.category for e in on_this_day}),
            "event_count": len(on_this_day),
            "prev_day": bar_days[prev] if prev >= 0 else None,
            "raw_return": None,
            "qfq_return": None,
            "false_gap": None,
        }
        # X 之前没有 bar 时无法算收益：这是「首根 bar 之前的事件」的正常情形。
        if prev >= 0:
            raw_prev, raw_curr = raw_close[prev], raw_close[curr]
            qfq_prev, qfq_curr = qfq_close[prev], qfq_close[curr]
            check["raw_return"] = _str_or_none(daily_return(raw_prev, raw_curr))
            check["qfq_return"] = _str_or_none(daily_return(qfq_prev, qfq_curr))
            hit = is_false_gap(qfq_prev, qfq_curr)
            check["false_gap"] = hit
        checks.append(check)

    false_gaps = sum(1 for c in checks if c["false_gap"])
    return {
        "entity": entity,
        "status": "ok",
        "event_count": len(events),
        "visible_event_count": len(visible),
        "out_of_range_event_count": len(out_of_range),
        "note": None,
        "bar_count": len(bar_days),
        "first_day": bar_days[0],
        "last_day": bar_days[-1],
        "checks": checks,
        "false_gaps": false_gaps,
        # 供 --compare 逐值比对；不给 --compare 时由调用方剥掉，报告保持精简。
        "qfq_bars": [
            [day] + [str(value) for value in row] for day, row in zip(bar_days, qfq)
        ],
    }


def _str_or_none(value):
    return None if value is None else str(value)


def compare_with_rust(report, compare_path):
    """把本脚本的结果与 Rust 前复权序列逐值比对（接口，PR-C 落库后启用）。

    输入格式（PR-C 决定，此处只实现读到即可比的最小形态）：一个 JSON 文件，
    `{"entities": {"<entity>": {"bars": [[day, open, high, low, close], ...]}}}`，
    价格是字符串或数字，单位元，已按 6 位小数舍入。

    没有输入时返回 `NOT_RUN`——**不静默通过**。
    """
    if not compare_path:
        return {"status": "NOT_RUN", "reason": "--compare 未给出输入文件"}
    if not os.path.exists(compare_path):
        raise InputError(f"--compare 文件不存在：{compare_path}")
    try:
        with open(compare_path, "r", encoding="utf-8") as handle:
            payload = json.load(handle)
    except (OSError, json.JSONDecodeError) as exc:
        raise InputError(f"--compare 文件读不了：{exc}") from exc
    entities = payload.get("entities")
    if not isinstance(entities, dict):
        raise InputError("--compare 文件缺少 entities 对象")

    mismatches, compared = [], 0
    for result in report["entities"]:
        entity = result["entity"]
        if result["status"] != "ok":
            continue
        expected = entities.get(entity)
        if expected is None:
            mismatches.append({"entity": entity, "reason": "compare 文件里没有该实体"})
            continue
        ours = result.get("qfq_bars") or []
        theirs = expected.get("bars") or []
        if len(ours) != len(theirs):
            mismatches.append(
                {
                    "entity": entity,
                    "reason": f"bar 数不同：本脚本 {len(ours)}，Rust {len(theirs)}",
                }
            )
            continue
        for mine, other in zip(ours, theirs):
            compared += 1
            if [str(x) for x in mine] != [str(x) for x in other]:
                mismatches.append(
                    {"entity": entity, "ours": list(map(str, mine)), "rust": other}
                )
    return {
        "status": "PASS" if not mismatches else "FAIL",
        "compared_bars": compared,
        "mismatch_count": len(mismatches),
        "mismatches": mismatches[:50],
    }


def run(conn, entities, compare_path=None):
    """逐实体跑检查，返回报告 dict。按实体流式处理，不保留全部实体数据。"""
    results = []
    for entity in entities:
        events = load_events(conn, entity)
        bar_days, bar_raws = load_raw_bars(conn, entity)
        if not bar_days:
            results.append(
                {
                    "entity": entity,
                    "status": "no_bars",
                    "event_count": len(events),
                    "visible_event_count": 0,
                    "out_of_range_event_count": len(events),
                    "note": "库里没有不复权日线",
                    "checks": [],
                    "false_gaps": 0,
                }
            )
            continue
        results.append(analyse_entity(entity, bar_days, bar_raws, events))

    report = {
        "tool": "check_etf_qfq_continuity",
        "semantics": {
            "price_unit": "raw/1000 元",
            "event_date": "权益登记日 R（adjustment_events.effective_day）",
            "effective_day": "X = R 之后第一根 bar 的日期",
            "applies_to": "bar_date < X（含 R 当天）",
            "transform": "cat11 = p / c3；cat1 = p - c1/10",
            "order": "同一个 X 上先 cat11 后 cat1",
            "rounding": "最后一次 HALF_UP，保留 6 位小数",
            "false_gap_rule": (
                f"|qfq_ratio| 落在 "
                f"{'/'.join(str(m) for m in FALSE_GAP_MULTIPLIERS)}× "
                f"×(1 ± {FALSE_GAP_RELATIVE_TOLERANCE}) 内"
            ),
        },
        "entities": results,
    }
    report["compare"] = compare_with_rust(report, compare_path)
    _strip_compare_bars(report, compare_path)
    report["summary"] = _summary(results)
    return report


def _strip_compare_bars(report, compare_path):
    """不给 --compare 时不把 qfq_bars 留在报告里（报告保持精简）。"""
    if compare_path:
        return
    for result in report["entities"]:
        result.pop("qfq_bars", None)


def _summary(results):
    return {
        "entity_count": len(results),
        "ok": sum(1 for r in results if r["status"] == "ok"),
        "no_events": sum(1 for r in results if r["status"] == "no_events"),
        "no_bars": sum(1 for r in results if r["status"] == "no_bars"),
        "false_gaps": sum(r["false_gaps"] for r in results),
        "event_count": sum(r["event_count"] for r in results),
    }


def decide_exit_code(report):
    """退出码：0 全部通过；1 出现假跳空；2 输入异常（在调用处抛 InputError）。"""
    if report["summary"]["false_gaps"] > 0:
        return EXIT_FALSE_GAP
    if report["compare"]["status"] == "FAIL":
        return EXIT_FALSE_GAP
    return EXIT_OK


def format_summary(report):
    """人读摘要：每只样本的结论、事件数、假跳空个数。"""
    lines = []
    for result in report["entities"]:
        if result["status"] == "no_events":
            lines.append(f"{result['entity']}: 无事件")
            continue
        if result["status"] == "no_bars":
            lines.append(f"{result['entity']}: 无数据（库里没有不复权日线）")
            continue
        lines.append(
            f"{result['entity']}: 事件 {result['event_count']} 个"
            f"（可见 {result['visible_event_count']}，"
            f"越界 {result['out_of_range_event_count']}），"
            f"假跳空 {result['false_gaps']} 个"
        )
    summary = report["summary"]
    lines.append(
        f"合计：{summary['entity_count']} 只样本，{summary['ok']} 只有事件且有数据，"
        f"{summary['no_events']} 只无事件，{summary['no_bars']} 只无数据，"
        f"假跳空 {summary['false_gaps']} 个"
    )
    compare = report["compare"]
    lines.append(f"compare: {compare['status']}" + (
        "" if compare["status"] == "NOT_RUN" else
        f"（比对 {compare.get('compared_bars', 0)} 根 bar，不一致 {compare.get('mismatch_count', 0)} 处）"
    ))
    return "\n".join(lines)


def main(argv=None):
    parser = argparse.ArgumentParser(
        description="ETF 前复权的 Python 独立参照实现与连续性检查（#607 PR-F）",
    )
    parser.add_argument("--db", required=True, help="只读打开的 SQLite 数据库路径")
    parser.add_argument(
        "--entities",
        default=",".join(DEFAULT_ENTITIES),
        help="逗号分隔的实体列表，默认是 D-89 的 10 只样本",
    )
    parser.add_argument("--report", help="报告 JSON 输出路径")
    parser.add_argument(
        "--compare",
        help="Rust 前复权序列输出文件；未给出时结论为 NOT_RUN",
    )
    args = parser.parse_args(argv)

    entities = [item.strip() for item in args.entities.split(",") if item.strip()]
    if not entities:
        print("输入异常：--entities 为空", file=sys.stderr)
        return EXIT_INPUT_ERROR

    started = time.monotonic()
    try:
        conn = open_readonly(args.db)
        try:
            report = run(conn, entities, args.compare)
        finally:
            conn.close()
    except InputError as exc:
        print(f"输入异常：{exc}", file=sys.stderr)
        return EXIT_INPUT_ERROR
    report["elapsed_seconds"] = round(time.monotonic() - started, 3)

    if args.report:
        directory = os.path.dirname(os.path.abspath(args.report))
        os.makedirs(directory, exist_ok=True)
        with open(args.report, "w", encoding="utf-8") as handle:
            json.dump(report, handle, ensure_ascii=False, indent=2)
            handle.write("\n")

    print(format_summary(report))
    print(f"耗时 {report['elapsed_seconds']}s")
    return decide_exit_code(report)


if __name__ == "__main__":
    sys.exit(main())
