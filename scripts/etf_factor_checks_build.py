"""(#607) 公告取证结果 → `etf-factor-checks-v1` 记录（首次导入用）。

输入是阶段 1 取证表 `events-v3.csv`（VM2-23 的产物，1,233 行 = 979 cat1 + 254
cat11），加一个**只读**打开的行情库；输出是 `source_store import --format
etf-factor-checks-jsonl` 原样接受的 JSONL，以及一份供复核的 JSON 报告。

## 三条必须记住的事实

1. **真正的除权除息日 E 是 `X_day` 列。** `events-v3.csv` 的 `day` 列按
   `1969-12-31` 解码，比 E 早一天（取证脚本的日期基准错）。所以匹配键是
   库里的 `(entity, effective_day, category)` ↔ v3 的 `(entity, X_day, cat)`；
   `ordinal` 与 `evidence_digest` 一律从库里读，不从 v3 猜。
   `X_day` 为空（未来事件，库里还没有 E 那根 bar）时用 `day + 1` 兜底，并在
   报告的 `warnings.v3_missing_x_day` 里列出来。
2. **库里的每个 cat1/cat11 事件恰好一条记录。** 对不上的（v3 没有、库里没有、
   匹配到多条、类别不一致）全部进报告的 `unmatched`，一条都不静默跳过；被
   `unmatched` 命中的实体在报告里同时计入「被阻断」。
3. **`check_status` 只按证据判，不按期望判。**
   - `verified_official`：官方核对，`basis=official_qss`，证据是 E 当天官方
     `qss`；
   - `verified_announcement`：公告核对，`basis=announcement`，证据是公告 URL
     与其字节 sha256；
   - `no_effect`：原样保留（首根 bar 之前 / 未来事件，不影响序列），
     `basis=none`；
   - `unverified`：按 **D-92** 再判一次——用 gbbq 因子算出的复权价与用公告值
     算出的复权价相差 **≤ 0.001 元**（1 tick）就记 `verified_announcement`，
     否则保持 `unverified`。每条的差值写进 `price_delta`，`basis_ref` 里写明
     适用的是 D-92。
   - 同一个 E 上的复合事件（`sz.159922` 2024-12-02 的 cat11 `c3=2.5` + cat1
     `c1=1.292`）必须**合并**核对：先 cat11 后 cat1 得
     `6.159 / 2.5 − 0.1292 = 2.3344`，与官方 `qss` 2.334 相差 0.0004，所以两条
     都记 `verified_official`，`basis_ref` 注明「同 E 复合」。

## 记录字段的口径（对齐 `src/source_store/etf_factor_checks.rs`）

- `gbbq_value`：库里的 `c1`（cat1，元/10 份）或 `c3`（cat11，折算比），逐字保留。
- `announced_value`：公告值（cat1 取 `ann_amount_per10`，没有则取 `ann_amount`；
  cat11 取 `ann_ratio`），官方核对的记录取 E 当天官方 `qss`。
- `price_delta` / `within_one_tick`：**判定所依据的那个价差**。
  - 官方核对：`qss − 用 gbbq 因子算出的复权价`；
  - D-92 复判：`用 gbbq 因子算出的复权价 − 用公告值算出的复权价`；
  - 公告核对（v3 已判 `verified_announcement` 的那 987 条）：留空——判定依据是
    公告的金额与日期逐项一致，不是一个价差；留空比填一个不参与判定的数字更
    不容易误读。
- `evidence_sha256`：公告记录是**公告 PDF 字节**的 sha256（v3 的 `ann_sha256`）。
  官方记录是**转换时消费的官方证据文本** `entity|E|qss|ks` 的 sha256：行情库里
  没有冻结的官方行字节，脚本不伪造 HTTP 响应体的摘要，这一点写在报告的
  `evidence_note` 里。PR-B 的字段校验只要求 64 位 hex。
- `checked_at`：核对日期，默认今天（UTC），可用 `--checked-at` 固定，保证复跑
  逐位可复现（记录 id 是内容的 sha256，日期变了 id 就变）。

## 只读与边界

- 行情库用 `file:<db>?mode=ro` 打开（能读 WAL，不加 `immutable=1`），不复制、
  不写；只读 `market_series`（挑 `instrument_class=2` 的 ETF）与
  `adjustment_events`。
- **不写任何来源库**。真正的导入是部署后在 vm2-126 上另派的运维任务，要 owner
  批准；本脚本只生成文件。
- 输出前用 `validate_record()` 逐条过一遍 PR-B 的校验规则（与 Rust 的
  `EtfFactorCheck::validate` 逐条对应），不过就不写文件。

## 退出码

- `0`：转换完成，全部记录通过校验，且没有 `unmatched`。
- `2`：输入异常（数据库打不开、CSV 缺列、日期不合法等）。
- `3`：有 `unmatched` 或有记录没过校验（报告照写，供人复核）。加
  `--allow-unmatched` 可把 `unmatched` 降为 0 退出码（记录缺失仍写进报告，
  对应实体会被 D-92 的缺记录规则阻断）。
"""

import argparse
import csv
import hashlib
import json
import os
import re
import sqlite3
import sys
from collections import defaultdict
from datetime import date, datetime, timedelta, timezone
from decimal import Decimal, InvalidOperation, localcontext

#: 记录版本（PR-B `CHECKS_VERSION`）。
RECORD_VERSION = "etf-factor-checks-v1"

#: D-92 的 1 tick 门槛：复权价相差 ≤ 0.001 元即视为核对通过。
ONE_TICK = Decimal("0.001")

#: 库里 `effective_day` / `series_bars.trade_day` 的基准（`src/compact_market.rs`）。
EPOCH = date(1970, 1, 1)

#: `market_series.instrument_class` 的 ETF 取值（`src/market_data.rs`）。
ETF_INSTRUMENT_CLASS = 2

#: PR-B 允许的 `check_status` / `basis`。
CHECK_STATUSES = ("verified_official", "verified_announcement", "no_effect", "unverified")
BASES = ("official_qss", "announcement", "none")

#: 规范 JSON 的字段顺序（= Rust `EtfFactorCheck` 的声明顺序，id 是它的 sha256）。
CANONICAL_FIELDS = (
    "record_version",
    "entity",
    "ordinal",
    "effective_day",
    "category",
    "check_status",
    "basis",
    "basis_ref",
    "evidence_url",
    "evidence_sha256",
    "announced_value",
    "gbbq_value",
    "price_delta",
    "within_one_tick",
    "checked_at",
    "evidence_digest",
)

#: 官方 `qss` 证据的来源标签与可复跑的请求 URL。
OFFICIAL_SOURCES = {
    "sz": (
        "szse_1815_stock",
        "https://www.szse.cn/api/report/ShowReport/data"
        "?SHOWTYPE=json&CATALOGID=1815_stock&TABKEY=tab2&txtQueryDate={ex_day}",
    ),
}

#: 公告来源前缀（`basis_ref` 的可读标签）。
ANNOUNCEMENT_SOURCES = {"sh": "sse_ann", "sz": "szse_ann", "bj": "bse_ann"}

EVIDENCE_NOTE = (
    "官方记录的 evidence_sha256 是转换时消费的官方证据文本 "
    "'entity|E|qss|ks' 的 sha256（行情库里没有冻结的官方行字节）；"
    "公告记录的是公告 PDF 字节的 sha256（events-v3 的 ann_sha256）。"
)

EXIT_OK = 0
EXIT_INPUT_ERROR = 2
EXIT_NEEDS_REVIEW = 3

ENTITY_RE = re.compile(r"^(sh|sz|bj)\.(\d{6})$")
DECIMAL_RE = re.compile(r"^[+-]?(\d+(\.\d*)?|\.\d+)$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")


class InputError(Exception):
    """输入异常：退出码 2。"""


# ---------------------------------------------------------------------------
# 小工具
# ---------------------------------------------------------------------------


def parse_day(text):
    """`YYYY-MM-DD` → `date`；不合法返回 None（不做任何猜测）。"""
    value = (text or "").strip()
    if not value:
        return None
    try:
        parsed = datetime.strptime(value, "%Y-%m-%d").date()
    except ValueError:
        return None
    if parsed.isoformat() != value:
        return None
    return parsed


def day_text(day):
    """`date` 或 epoch 日序号 → `YYYY-MM-DD`。"""
    if isinstance(day, date):
        return day.isoformat()
    return (EPOCH + timedelta(days=int(day))).isoformat()


def epoch_day(text):
    """`YYYY-MM-DD` → 库里的 epoch 日序号。"""
    day = parse_day(text)
    if day is None:
        raise InputError(f"不是合法日期：{text!r}")
    return (day - EPOCH).days


def sha256_hex(data):
    return hashlib.sha256(data).hexdigest()


#: 价差写进记录时的最大小数位。rust_decimal 的 scale 上限是 28，而
#: `a / c3` 的精确差值可能长得多；12 位远低于 1 tick（0.001），不影响判定。
DELTA_SCALE = Decimal("0.000000000001")


def decimal_text(value, quantize=None):
    """Decimal → 非科学计数法的文本；0 一律写 `0`（rust_decimal 不认 `0E-7`）。

    `quantize` 给定时先按 HALF_UP 收到那个位数（记录里的 `price_delta` 用它，
    避免把 35 位小数的除法余数写进 `rust_decimal` 解析不了的长度）。
    """
    if quantize is not None:
        value = value.quantize(quantize)
    if value == 0:
        return "0"
    text = format(value, "f")
    if "." in text:
        text = text.rstrip("0").rstrip(".")
    return text


def parse_decimal(value, what):
    """`value` 是空串时返回 None，否则必须是十进制数。"""
    text = (value or "").strip()
    if not text:
        return None
    try:
        parsed = Decimal(text)
    except InvalidOperation as exc:
        raise InputError(f"{what} 不是十进制数：{text!r}") from exc
    if not parsed.is_finite():
        raise InputError(f"{what} 不是有限数：{text!r}")
    return parsed


def market_of(entity):
    return entity.split(".", 1)[0] if "." in entity else ""


# ---------------------------------------------------------------------------
# 输入 1：events-v3.csv
# ---------------------------------------------------------------------------


def load_events_v3(path):
    """读取证表，给每行算出 E（除权除息日）的候选值。

    返回 `(rows, warnings)`；每行是一个 dict，键与 v3 的列同名，另加：

    - `ex_day_candidates`：E 的候选，按可信度排序——`X_day` 优先，其次
      `day + 1`（v3 的 `day` 按 `1969-12-31` 解码，比 E 早一天）；
    - `ex_day`：第一个候选（只用于报告与证据标签，记录里的 `effective_day`
      一律取库里的 `adjustment_events.effective_day`）；
    - `market`、`line`、`category`（int）。

    为什么需要两个候选：`X_day` 在两种情形下不等于 E——**首根 bar 之前**的事件
    （75 条）的 `X_day` 是该实体的第一根 bar，不是除权日；`day` 落在周末且下一个
    交易日隔了一天的（`sz.159922` 2024-11-30 周六 → E 2024-12-02）用 `day + 1`
    会偏一天。脚本不猜：两个候选都拿去和库里比，只有一个命中才算匹配。
    """
    if not os.path.exists(path):
        raise InputError(f"events-v3 不存在：{path}")
    rows = []
    warnings = {
        "v3_x_day_not_day_plus_one": [],
        "v3_missing_x_day": [],
    }
    with open(path, newline="", encoding="utf-8") as handle:
        reader = csv.DictReader(handle)
        if reader.fieldnames is None:
            raise InputError(f"{path}: CSV 没有表头")
        for column in ("entity", "cat", "final_status"):
            if column not in reader.fieldnames:
                raise InputError(f"{path}: 缺少列 {column}")
        if "X_day" not in reader.fieldnames and "day" not in reader.fieldnames:
            raise InputError(f"{path}: 既没有 X_day 也没有 day 列")
        for line, raw in enumerate(reader, start=2):
            row = {key: (value or "").strip() for key, value in raw.items() if key}
            entity = row.get("entity", "")
            if not ENTITY_RE.match(entity):
                raise InputError(f"{path}:{line}: entity {entity!r} 不是 <sh|sz|bj>.<6 位>")
            category_text = row.get("cat", "")
            if category_text not in ("1", "11"):
                raise InputError(f"{path}:{line}: cat {category_text!r} 不是 1 或 11")
            row["category"] = int(category_text)
            row["market"] = market_of(entity)
            row["line"] = line
            if row.get("final_status", "") not in CHECK_STATUSES:
                raise InputError(
                    f"{path}:{line}: final_status {row.get('final_status')!r} 不是 "
                    + "|".join(CHECK_STATUSES)
                )
            day = parse_day(row.get("day", ""))
            x_day = parse_day(row.get("X_day", ""))
            if x_day is None and row.get("X_day", ""):
                raise InputError(f"{path}:{line}: X_day {row['X_day']!r} 不是 YYYY-MM-DD")
            candidates = []
            if x_day is not None:
                candidates.append(x_day.isoformat())
            if day is not None:
                plus_one = (day + timedelta(days=1)).isoformat()
                if plus_one not in candidates:
                    candidates.append(plus_one)
            if not candidates:
                raise InputError(f"{path}:{line}: X_day 与 day 都为空或不可解析")
            row["ex_day_candidates"] = candidates
            row["ex_day"] = candidates[0]
            row["day_plus_one"] = (
                (day + timedelta(days=1)).isoformat() if day is not None else ""
            )
            if x_day is None:
                warnings["v3_missing_x_day"].append(
                    {
                        "entity": entity,
                        "category": row["category"],
                        "day": row.get("day", ""),
                        "ex_day_candidates": candidates,
                        "final_status": row.get("final_status", ""),
                    }
                )
            elif day is not None and (x_day - day).days != 1:
                warnings["v3_x_day_not_day_plus_one"].append(
                    {
                        "entity": entity,
                        "category": row["category"],
                        "day": row.get("day", ""),
                        "X_day": row.get("X_day", ""),
                        "day_plus_one": row["day_plus_one"],
                        "attribution_v2": row.get("attribution_v2", ""),
                        "final_status": row.get("final_status", ""),
                    }
                )
            rows.append(row)
    return rows, warnings


# ---------------------------------------------------------------------------
# 输入 2：行情库（只读）
# ---------------------------------------------------------------------------


def open_read_only(db_path):
    """以只读方式打开行情库：`file:<db>?mode=ro`，能读 WAL，不复制。"""
    if not os.path.exists(db_path):
        raise InputError(f"数据库不存在：{db_path}")
    uri = f"file:{db_path}?mode=ro"
    try:
        conn = sqlite3.connect(uri, uri=True)
        conn.execute("PRAGMA query_only = ON")
    except sqlite3.Error as exc:
        raise InputError(f"数据库打不开：{exc}") from exc
    return conn


def _columns(conn, table):
    return {row[1] for row in conn.execute(f"PRAGMA table_info({table})")}


def read_db_events(conn):
    """读 ETF 的 cat1/cat11 事件。

    返回 `(etf_entities, columns, selected)`：`etf_entities` 是 `market_series`
    里 `instrument_class=2` 的实体（表不存在时为空集），`selected` 是
    `adjustment_events` 里要读的列（`basis_status` 存在时才读）。事件本身由
    [`fetch_events`] 读，这样测试可以把同一套逻辑套在内存夹具上。
    """
    tables = {
        row[0]
        for row in conn.execute("SELECT name FROM sqlite_master WHERE type='table'")
    }
    if "adjustment_events" not in tables:
        raise InputError("库里没有 adjustment_events 表（schema 太旧或不是行情库）")
    etf_entities = set()
    if "market_series" in tables:
        etf_entities = {
            row[0]
            for row in conn.execute(
                "SELECT DISTINCT entity FROM market_series WHERE instrument_class = ?",
                (ETF_INSTRUMENT_CLASS,),
            )
        }
    columns = _columns(conn, "adjustment_events")
    for column in ("entity", "ordinal", "effective_day", "category", "c1", "c3", "evidence_digest"):
        if column not in columns:
            raise InputError(f"adjustment_events 缺少列 {column}")
    selected = ["entity", "ordinal", "effective_day", "category", "c1", "c3", "evidence_digest"]
    if "basis_status" in columns:
        selected.append("basis_status")
    return etf_entities, columns, selected


#: 一条 SELECT 里最多绑定的实体数（SQLite 的绑定变量上限是 999）。
ENTITY_BATCH = 400


def fetch_events(conn, selected, entities):
    """按实体集合读事件行（只读；category 只取 1/11，按实体分块查询）。"""
    ordered = sorted(entities)
    events = []
    for start in range(0, len(ordered), ENTITY_BATCH):
        batch = ordered[start : start + ENTITY_BATCH]
        placeholders = ",".join("?" for _ in batch)
        statement = (
            f"SELECT {', '.join(selected)} FROM adjustment_events "
            f"WHERE category IN (1,11) AND entity IN ({placeholders}) "
            "ORDER BY entity, effective_day, ordinal"
        )
        for row in conn.execute(statement, tuple(batch)):
            record = dict(zip(selected, row))
            digest = record["evidence_digest"]
            if isinstance(digest, memoryview):
                digest = digest.tobytes()
            if isinstance(digest, bytes):
                if len(digest) != 32:
                    raise InputError(
                        f"{record['entity']}#{record['ordinal']}: "
                        "evidence_digest 不是 32 字节"
                    )
                digest = digest.hex()
            record["evidence_digest"] = str(digest).lower()
            record["effective_day"] = day_text(record["effective_day"])
            record["ordinal"] = int(record["ordinal"])
            record["category"] = int(record["category"])
            record["c1"] = str(record["c1"])
            record["c3"] = str(record["c3"])
            events.append(record)
    events.sort(key=lambda item: (item["entity"], item["effective_day"], item["ordinal"]))
    return events


# ---------------------------------------------------------------------------
# 匹配
# ---------------------------------------------------------------------------


def event_key(event):
    return (event["entity"], event["effective_day"], event["category"])


def match_events(db_events, v3_rows):
    """按 `(entity, E, category)` 一一配对；对不上的全部列出。

    E 用 v3 行的候选（`X_day`、`day + 1`）去库里找，只有一个候选命中才算匹配；
    两个候选都命中、库里有多条同键事件、v3 有重复行、库里没有对应事件、类别
    不一致，全部进 `unmatched`，一条都不静默跳过。

    返回 `(pairs, unmatched)`，`pairs` 是 `db_key -> v3 row`。
    """
    db_by_key = defaultdict(list)
    for event in db_events:
        db_by_key[event_key(event)].append(event)
    unmatched = []
    claims = defaultdict(list)
    for row in v3_rows:
        entity = row["entity"]
        category = row["category"]
        hits = [
            (entity, candidate, category)
            for candidate in row["ex_day_candidates"]
            if (entity, candidate, category) in db_by_key
        ]
        if len(hits) > 1:
            unmatched.append(
                {
                    "entity": entity,
                    "category": category,
                    "effective_day": row["ex_day"],
                    "reason": "ambiguous_ex_day",
                    "detail": "两个候选 E 都在库里命中："
                    + ",".join(hit[1] for hit in hits),
                    "v3_line": row["line"],
                    "v3_final_status": row["final_status"],
                }
            )
            continue
        if not hits:
            other = sorted(
                other_day
                for (other_entity, other_day, other_category) in db_by_key
                if other_entity == entity
                and other_category != category
                and other_day in row["ex_day_candidates"]
            )
            if other:
                reason = "category_mismatch"
                detail = f"库里同一 E 上的类别不是 {category}（{other}）"
            else:
                reason = "no_db_event"
                detail = "库里没有这个 (entity, E, category) 事件，候选 E=" + ",".join(
                    row["ex_day_candidates"]
                )
            unmatched.append(
                {
                    "entity": entity,
                    "category": category,
                    "effective_day": row["ex_day"],
                    "reason": reason,
                    "detail": detail,
                    "v3_line": row["line"],
                    "v3_final_status": row["final_status"],
                }
            )
            continue
        claims[hits[0]].append(row)

    pairs = {}
    ambiguous = set()
    for key, rows in sorted(claims.items()):
        entity, ex_day, category = key
        if len(rows) > 1:
            for row in rows:
                unmatched.append(
                    {
                        "entity": entity,
                        "category": category,
                        "effective_day": ex_day,
                        "reason": "duplicate_v3_rows",
                        "detail": "v3 有 "
                        + str(len(rows))
                        + " 行指向同一事件（line "
                        + ",".join(str(item["line"]) for item in rows)
                        + "）",
                    }
                )
            continue
        events = db_by_key[key]
        if len(events) > 1:
            ambiguous.add(key)
            unmatched.append(
                {
                    "entity": entity,
                    "category": category,
                    "effective_day": ex_day,
                    "reason": "multiple_db_events",
                    "detail": "库里有多个同 (entity, E, category) 事件，ordinal="
                    + ",".join(str(item["ordinal"]) for item in events),
                    "v3_line": rows[0]["line"],
                }
            )
            continue
        pairs[key] = rows[0]

    for key, events in sorted(db_by_key.items()):
        if key in pairs or key in ambiguous:
            continue
        entity, ex_day, category = key
        if key in claims:
            reason = "multiple_v3_rows"
            detail = "v3 有重复行，库里这条事件无法唯一对应"
        else:
            reason = "no_v3_row"
            detail = "v3 里没有这个事件"
        for event in events:
            unmatched.append(
                {
                    "entity": entity,
                    "category": category,
                    "effective_day": ex_day,
                    "reason": reason,
                    "detail": detail,
                    "ordinal": event["ordinal"],
                }
            )
    return pairs, unmatched


# ---------------------------------------------------------------------------
# 价差与 D-92
# ---------------------------------------------------------------------------


def gbbq_factor(event):
    """库里事件 → (category, c1, c3)，非 1/11 或不可解析时返回 None。"""
    category = event["category"]
    try:
        c1 = Decimal(event["c1"] or "0")
        c3 = Decimal(event["c3"] or "0")
    except InvalidOperation:
        return None
    if not c1.is_finite() or not c3.is_finite():
        return None
    if category == 1:
        return (1, c1, Decimal(0))
    if category == 11:
        if c3 == 0:
            return None
        return (11, Decimal(0), c3)
    return None


def announced_factor(row):
    """v3 行 → 公告给出的 (category, c1, c3)；公告没有可用的数值时返回 None。

    cat1 用 `ann_amount_per10`（元/10 份，与 c1 同口径），没有时用 `ann_amount`；
    cat11 用 `ann_ratio`（折算比，`新价 = 旧价 / ratio`）。
    """
    category = row["category"]
    if category == 1:
        text = row.get("ann_amount_per10") or row.get("ann_amount") or ""
        value = parse_decimal(text, "ann_amount")
        if value is None:
            return None
        return (1, value, Decimal(0))
    if category == 11:
        value = parse_decimal(row.get("ann_ratio") or "", "ann_ratio")
        if value is None or value == 0:
            return None
        return (11, Decimal(0), value)
    return None


def compose_price(price, factors):
    """把若干因子套到价格上（同一 E 先 cat11 后 cat1，中间不舍入）。

    `factors` 是 `(category, c1, c3)` 的序列；返回**未舍入**的精确值。
    """
    value = Decimal(price)
    with localcontext() as context:
        context.prec = 40
        ordered = [item for item in factors if item[0] == 11] + [
            item for item in factors if item[0] == 1
        ]
        for category, c1, c3 in ordered:
            if category == 11:
                value = value / c3
            elif category == 1:
                value = value - c1 / 10
            else:
                raise InputError(f"不支持的事件类别 {category!r}（只支持 1 和 11）")
        return +value


def within_one_tick(delta):
    """D-92 的 1 tick 判定：|差值| ≤ 0.001 元。"""
    return abs(delta) <= ONE_TICK


def official_evidence(row, effective_day):
    """官方 `qss` 证据 → `(basis_ref 前缀, evidence_url, evidence_sha256)`。

    `effective_day` 是库里的 E（不是 v3 的 `X_day` 猜测）。只有深市有官方日行情
    （`1815_stock`）；其它市场返回 None，调用方据此 fail-closed 成
    `unverified`，不编造来源。
    """
    source = OFFICIAL_SOURCES.get(row["market"])
    if source is None:
        return None
    label, url_template = source
    text = "{}|{}|qss={}|ks={}".format(
        row["entity"], effective_day, row.get("official_qss", ""), row.get("official_ks", "")
    )
    return (
        f"{label}:{effective_day}",
        url_template.format(ex_day=effective_day),
        sha256_hex(text.encode("utf-8")),
    )


def announcement_evidence(row):
    """公告证据 → `(basis_ref 前缀, evidence_url, evidence_sha256)`；没有公告时 None。"""
    url = row.get("ann_url", "")
    sha = row.get("ann_sha256", "").lower()
    if not url or not SHA256_RE.match(sha):
        return None
    label = ANNOUNCEMENT_SOURCES.get(row["market"], "ann")
    published = row.get("ann_publish", "") or row["ex_day"]
    return (f"{label}:{published}", url, sha)


# ---------------------------------------------------------------------------
# 记录构造
# ---------------------------------------------------------------------------


def _record(entity, ordinal, effective_day, category, check_status, basis, checked_at, digest):
    return {
        "record_version": RECORD_VERSION,
        "entity": entity,
        "ordinal": ordinal,
        "effective_day": effective_day,
        "category": category,
        "check_status": check_status,
        "basis": basis,
        "basis_ref": "",
        "evidence_url": "",
        "evidence_sha256": "",
        "announced_value": "",
        "gbbq_value": "",
        "price_delta": "",
        "within_one_tick": None,
        "checked_at": checked_at,
        "evidence_digest": digest,
    }


def canonical_json(record):
    """规范 JSON：紧凑、键顺序固定（与 Rust `canonical_json` 同形）。"""
    ordered = {field: record[field] for field in CANONICAL_FIELDS}
    return json.dumps(ordered, ensure_ascii=False, separators=(",", ":"))


def record_id(record):
    return sha256_hex(canonical_json(record).encode("utf-8"))


def validate_record(record):
    """PR-B `EtfFactorCheck::validate` 的 Python 复刻；返回错误列表（空 = 通过）。

    规则与 `src/source_store/etf_factor_checks.rs` 逐条对应，任何一条不符就
    不写文件：记录宁可少一条，也不能变成「看起来是证据」的坏行。
    """
    errors = []

    def fail(message):
        errors.append(message)

    if record.get("record_version") != RECORD_VERSION:
        fail(f"record_version {record.get('record_version')!r} 不是 {RECORD_VERSION}")
    if not ENTITY_RE.match(record.get("entity") or ""):
        fail(f"entity {record.get('entity')!r} 不是 <sh|sz|bj>.<6 位>")
    ordinal = record.get("ordinal")
    if not isinstance(ordinal, int) or isinstance(ordinal, bool) or ordinal < 0:
        fail("ordinal 必须是 >= 0 的整数")
    if record.get("category") not in (1, 11):
        fail(f"category {record.get('category')!r} 不是 1 或 11")
    if parse_day(record.get("effective_day")) is None:
        fail(f"effective_day {record.get('effective_day')!r} 不是 YYYY-MM-DD")
    if parse_day(record.get("checked_at")) is None:
        fail(f"checked_at {record.get('checked_at')!r} 不是 YYYY-MM-DD")
    status = record.get("check_status")
    basis = record.get("basis")
    if status not in CHECK_STATUSES:
        fail(f"check_status {status!r} 不是 " + "|".join(CHECK_STATUSES))
    if basis not in BASES:
        fail(f"basis {basis!r} 不是 " + "|".join(BASES))
    if status in ("verified_official", "verified_announcement") and basis == "none":
        fail("verified 记录必须有 basis（official_qss|announcement）")
    if basis != "none":
        if not (record.get("basis_ref") or "").strip():
            fail("有 basis 时 basis_ref 必填")
        if not (record.get("evidence_url") or "").strip():
            fail("有 basis 时 evidence_url 必填")
        if not SHA256_RE.match(record.get("evidence_sha256") or ""):
            fail("有 basis 时 evidence_sha256 必须是 64 位小写 hex")
    elif (record.get("evidence_sha256") or "").strip():
        fail("basis=none 时 evidence_sha256 必须为空")
    allowed = {
        ("verified_official", "official_qss"),
        ("verified_announcement", "announcement"),
    }
    if status in ("verified_official", "verified_announcement") and (status, basis) not in allowed:
        fail(f"{status} 不能用 basis {basis}")
    for field in ("gbbq_value", "announced_value", "price_delta"):
        text = (record.get(field) or "").strip()
        if text and not DECIMAL_RE.match(text):
            fail(f"{field} 不是十进制数：{text!r}")
    if record.get("within_one_tick") is not None and not (record.get("price_delta") or "").strip():
        fail("within_one_tick 需要 price_delta")
    if not SHA256_RE.match(record.get("evidence_digest") or ""):
        fail("evidence_digest 必须是 64 位小写 hex")
    return errors


def _same_ex_day_groups(events):
    """同一个 E 上的事件组（复合事件用）。"""
    grouped = defaultdict(list)
    for event in events:
        grouped[(event["entity"], event["effective_day"])].append(event)
    for group in grouped.values():
        group.sort(key=lambda item: (item["category"] != 11, item["ordinal"]))
    return grouped


def _evidence_fields(record, evidence):
    if evidence is None:
        return
    label, url, sha = evidence
    record["basis_ref"] = label
    record["evidence_url"] = url
    record["evidence_sha256"] = sha


def build_records(db_events, v3_rows, checked_at):
    """把库事件 + v3 行转成记录，并汇总报告。

    返回 `(records, report)`；`records` 按 `(entity, effective_day, category,
    ordinal)` 排序，`report` 是可以直接写文件的 dict。
    """
    pairs, unmatched = match_events(db_events, v3_rows)
    groups = _same_ex_day_groups(db_events)
    warnings = {
        "official_delta_over_tick": [],
        "no_prev_close": [],
        "pending_basis_events": [],
        "official_source_unknown": [],
        "entity_not_etf_in_db": [],
        "v3_matched_by_day_plus_one": [],
        "v3_matched_by_x_day": 0,
    }
    for (entity, effective_day, category), row in sorted(pairs.items()):
        if row["ex_day"] == effective_day:
            warnings["v3_matched_by_x_day"] += 1
            continue
        # `X_day` 不是 E（首根 bar 之前、未来事件、周末登记日）：按 `day+1` 命中。
        warnings["v3_matched_by_day_plus_one"].append(
            {
                "entity": entity,
                "category": category,
                "effective_day": effective_day,
                "v3_day": row.get("day", ""),
                "v3_ex_day": row["ex_day"],
                "v3_line": row["line"],
            }
        )
    results = []

    for event in db_events:
        key = event_key(event)
        row = pairs.get(key)
        if row is None:
            continue
        record = _record(
            event["entity"],
            event["ordinal"],
            event["effective_day"],
            event["category"],
            "unverified",
            "none",
            checked_at,
            event["evidence_digest"],
        )
        factor = gbbq_factor(event)
        record["gbbq_value"] = event["c1"] if event["category"] == 1 else event["c3"]
        if (event.get("basis_status") or "verified") != "verified":
            warnings["pending_basis_events"].append(
                {
                    "entity": event["entity"],
                    "ordinal": event["ordinal"],
                    "effective_day": event["effective_day"],
                    "basis_status": event.get("basis_status"),
                }
            )
        group = groups[(event["entity"], event["effective_day"])]
        group_rows = [pairs.get(event_key(item)) for item in group]
        group_events = [
            item
            for item, item_row in zip(group, group_rows)
            if item_row is not None and item_row["final_status"] != "no_effect"
        ]
        reason = _decide(
            event,
            row,
            record,
            factor,
            group_events,
            group_rows,
            warnings,
        )
        results.append(
            {
                "record": record,
                "v3_row": row,
                "db_event": event,
                "reason": reason,
            }
        )

    records = [item["record"] for item in results]
    records.sort(
        key=lambda item: (item["entity"], item["effective_day"], item["category"], item["ordinal"])
    )
    report = _report(records, results, db_events, v3_rows, pairs, unmatched, warnings, checked_at)
    return records, report


def _decide(event, row, record, factor, group_events, group_rows, warnings):
    """填一条记录的判定字段；返回给人看的原因（写进报告的 D-92 明细）。"""
    status = row["final_status"]
    prev_close = parse_decimal(row.get("prev_close", ""), "prev_close")

    # 1) 不影响序列的事件原样保留（D-91 §3：首根 bar 之前 / 未来事件）。
    if status == "no_effect":
        record["check_status"] = "no_effect"
        record["basis"] = "none"
        record["basis_ref"] = "no_effect:" + (row.get("attribution_v2") or "unknown")
        return "no_effect:" + (row.get("attribution_v2") or "unknown")

    # 2) 同一个 E 上的复合事件：必须合并核对（D-91 §2，先 cat11 后 cat1）。
    if len(group_events) > 1:
        return _decide_composite(
            event, row, record, group_events, group_rows, prev_close, warnings
        )

    # 3) 单事件：官方核对。
    if status == "verified_official":
        official = parse_decimal(row.get("official_qss", ""), "official_qss")
        evidence = official_evidence(row, event["effective_day"])
        if official is None or evidence is None:
            record["check_status"] = "unverified"
            record["basis"] = "none"
            record["basis_ref"] = "official_qss_missing"
            warnings["official_source_unknown"].append(
                {"entity": event["entity"], "effective_day": event["effective_day"]}
            )
            return "official_qss 缺失或该市场没有官方来源"
        record["basis"] = "official_qss"
        _evidence_fields(record, evidence)
        record["announced_value"] = row.get("official_qss", "")
        # 自检：库里的因子和取证时的证据必须对得上，否则不能算「官方核对通过」。
        if factor is None or prev_close is None:
            record["check_status"] = "unverified"
            record["basis_ref"] += ";no_prev_close"
            warnings["no_prev_close"].append(
                {
                    "entity": event["entity"],
                    "ordinal": event["ordinal"],
                    "effective_day": event["effective_day"],
                    "reason": "官方核对算不出价差（因子或前收缺失）",
                }
            )
            return "官方 qss 核对（因子或前收缺失，降为 unverified）"
        delta = official - compose_price(prev_close, [factor])
        record["price_delta"] = decimal_text(delta, DELTA_SCALE)
        record["within_one_tick"] = within_one_tick(delta)
        if not record["within_one_tick"]:
            record["check_status"] = "unverified"
            record["basis_ref"] += ";delta_over_tick"
            warnings["official_delta_over_tick"].append(
                {
                    "entity": event["entity"],
                    "ordinal": event["ordinal"],
                    "effective_day": event["effective_day"],
                    "price_delta": record["price_delta"],
                }
            )
            return "官方 qss 核对（价差超 1 tick，降为 unverified）"
        record["check_status"] = "verified_official"
        return "官方 qss 核对（v3 verified_official）"

    # 4) 单事件：公告核对（v3 已判）。
    if status == "verified_announcement":
        evidence = announcement_evidence(row)
        if evidence is None:
            record["check_status"] = "unverified"
            record["basis"] = "none"
            record["basis_ref"] = "announcement_missing"
            return "v3 判 verified_announcement 但没有公告 URL/sha256"
        record["check_status"] = "verified_announcement"
        record["basis"] = "announcement"
        _evidence_fields(record, evidence)
        record["announced_value"] = _announced_text(row)
        return "公告核对（v3 verified_announcement）"

    # 5) v3 的 unverified：按 D-92 的 1 tick 再判一次。
    evidence = announcement_evidence(row)
    _evidence_fields(record, evidence)
    announced = announced_factor(row)
    record["announced_value"] = _announced_text(row)
    if evidence is None:
        record["basis"] = "none"
        record["basis_ref"] = "unverified:no_evidence"
    else:
        record["basis"] = "announcement"
        record["basis_ref"] = record["basis_ref"] + ";D-92 one-tick"
    if announced is None or factor is None or prev_close is None:
        record["check_status"] = "unverified"
        if evidence is not None:
            record["basis_ref"] = record["basis_ref"] + " not_applicable"
        warnings["no_prev_close"].append(
            {
                "entity": event["entity"],
                "ordinal": event["ordinal"],
                "effective_day": event["effective_day"],
                "reason": "D-92 判不了（公告值 / gbbq 因子 / 前收缺失）",
            }
        )
        return "D-92 判不了：公告值、gbbq 因子或前收缺失"
    delta = compose_price(prev_close, [factor]) - compose_price(prev_close, [announced])
    record["price_delta"] = decimal_text(delta, DELTA_SCALE)
    record["within_one_tick"] = within_one_tick(delta)
    if record["within_one_tick"]:
        record["check_status"] = "verified_announcement"
        return f"D-92 通过：|Δ|={decimal_text(abs(delta), DELTA_SCALE)} <= 0.001"
    record["check_status"] = "unverified"
    if evidence is not None:
        record["basis_ref"] = record["basis_ref"] + " failed"
    return f"D-92 不通过：|Δ|={decimal_text(abs(delta), DELTA_SCALE)} > 0.001"


def _decide_composite(event, row, record, group_events, group_rows, prev_close, warnings):
    """同一个 E 上的多个事件：合并成一次调整后再核对（D-91 §2）。"""
    official_texts = {item.get("official_qss", "") for item in group_rows if item}
    prev_closes = {item.get("prev_close", "") for item in group_rows if item}
    prev = prev_close
    if len(prev_closes) == 1:
        prev = parse_decimal(next(iter(prev_closes)), "prev_close")
    elif len(prev_closes) > 1:
        prev = None
    evidence = official_evidence(row, event["effective_day"])
    factors = [gbbq_factor(item) for item in group_events]
    official = None
    if len(official_texts) == 1:
        official = parse_decimal(next(iter(official_texts)), "official_qss")
    if official is None or evidence is None or prev is None or any(item is None for item in factors):
        record["check_status"] = "unverified"
        record["basis"] = "official_qss" if evidence is not None else "none"
        if evidence is not None:
            _evidence_fields(record, evidence)
            record["basis_ref"] = record["basis_ref"] + ";same_E_composite;unusable"
        else:
            record["basis_ref"] = "same_E_composite;no_official_qss"
        return "同 E 复合：官方 qss / 前收 / 因子缺失，合并结果无法锚定"
    record["basis"] = "official_qss"
    _evidence_fields(record, evidence)
    record["basis_ref"] = record["basis_ref"] + ";same_E_composite"
    record["announced_value"] = next(iter(official_texts))
    delta = official - compose_price(prev, factors)
    record["price_delta"] = decimal_text(delta, DELTA_SCALE)
    record["within_one_tick"] = within_one_tick(delta)
    if record["within_one_tick"]:
        record["check_status"] = "verified_official"
        return (
            "同 E 复合（先 cat11 后 cat1）："
            f"官方 {decimal_text(official)} − 合并价 = {decimal_text(delta, DELTA_SCALE)}"
        )
    record["check_status"] = "unverified"
    warnings["official_delta_over_tick"].append(
        {
            "entity": event["entity"],
            "ordinal": event["ordinal"],
            "effective_day": event["effective_day"],
            "price_delta": record["price_delta"],
            "reason": "同 E 复合的合并结果与官方 qss 不符",
        }
    )
    return f"同 E 复合：|Δ|={decimal_text(abs(delta), DELTA_SCALE)} > 0.001"


def _announced_text(row):
    """记录里的 `announced_value`：公告给的原始数值，去掉浮点噪声。

    cat1 取 `ann_amount_per10`（元/10 份，与 c1 同口径；v3 里它与 `ann_amount`
    绝大多数同值，只有 sh.511880 2020-12-30 那行是 2.015 对 20.15），cat11 取
    `ann_ratio`。v3 的这列偶尔带 `20.150000000000002` 这样的浮点尾巴，收到 12 位
    小数再写，免得记录里的数字看起来像精度丢失。
    """
    text = (
        (row.get("ann_amount_per10") or row.get("ann_amount") or "")
        if row["category"] == 1
        else (row.get("ann_ratio") or "")
    )
    value = parse_decimal(text, "announced_value")
    return "" if value is None else decimal_text(value, DELTA_SCALE)


# ---------------------------------------------------------------------------
# 报告
# ---------------------------------------------------------------------------


def _count(records, key):
    counts = defaultdict(int)
    for record in records:
        counts[key(record)] += 1
    return dict(sorted(counts.items()))


def _blocked_entities(records, unmatched):
    """按 D-92 的阻断规则汇总实体：unverified 记录或缺失记录都整只阻断。"""
    unverified = defaultdict(int)
    for record in records:
        if record["check_status"] == "unverified":
            unverified[record["entity"]] += 1
    missing = defaultdict(int)
    for row in unmatched:
        missing[row["entity"]] += 1
    entities = {record["entity"] for record in records} | set(missing)
    blocked = []
    for entity in sorted(entities):
        reasons = []
        if unverified.get(entity):
            reasons.append("etf_factor_unverified")
        if missing.get(entity):
            reasons.append("etf_factor_missing_record")
        if reasons:
            blocked.append(
                {
                    "entity": entity,
                    "reasons": reasons,
                    "unverified_records": unverified.get(entity, 0),
                    "missing_events": missing.get(entity, 0),
                }
            )
    return entities, blocked


def _report(records, results, db_events, v3_rows, pairs, unmatched, warnings, checked_at):
    entities, blocked = _blocked_entities(records, unmatched)
    blocked_names = {item["entity"] for item in blocked}
    d92_rows = []
    for item in results:
        row = item["v3_row"]
        if row["final_status"] != "unverified":
            continue
        record = item["record"]
        d92_rows.append(
            {
                "entity": record["entity"],
                "ordinal": record["ordinal"],
                "category": record["category"],
                "effective_day": record["effective_day"],
                "decision": record["check_status"],
                "price_delta": record["price_delta"],
                "within_one_tick": record["within_one_tick"],
                "announced_value": record["announced_value"],
                "gbbq_value": record["gbbq_value"],
                "basis": record["basis"],
                "basis_ref": record["basis_ref"],
                "reason": item["reason"],
            }
        )
    matched_lines = {item["v3_row"]["line"] for item in results}
    not_matched_d92 = [
        {
            "entity": row["entity"],
            "category": row["category"],
            "effective_day": row["ex_day"],
            "v3_line": row["line"],
        }
        for row in v3_rows
        if row["final_status"] == "unverified" and row["line"] not in matched_lines
    ]
    validation_failures = []
    for record in records:
        errors = validate_record(record)
        if errors:
            validation_failures.append(
                {"entity": record["entity"], "ordinal": record["ordinal"], "errors": errors}
            )
    by_market = defaultdict(lambda: defaultdict(int))
    for record in records:
        by_market[market_of(record["entity"])][record["check_status"]] += 1
    return {
        "ok": not unmatched and not validation_failures,
        "checked_at": checked_at,
        "events_v3_rows": len(v3_rows),
        "db_events": len(db_events),
        "records": len(records),
        "matched": len(pairs),
        "by_check_status": _count(records, lambda record: record["check_status"]),
        "by_basis": _count(records, lambda record: record["basis"]),
        "by_market_and_status": {
            market: dict(sorted(counts.items())) for market, counts in sorted(by_market.items())
        },
        "unmatched": {"count": len(unmatched), "rows": unmatched},
        "d92": {
            "rows": d92_rows,
            "verified": sum(1 for item in d92_rows if item["decision"] != "unverified"),
            "unverified": sum(1 for item in d92_rows if item["decision"] == "unverified"),
            "not_matched": not_matched_d92,
        },
        "warnings": warnings,
        "entities": {
            "total": len(entities),
            "deliverable": len(entities) - len(blocked),
            "blocked": len(blocked),
            "blocked_entities": blocked,
            "deliverable_entities": sorted(entities - blocked_names),
        },
        "validation": {
            "records_checked": len(records),
            "failures": validation_failures,
        },
        "evidence_note": EVIDENCE_NOTE,
    }


# ---------------------------------------------------------------------------
# 入口
# ---------------------------------------------------------------------------


def convert(events_v3_path, db_path, checked_at=None, allow_unmatched=False):
    """跑完整转换：返回 `(records, report)`，不写文件。"""
    checked_at = checked_at or datetime.now(timezone.utc).date().isoformat()
    if parse_day(checked_at) is None:
        raise InputError(f"--checked-at {checked_at!r} 不是 YYYY-MM-DD")
    v3_rows, v3_warnings = load_events_v3(events_v3_path)
    conn = open_read_only(db_path)
    try:
        etf_entities, _columns_seen, selected = read_db_events(conn)
        entities = etf_entities | {row["entity"] for row in v3_rows}
        db_events = fetch_events(conn, selected, entities)
    finally:
        conn.close()
    records, report = build_records(db_events, v3_rows, checked_at)
    report["warnings"].update(v3_warnings)
    not_etf = sorted({row["entity"] for row in v3_rows} - etf_entities)
    if etf_entities:
        report["warnings"]["entity_not_etf_in_db"] = [
            {"entity": entity} for entity in not_etf
        ]
    report["inputs"] = {"events_v3": os.path.abspath(events_v3_path), "db": os.path.abspath(db_path)}
    report["allow_unmatched"] = allow_unmatched
    return records, report


def write_jsonl(path, records):
    directory = os.path.dirname(os.path.abspath(path))
    if directory:
        os.makedirs(directory, exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        for record in records:
            handle.write(canonical_json(record))
            handle.write("\n")


def write_report(path, report):
    directory = os.path.dirname(os.path.abspath(path))
    if directory:
        os.makedirs(directory, exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(report, handle, ensure_ascii=False, indent=2, sort_keys=False)
        handle.write("\n")


def main(argv=None):
    parser = argparse.ArgumentParser(
        description="把 events-v3 的取证结果转成 etf-factor-checks-v1 记录（只读库，不写来源库）",
    )
    subparsers = parser.add_subparsers(dest="command", required=True)
    convert_parser = subparsers.add_parser(
        "convert", help="转换并写出 JSONL（可选报告）"
    )
    convert_parser.add_argument("--events-v3", required=True, help="取证表 events-v3.csv")
    convert_parser.add_argument("--db", required=True, help="行情库（只读打开）")
    convert_parser.add_argument("--out", required=True, help="输出 JSONL（每行一条记录）")
    convert_parser.add_argument("--report", help="输出 JSON 报告")
    convert_parser.add_argument(
        "--checked-at",
        help="核对日期 YYYY-MM-DD（默认今天 UTC；固定它可以复跑出逐位相同的记录 id）",
    )
    convert_parser.add_argument(
        "--allow-unmatched",
        action="store_true",
        help="有 unmatched 时仍以 0 退出（报告照写）",
    )
    args = parser.parse_args(argv)

    if args.command != "convert":
        parser.error(f"未知子命令 {args.command!r}")

    try:
        records, report = convert(
            args.events_v3, args.db, args.checked_at, args.allow_unmatched
        )
    except InputError as exc:
        print(json.dumps({"ok": False, "error": str(exc)}, ensure_ascii=False))
        return EXIT_INPUT_ERROR

    failures = report["validation"]["failures"]
    if failures:
        # 一条坏记录都不写：宁可这次不出文件。
        print(
            json.dumps(
                {
                    "ok": False,
                    "error": "记录没有通过 PR-B 的校验规则，未写文件",
                    "failures": failures[:20],
                    "failed_records": len(failures),
                },
                ensure_ascii=False,
            )
        )
        if args.report:
            write_report(args.report, report)
        return EXIT_NEEDS_REVIEW

    write_jsonl(args.out, records)
    if args.report:
        write_report(args.report, report)
    summary = {
        "ok": report["ok"],
        "records": report["records"],
        "by_check_status": report["by_check_status"],
        "unmatched": report["unmatched"]["count"],
        "blocked_entities": report["entities"]["blocked"],
        "d92": {
            "rows": len(report["d92"]["rows"]),
            "verified": report["d92"]["verified"],
            "unverified": report["d92"]["unverified"],
        },
        "out": os.path.abspath(args.out),
    }
    print(json.dumps(summary, ensure_ascii=False))
    if report["unmatched"]["count"] and not args.allow_unmatched:
        return EXIT_NEEDS_REVIEW
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
