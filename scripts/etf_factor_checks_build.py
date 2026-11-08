"""(#607) 公告取证结果 → `etf-factor-checks-v1` 记录（首次导入用），外加每周核对。

输入是阶段 1 取证表 `events-v3.csv`（VM2-23 的产物，1,233 行 = 979 cat1 + 254
cat11），加一个**只读**打开的行情库；输出是 `source_store import --format
etf-factor-checks-jsonl` 原样接受的 JSONL，以及一份供复核的 JSON 报告。

两个子命令：

- `convert`：一次性把取证表转成记录（首次导入）；
- `verify`：**每周核对**（D-92 Q10），库里的快照之后新出现的 cat1/cat11 事件，
  凡是 `(entity, ordinal, evidence_digest)` 在已有记录里没有匹配的，逐条去官方
  日行情（深市 `1815_stock`）或基金公告（沪市 `commonQuery.do` / 深市本地公告
  索引 `szse_etf_notice_ann`）取证，生成新记录。核对不了的记 `unverified` 并写
  明原因，一条都不跳过——PR-B 的阻断判定「没有匹配记录即阻断」，所以新事件在
  这里有记录（哪怕是 `unverified`）才算处理过。

`verify` 的网络访问全部走可替换的 fetcher 接口（[`HttpFetcher`]），测试用假实现；
PDF 文本抽取是纯函数（[`parse_announcement_text`]），测试用 `scripts/testdata/`
里的文本夹具，仓库里不放 PDF。**测试不联网、不碰生产、不写来源库。**

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
import time
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

#: `verify` 的请求预算与限速（D-92 Q10 的运维约束）。
DEFAULT_MAX_REQUESTS = 300
MIN_REQUEST_INTERVAL_SECONDS = 1.0
RETRY_BACKOFF_SECONDS = 30.0
MAX_RETRIES = 1
HTTP_TIMEOUT_SECONDS = 30.0
#: 交易所只按 UA 区分「谁在抓」；这里用它而不是浏览器 UA。
USER_AGENT = "x-data-mcp-exchange-source/1"
#: 深市公告的候选窗口（公告日落在 [E − 5, E + 60] 内）。
SZSE_NOTICE_LOOKBACK_DAYS = 5
SZSE_NOTICE_LOOKAHEAD_DAYS = 60
#: 深市官方日行情的分页上限（`PAGENO`），防止一个坏响应把请求全烧掉。
SZSE_OFFICIAL_MAX_PAGES = 4

EXIT_OK = 0
EXIT_INPUT_ERROR = 2
EXIT_NEEDS_REVIEW = 3

ENTITY_RE = re.compile(r"^(sh|sz|bj)\.(\d{6})$")
DECIMAL_RE = re.compile(r"^[+-]?(\d+(\.\d*)?|\.\d+)$")

#: 全角数字/标点在公告里很常见；先把它们折成半角再解析（`４２．２８` → `42.28`）。
FULLWIDTH_TRANSLATION = str.maketrans(
    "０１２３４５６７８９．，：；（）％＋－＝　",
    "0123456789.,:;()%+-= ",
)
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


def normalize_fullwidth(text):
    """全角数字与全角标点 → 半角（`４２．２８` → `42.28`）。

    公告原文里两种写法都有，VM2-23 把 `42．28` 读成 42.0 就是漏了这一步。数字、
    小数点、正负号、千分位与括号都要折，否则 `parse_decimal` 会把它当成非法值。
    """
    if not text:
        return text
    return text.translate(FULLWIDTH_TRANSLATION)


def parse_decimal(value, what):
    """`value` 是空串时返回 None，否则必须是十进制数。

    全角写法先归一化（`４２．２８` → `42.28`），否则 `Decimal` 直接抛异常。
    """
    text = normalize_fullwidth((value or "").strip())
    text = text.replace("−", "-").replace("－", "-").replace(",", "")
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
# 人工裁定（`--overrides`）
# ---------------------------------------------------------------------------


class Override:
    """一条人工裁定：这条事件的公告值、依据与原文片段。

    它只给**公告说的数**，不给判定：判定仍由 D-92 的 1 tick 规则做。字段：

    - `entity` / `ordinal` / `effective_day` / `category`：定位事件（四个都要对）；
    - `announced`：`{"c1": <元/10 份>}`（cat1）或 `{"c3": <折算比>}`（cat11）；
    - `basis_ref`：人看的依据，例如「D-92 裁定，#607 2026-11-08 评论」；
    - `snippet`：公告原文片段（照抄，用来复核读出来的数）；
    - `evidence_url` / `evidence_sha256`：可选，给了就写进记录的公告证据。
    """

    __slots__ = (
        "entity",
        "ordinal",
        "effective_day",
        "category",
        "announced",
        "basis_ref",
        "snippet",
        "evidence_url",
        "evidence_sha256",
        "note",
    )

    def __init__(self, **fields):
        for slot in self.__slots__:
            setattr(self, slot, fields.get(slot))
        self.announced = fields.get("announced") or {}

    def key(self):
        return (self.entity, self.ordinal, self.effective_day, self.category)

    def as_dict(self):
        return {
            "entity": self.entity,
            "ordinal": self.ordinal,
            "effective_day": self.effective_day,
            "category": self.category,
            "announced": {
                key: decimal_text(value, DELTA_SCALE)
                for key, value in sorted(self.announced.items())
            },
            "basis_ref": self.basis_ref,
            "snippet": self.snippet,
        }


def load_overrides(path):
    """读人工裁定 JSON → `{(entity, ordinal, effective_day, category): Override}`。

    文件形状是一个 `{"overrides": [...]}` 对象或直接一个数组。每一行都要能过校验：
    实体形状、`ordinal >= 0`、`category in (1, 11)`、日期合法、`announced` 里有与
    类别对应的数（cat1 给 `c1`，cat11 给 `c3`）、`basis_ref` 与 `snippet` 非空。
    不合格就抛 `InputError`——人工裁定是「人工签字」的东西，不能含糊。

    同一个键出现两次是错误（哪条说了算不能靠顺序）。
    """
    if not path:
        return {}
    if not os.path.exists(path):
        raise InputError(f"--overrides 文件不存在：{path}")
    try:
        with open(path, "r", encoding="utf-8") as handle:
            payload = json.load(handle)
    except (OSError, json.JSONDecodeError) as exc:
        raise InputError(f"--overrides 读不了：{exc}") from exc
    rows = payload.get("overrides") if isinstance(payload, dict) else payload
    if not isinstance(rows, list):
        raise InputError("--overrides 需要 {'overrides': [...]} 对象或数组")
    overrides = {}
    for index, raw in enumerate(rows, start=1):
        if not isinstance(raw, dict):
            raise InputError(f"--overrides 第 {index} 条不是对象")
        entity = str(raw.get("entity") or "")
        if not ENTITY_RE.match(entity):
            raise InputError(
                f"--overrides 第 {index} 条 entity {entity!r} 不是 <sh|sz|bj>.<6 位>"
            )
        try:
            ordinal = int(raw.get("ordinal"))
        except (TypeError, ValueError) as exc:
            raise InputError(f"--overrides 第 {index} 条 ordinal 不是整数") from exc
        if ordinal < 0:
            raise InputError(f"--overrides 第 {index} 条 ordinal 不能为负")
        category = raw.get("category")
        if category not in (1, 11):
            raise InputError(f"--overrides 第 {index} 条 category {category!r} 不是 1 或 11")
        effective_day = str(raw.get("effective_day") or "")
        if parse_day(effective_day) is None:
            raise InputError(
                f"--overrides 第 {index} 条 effective_day {effective_day!r} 不是 YYYY-MM-DD"
            )
        announced_raw = raw.get("announced")
        if not isinstance(announced_raw, dict):
            raise InputError(f"--overrides 第 {index} 条缺 announced 对象")
        announced = {}
        field = "c1" if category == 1 else "c3"
        if field not in announced_raw:
            raise InputError(
                f"--overrides 第 {index} 条 category={category} 需要 announced.{field}"
            )
        for name, value in announced_raw.items():
            if name not in ("c1", "c3"):
                raise InputError(f"--overrides 第 {index} 条 announced 不支持字段 {name!r}")
            parsed = parse_decimal(str(value), f"--overrides[{index}].announced.{name}")
            if parsed is None:
                raise InputError(f"--overrides 第 {index} 条 announced.{name} 为空")
            if name == "c3" and parsed == 0:
                raise InputError(f"--overrides 第 {index} 条 announced.c3 不能为 0")
            announced[name] = parsed
        basis_ref = str(raw.get("basis_ref") or "").strip()
        if not basis_ref:
            raise InputError(f"--overrides 第 {index} 条缺 basis_ref")
        snippet = str(raw.get("snippet") or "").strip()
        if not snippet:
            raise InputError(f"--overrides 第 {index} 条缺 snippet（公告原文片段）")
        evidence_sha256 = str(raw.get("evidence_sha256") or "").strip().lower()
        if evidence_sha256 and not SHA256_RE.match(evidence_sha256):
            raise InputError(
                f"--overrides 第 {index} 条 evidence_sha256 不是 64 位小写 hex"
            )
        override = Override(
            entity=entity,
            ordinal=ordinal,
            effective_day=effective_day,
            category=int(category),
            announced=announced,
            basis_ref=basis_ref,
            snippet=snippet,
            evidence_url=str(raw.get("evidence_url") or "").strip(),
            evidence_sha256=evidence_sha256,
            note=str(raw.get("note") or "").strip(),
        )
        if override.key() in overrides:
            raise InputError(f"--overrides 第 {index} 条与前面的键重复：{override.key()}")
        overrides[override.key()] = override
    return overrides


def apply_overrides(db_events, pairs, overrides):
    """把裁定挂到 v3 行上；返回 `(used_keys, unused_keys)`。

    键是 `(entity, ordinal, effective_day, category)`，`ordinal` 与 `effective_day`
    只有库里读出来的事件才有（v3 没有 `ordinal` 列），所以要先配对再用。

    未用到的裁定要报出来：它可能是键写错了（ordinal 或 E 不对），静默忽略等于
    让人以为自己签过字。
    """
    used = set()
    for event in db_events:
        row = pairs.get(event_key(event))
        if row is None:
            continue
        key = (
            event["entity"],
            event["ordinal"],
            event["effective_day"],
            event["category"],
        )
        override = overrides.get(key)
        if override is None:
            continue
        row["override"] = override
        used.add(key)
    unused = sorted(set(overrides) - used)
    return used, unused


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

    人工裁定（`--overrides`，见 [`load_overrides`]）优先：它带原文片段，比 v3 的
    一列解析结果更可信。
    """
    override = row.get("override")
    if override is not None:
        factor = override_factor(override, row["category"])
        if factor is not None:
            return factor
    category = row["category"]
    if category == 1:
        text = row.get("ann_amount_per10") or row.get("ann_amount") or ""
        value = parse_decimal(text, "ann_amount")
        if value is None:
            return None
        return (1, value, Decimal(0))
    if category == 11:
        # v3 只有解析后的一个数，没有原文。写的是「N:M 拆分/合并」的那些，方向
        # 由 [`_parse_ratio`] 按原文判定（N:M 即「新：旧」，N/M 就是 c3）；v3 里
        # 存的是解析结果，所以只在它有原文片段时才能纠正——否则保持原值。
        text = row.get("ann_ratio_text") or ""
        if text.strip():
            ratio, _note = _parse_ratio(normalize_fullwidth(text))
            if ratio is not None:
                return (11, Decimal(0), ratio)
        value = parse_decimal(row.get("ann_ratio") or "", "ann_ratio")
        if value is None or value == 0:
            return None
        return (11, Decimal(0), value)
    return None


def override_factor(override, category):
    """人工裁定 → `(category, c1, c3)`；裁定没有这个类别时返回 None。

    裁定里只写「公告说的数」，两个类别都按同一口径表达（见 [`load_overrides`]）。
    """
    announced = override.announced if hasattr(override, "announced") else override.get(
        "announced", {}
    )
    if category == 1:
        if "c1" not in announced:
            return None
        return (1, announced["c1"], Decimal(0))
    if category == 11:
        if "c3" not in announced:
            return None
        return (11, Decimal(0), announced["c3"])
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


def _bar_groups(events, rows_by_key):
    """按「E 之后的第一根 bar」（v3 的 `X_day`）分组。

    为什么不是按 E 分组：两条事件的 E 可以不同（`sz.159922` 的 cat11 在周日
    2024-12-01、cat1 在周一 2024-12-02），但它们作用在**同一根 bar** 上，官方
    `qss` 是两笔合起来的结果。按 E 分组会把它们拆成两条单事件，谁都核不上。

    `rows_by_key` 是 `{event_key(event): v3 row}`；没有 v3 行的事件按自己的 E 当分组键
    （它是这一组里唯一的事件，分组不影响结果）。
    """
    grouped = defaultdict(list)
    for event in events:
        row = rows_by_key.get(event_key(event))
        # 分组键 = 这条事件作用到的第一根 bar：v3 的 X_day，没有就用 E。
        bar_day = (row or {}).get("X_day") or event["effective_day"]
        grouped[(event["entity"], bar_day)].append(event)
    for group in grouped.values():
        # 同一个 bar 上：E 早的先做（不同 E 的复合按时间先后），E 相同先 cat11。
        group.sort(key=lambda item: (item["effective_day"], item["category"] != 11, item["ordinal"]))
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
    # 复合核对按「E 之后的第一根 bar」分组：不同 E 但作用在同一根 bar 上的事件
    # 也要合并（`sz.159922` 的 cat11 E=周日 + cat1 E=周一），否则两条都核不上。
    bar_groups = _bar_groups(db_events, pairs)
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
        bar_day = (row.get("X_day") or event["effective_day"])
        group = bar_groups[(event["entity"], bar_day)]
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
    override = row.get("override")
    if override is not None:
        return _decide_with_override(event, row, record, factor, override, prev_close, warnings)
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
    """作用在同一根 bar 上的多个事件：合并成一次调整后再核对（D-91 §2）。

    D-91 §2 定的是「同一个 E 上先 cat11 后 cat1」。生产数据里还有另一种复合：
    两条事件的 E 不同（`sz.159922` 的 cat11 E=2024-12-01 周日、cat1 E=2024-12-02
    周一），但都作用在**同一根 bar**（`X_day` = 2024-12-02）上——官方当天的 `qss`
    是两笔合起来的结果，所以必须合并核对。`compose_price` 对 `(category, c1, c3)`
    序列按给定次序复合，调用方已把 `group_events` 按 `(E, 类别)` 排好。

    复合的依据写进 `basis_ref`：`same_E_composite`（E 相同）或
    `same_bar_composite`（E 不同、作用在同一根 bar）。
    """
    same_day = len({item["effective_day"] for item in group_events}) == 1
    label = "same_E_composite" if same_day else "same_bar_composite"
    official_texts = {item.get("official_qss", "") for item in group_rows if item}
    prev_closes = {item.get("prev_close", "") for item in group_rows if item}
    prev = prev_close
    if len(prev_closes) == 1:
        prev = parse_decimal(next(iter(prev_closes)), "prev_close")
    elif len(prev_closes) > 1:
        prev = None
    # 官方证据是**那根 bar** 当天的 qss，不是某一条事件自己的 E：复合组里 E 可能
    # 不同（`sz.159922` 的 cat11 在周日），qss 只在 bar 上有。
    bar_day = row.get("X_day") or event["effective_day"]
    evidence = official_evidence(row, bar_day)
    factors = [gbbq_factor(item) for item in group_events]
    official = None
    if len(official_texts) == 1:
        official = parse_decimal(next(iter(official_texts)), "official_qss")
    if official is None or evidence is None or prev is None or any(item is None for item in factors):
        record["check_status"] = "unverified"
        record["basis"] = "official_qss" if evidence is not None else "none"
        if evidence is not None:
            _evidence_fields(record, evidence)
            record["basis_ref"] = record["basis_ref"] + f";{label};unusable"
        else:
            record["basis_ref"] = f"{label};no_official_qss"
        return f"{label}：官方 qss / 前收 / 因子缺失，合并结果无法锚定"
    record["basis"] = "official_qss"
    _evidence_fields(record, evidence)
    record["basis_ref"] = record["basis_ref"] + f";{label}"
    record["announced_value"] = next(iter(official_texts))
    delta = official - compose_price(prev, factors)
    record["price_delta"] = decimal_text(delta, DELTA_SCALE)
    record["within_one_tick"] = within_one_tick(delta)
    if record["within_one_tick"]:
        record["check_status"] = "verified_official"
        return (
            f"{label}（按 E 先后复合）："
            f"官方 {decimal_text(official)} − 合并价 = {decimal_text(delta, DELTA_SCALE)}"
        )
    record["check_status"] = "unverified"
    warnings["official_delta_over_tick"].append(
        {
            "entity": event["entity"],
            "ordinal": event["ordinal"],
            "effective_day": event["effective_day"],
            "price_delta": record["price_delta"],
            "reason": f"{label}：合并结果与官方 qss 不符",
        }
    )
    return f"{label}：|Δ|={decimal_text(abs(delta), DELTA_SCALE)} > 0.001"


def _decide_with_override(event, row, record, factor, override, prev_close, warnings):
    """人工裁定：用裁定给的公告值走 D-92 的 1 tick 判定。

    裁定替代的是「公告值是什么」，**不是**判定本身。价差仍按
    `用 gbbq 因子算出的复权价 − 用公告值算出的复权价` 计算，`|Δ| <= 0.001` 才通过。

    - 公告证据：裁定给了 `evidence_url` + `evidence_sha256` 就用它，
      否则保留 v3 行的公告证据；
    - `basis_ref` 除了证据标签，还写上裁定的依据（例如 D-92 的文档与日期），
      这样复核时能一眼看出「这个数不是脚本自己读出来的」。
    """
    basis_ref = override.basis_ref
    record["evidence_url"] = override.evidence_url or record.get("evidence_url", "")
    record["evidence_sha256"] = override.evidence_sha256 or record.get("evidence_sha256", "")
    record["basis"] = "announcement" if record["evidence_sha256"] else "none"
    record["announced_value"] = _announced_text(row)
    announced = announced_factor(row)
    head = (record.get("basis_ref") or market_label(event["entity"])).split(";")[0]
    if record["basis"] == "none":
        record["basis_ref"] = f"{head};D-92 one-tick;override:{basis_ref};no_announcement_bytes"
    else:
        record["basis_ref"] = f"{head};D-92 one-tick;override:{basis_ref}"
    if announced is None or factor is None or prev_close is None:
        record["check_status"] = "unverified"
        record["basis_ref"] += ";D-92 not_applicable"
        warnings["no_prev_close"].append(
            {
                "entity": event["entity"],
                "ordinal": event["ordinal"],
                "effective_day": event["effective_day"],
                "reason": "人工裁定：公告值 / gbbq 因子 / 前收缺失，D-92 判不了",
            }
        )
        return "人工裁定：D-92 判不了（公告值、gbbq 因子或前收缺失）"
    delta = compose_price(prev_close, [factor]) - compose_price(prev_close, [announced])
    record["price_delta"] = decimal_text(delta, DELTA_SCALE)
    record["within_one_tick"] = within_one_tick(delta)
    if record["within_one_tick"]:
        record["check_status"] = "verified_announcement"
        return f"人工裁定 + D-92 通过：|Δ|={decimal_text(abs(delta), DELTA_SCALE)} <= 0.001"
    record["check_status"] = "unverified"
    record["basis_ref"] += ";D-92 failed"
    return f"人工裁定 + D-92 不通过：|Δ|={decimal_text(abs(delta), DELTA_SCALE)} > 0.001"


def _announced_text(row):
    """记录里的 `announced_value`：公告给的原始数值，去掉浮点噪声。

    cat1 取 `ann_amount_per10`（元/10 份，与 c1 同口径；v3 里它与 `ann_amount`
    绝大多数同值，只有 sh.511880 2020-12-30 那行是 2.015 对 20.15），cat11 取
    `ann_ratio`。v3 的这列偶尔带 `20.150000000000002` 这样的浮点尾巴，收到 12 位
    小数再写，免得记录里的数字看起来像精度丢失。
    """
    override = row.get("override")
    if override is not None:
        factor = override_factor(override, row["category"])
        if factor is not None:
            _category, c1, c3 = factor
            return decimal_text(c1 if row["category"] == 1 else c3, DELTA_SCALE)
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
# 每周核对（verify）：请求预算、限速与 fetcher 接口
# ---------------------------------------------------------------------------

#: 官方日行情的请求 URL（可复跑）。`PAGENO` 从 1 开始。
SZSE_OFFICIAL_URL_TEMPLATE = (
    "https://www.szse.cn/api/report/ShowReport/data"
    "?SHOWTYPE=JSON&CATALOGID=1815_stock&TABKEY=tab2"
    "&txtBeginDate={day}&txtEndDate={day}&PAGENO={page}"
)
#: 沪市公告检索（`COMMON_PL_JJXX_JJGG_L`），按 `SECURITY_CODE` + 日期窗口。
SSE_ANNOUNCEMENT_URL_TEMPLATE = (
    "https://query.sse.com.cn/commonQuery.do?sqlId=COMMON_PL_JJXX_JJGG_L"
    "&isPagination=true&pageHelp.pageSize=100&pageHelp.pageNo={page}"
    "&SECURITY_CODE={code}&START_DATE={start}&END_DATE={end}"
)
#: `--dry-run` 报告里的 URL 模板（参数保持占位，不写真日期）。
_SZSE_OFFICIAL_TEMPLATE_TEMPLATE = (
    "https://www.szse.cn/api/report/ShowReport/data"
    "?SHOWTYPE=JSON&CATALOGID=1815_stock&TABKEY=tab2"
    "&txtBeginDate={E}&txtEndDate={E}&PAGENO={page}"
)
_SSE_ANNOUNCEMENT_TEMPLATE_TEMPLATE = (
    "https://query.sse.com.cn/commonQuery.do?sqlId=COMMON_PL_JJXX_JJGG_L"
    "&isPagination=true&pageHelp.pageSize=100&pageHelp.pageNo={page}"
    "&SECURITY_CODE={code}&START_DATE={E-30}&END_DATE={E+15}"
)
#: 沪市公告的日期窗口（公告日落在 [E − 30, E + 15] 内）。
SSE_ANNOUNCEMENT_LOOKBACK_DAYS = 30
SSE_ANNOUNCEMENT_LOOKAHEAD_DAYS = 15
#: 一次 `--dry-run` 只按模板列请求，不展开每一个事件（避免报告变成请求日志）。


class FetchError(Exception):
    """一次抓取失败（网络错误、非 200、解析失败）；由调用方决定重试还是放弃。"""


class RequestBudgetExceeded(Exception):
    """请求预算用完。**任何**后续请求都不再发出，包括重试。"""


class Fetcher:
    """可替换的网络访问接口：只做主请求，不管预算、限速与重试。

    线上实现是 [`HttpFetcher`]；测试用假实现，`--dry-run` 一个请求都不发。
    """

    def get(self, url):  # pragma: no cover - 接口
        raise NotImplementedError

    def get_bytes(self, url):  # pragma: no cover - 接口
        raise NotImplementedError

    def pdftotext(self, pdf_bytes, name="ann.pdf"):
        """PDF 字节 → 文本（`pdftotext -layout` 的等价物）。

        抽取器不能猜：没有 `pdftotext` 时抛 `FetchError`，调用方把该事件记
        `unverified`，而不是把空文本当公告内容。
        """
        raise FetchError("这个 fetcher 不支持 PDF 文本抽取")


class HttpFetcher(Fetcher):
    """线上 fetcher：`urllib`，UA 固定为 [`USER_AGENT`]。

    它**不做**限速、重试与预算控制，那些都在 [`RequestBudget`] 里——否则测试
    没法在不联网的前提下验证提交次数。
    """

    def __init__(self, timeout=HTTP_TIMEOUT_SECONDS):
        self.timeout = timeout

    def get_bytes(self, url):
        import urllib.error
        import urllib.request

        request = urllib.request.Request(
            url, headers={"User-Agent": USER_AGENT, "Accept": "*/*"}
        )
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                status = getattr(response, "status", 200)
                body = response.read()
        except urllib.error.HTTPError as exc:
            raise FetchError(f"HTTP {exc.code}: {url}") from exc
        except urllib.error.URLError as exc:
            raise FetchError(f"URL 错误（{exc.reason}）：{url}") from exc
        except OSError as exc:  # 超时、连接重置
            raise FetchError(f"网络错误（{exc}）：{url}") from exc
        if status != 200:
            raise FetchError(f"HTTP {status}: {url}")
        return body

    def get(self, url):
        return self.get_bytes(url).decode("utf-8", errors="replace")

    def pdftotext(self, pdf_bytes, name="ann.pdf"):
        return pdftotext_bytes(pdf_bytes, name)


def pdftotext_bytes(pdf_bytes, name="ann.pdf"):
    """PDF 字节 → 文本：写临时文件后串行跑 `pdftotext -layout`。

    **串行**是有意的：VM2-23 的并发跑出现过文本与 PDF 错号。
    """
    import shutil
    import subprocess
    import tempfile

    binary = shutil.which("pdftotext")
    if binary is None:
        raise FetchError("本机没有 pdftotext，抽不出公告文本")
    with tempfile.TemporaryDirectory() as directory:
        pdf_path = os.path.join(directory, "ann.pdf")
        txt_path = os.path.join(directory, "ann.txt")
        with open(pdf_path, "wb") as handle:
            handle.write(pdf_bytes)
        result = subprocess.run(
            [binary, "-layout", pdf_path, txt_path],
            capture_output=True,
            text=True,
            check=False,
        )
        if result.returncode != 0 or not os.path.exists(txt_path):
            raise FetchError(
                f"pdftotext 失败（{result.returncode}）：{(result.stderr or '').strip()[:200]}"
            )
        with open(txt_path, "rb") as handle:
            return handle.read().decode("utf-8", errors="replace")


class RequestBudget:
    """请求计数 + 限速 + 重试（≥ 30 s 退避，最多 1 次，重试也计数）。

    超过 `max_requests` 立即 `RequestBudgetExceeded`，**不**发出第 N+1 个请求。
    请求间隔不小于 `min_interval`；`sleep` 与 `clock` 可注入，测试因此不需要真等。
    """

    def __init__(
        self,
        fetcher,
        max_requests=DEFAULT_MAX_REQUESTS,
        min_interval=MIN_REQUEST_INTERVAL_SECONDS,
        backoff=RETRY_BACKOFF_SECONDS,
        max_retries=MAX_RETRIES,
        sleep=None,
        clock=None,
    ):
        self.fetcher = fetcher
        self.max_requests = int(max_requests)
        self.min_interval = float(min_interval)
        self.backoff = float(backoff)
        self.max_retries = int(max_retries)
        self.sleep = sleep or time.sleep
        self.clock = clock or time.monotonic
        self.requests = 0
        self.retries = 0
        self._last_started = None
        self.urls = []

    @property
    def remaining(self):
        return max(0, self.max_requests - self.requests)

    def _throttle(self):
        if self._last_started is None or self.min_interval <= 0:
            return
        elapsed = self.clock() - self._last_started
        if elapsed < self.min_interval:
            self.sleep(self.min_interval - elapsed)

    def _spend(self):
        if self.requests >= self.max_requests:
            raise RequestBudgetExceeded(
                f"请求数已达上限 {self.max_requests}"
            )
        self.requests += 1

    def call(self, url, binary=False):
        """发一个请求（含最多 `max_retries` 次重试），返回响应。

        重试都计入请求数；预算用完时抛 `RequestBudgetExceeded`，重试也不再发。
        最后一次仍失败时抛 `FetchError`（由调用方记 `unverified`）。
        """
        attempt = 0
        while True:
            self._spend()
            self._throttle()
            self._last_started = self.clock()
            self.urls.append(url)
            try:
                if binary:
                    return self.fetcher.get_bytes(url)
                return self.fetcher.get(url)
            except FetchError:
                if attempt >= self.max_retries:
                    raise
                attempt += 1
                self.retries += 1
                self.sleep(self.backoff)

    def fetch_pdf_text(self, url, name="ann.pdf"):
        """下载 PDF 并抽文本；两步共用一个请求名额（PDF 下载算网络请求）。"""
        body = self.call(url, binary=True)
        try:
            return self.fetcher.pdftotext(body, name)
        except FetchError:
            raise
        except Exception as exc:  # fetcher 的实现细节不泄漏成别的异常
            raise FetchError(f"PDF 文本抽取失败：{exc}") from exc


# ---------------------------------------------------------------------------
# verify：官方日行情（深市 `1815_stock`）
# ---------------------------------------------------------------------------


def _block_rows(payload):
    """SZSE `ShowReport/data` 响应 → 行列表（容忍两种常见信封形状）。"""
    if isinstance(payload, list):
        blocks = payload
    else:
        raise InputError("官方日行情响应不是 JSON 数组")
    rows = []
    for block in blocks:
        if not isinstance(block, dict):
            continue
        data = block.get("data")
        if isinstance(data, list):
            rows.extend(item for item in data if isinstance(item, dict))
    return rows


def _row_text(row, *names):
    """取一行的若干候选字段（大小写不敏感），去空格；没有则空串。"""
    lowered = {str(key).lower(): value for key, value in row.items()}
    for name in names:
        value = lowered.get(name.lower())
        if value is not None and str(value).strip() != "":
            return str(value).strip()
    return ""


def _row_code(row):
    """行里的证券代码：`zqdm`（深市日行情）或 `code`/`pz`。"""
    text = _row_text(row, "zqdm", "code", "pz", "Zqdm", "Code")
    digits = re.sub(r"\D", "", text)
    return digits[-6:] if len(digits) >= 6 else ""


class OfficialRow:
    """某 ETF 在 E 当天的官方日行情行（只留核对要用的字段）。"""

    __slots__ = ("entity", "day", "qss", "ks", "raw_text")

    def __init__(self, entity, day, qss, ks, raw_text):
        self.entity = entity
        self.day = day
        self.qss = qss
        self.ks = ks
        self.raw_text = raw_text


def parse_official_rows(text, day, codes):
    """官方日行情响应文本 → `{code: OfficialRow}`（只留 `codes` 里的）。

    `qss`（前收）是核对锚点；`ks`（开盘）只写进证据标签，不参与判定。
    """
    try:
        payload = json.loads(text)
    except json.JSONDecodeError as exc:
        raise FetchError(f"官方日行情不是合法 JSON：{exc}") from exc
    wanted = {str(code) for code in codes}
    found = {}
    for row in _block_rows(payload):
        code = _row_code(row)
        if code not in wanted:
            continue
        qss = _row_text(row, "qss", "qianShou", "preClose")
        if not qss:
            continue
        found[code] = OfficialRow(
            entity=None,
            day=day,
            qss=qss,
            ks=_row_text(row, "ks", "kaiPan", "open"),
            raw_text=json.dumps(row, ensure_ascii=False, sort_keys=True),
        )
    return found


def official_url(day, page=1):
    return SZSE_OFFICIAL_URL_TEMPLATE.format(day=day, page=page)


def fetch_official_rows(budget, entity, effective_day):
    """按 E 取该 ETF 的官方日行情行；市场不支持时返回 None（调用方据此 fail-closed）。

    沪市没有历史日线接口（设计 Q9），返回 None 而不是编一个 URL。
    """
    if market_of(entity) != "sz":
        return None
    code = entity.split(".", 1)[1]
    for page in range(1, SZSE_OFFICIAL_MAX_PAGES + 1):
        text = budget.call(official_url(effective_day, page))
        # 一页里可能有该 ETF 的多行（同一 code 不应出现多次），取第一行。
        rows = parse_official_rows(text, effective_day, {code})
        if code in rows:
            row = rows[code]
            row.entity = entity
            return row
    return None


# ---------------------------------------------------------------------------
# verify：公告文本解析（纯函数，测试用 `scripts/testdata/` 的夹具）
# ---------------------------------------------------------------------------

DAY_RE = re.compile(r"(20\d{2}|19\d{2})\s*[-年/.]\s*(\d{1,2})\s*[-月/.]\s*(\d{1,2})")
DECIMAL_TEXT = r"\d+(?:\.\d+)?"


class AnnouncementTerms:
    """公告给出的核对字段（每 10 份 / 每份的金额，或折算比例）。

    - `amount_per10`：每 10 份派发的现金（元/10 份，与 `c1` 同口径）；
    - `amount_per_share`：每份派发的现金（元/份）；
    - `ratio`：折算/拆分比例，语义是 `新价 = 旧价 / ratio`；
    - `registration_day` / `ex_day`：权益登记日 / 除权除息日（`YYYY-MM-DD`）；
    - `mode`：`per10` / `per_share` / `ratio`；
    - `note`：解析依据（人或机读都靠它复核）。
    """

    __slots__ = (
        "mode",
        "amount_per10",
        "amount_per_share",
        "ratio",
        "registration_day",
        "ex_day",
        "note",
    )

    def __init__(
        self,
        mode="",
        amount_per10=None,
        amount_per_share=None,
        ratio=None,
        registration_day="",
        ex_day="",
        note="",
    ):
        self.mode = mode
        self.amount_per10 = amount_per10
        self.amount_per_share = amount_per_share
        self.ratio = ratio
        self.registration_day = registration_day
        self.ex_day = ex_day
        self.note = note

    def payable_per10(self):
        """公告给的现金，统一成元/10 份；没有现金条款时返回 None。"""
        if self.amount_per10 is not None:
            return self.amount_per10
        if self.amount_per_share is not None:
            return self.amount_per_share * 10
        return None

    def as_dict(self):
        def text(value):
            return None if value is None else decimal_text(value, DELTA_SCALE)

        return {
            "mode": self.mode,
            "amount_per10": text(self.amount_per10),
            "amount_per_share": text(self.amount_per_share),
            "ratio": text(self.ratio),
            "registration_day": self.registration_day,
            "ex_day": self.ex_day,
            "note": self.note,
        }


def _iso(day):
    return day.isoformat() if isinstance(day, date) else ""


def _parse_ratio(pair_text):
    """`10：1` / `1：0.4` / `1：10` 这类写法 → 折算比（新价 = 旧价 / ratio）。

    公告把比例写成「新份数：旧份数」，所以 `ratio = first / second`：

    - `按 10：1 拆分、增加基金份额`（`sh.510180` 2009-05-20）：1 份拆成 10 份，
      价格 ÷ 10 → `c3 = 10`。VM2-23 把它读成 0.1，`.day` 的前收 6.18 与 X 当天
      开盘 0.612 都说明 `c3 = 10`；
    - `1：0.4`（`sz.159922` 2024-12-02）：0.4 份并入 1 份，价格 ÷ 2.5 → `c3 = 2.5`；
    - `1：10 合并、减少基金份额`：10 份并为 1 份，价格 ×10 → `c3 = 0.1`。

    方向词只用来**校验**读出来的方向：拆分要求 `c3 > 1`、合并要求 `c3 < 1`。
    方向词与数值互相矛盾时宁可不认（ratio=None），也不把方向猜反——错方向会让
    前复权序列整段错一倍，比 `unverified` 难发现得多。
    """
    match = re.search(r"(\d+(?:\.\d+)?)\s*[:：]\s*(\d+(?:\.\d+)?)", pair_text)
    if match is None:
        return None, ""
    first = Decimal(match.group(1))
    second = Decimal(match.group(2))
    if first == 0 or second == 0:
        return None, "比例为 0，无法判断"
    ratio = first / second
    split = "拆" in pair_text or "增加份额" in pair_text or "份额增加" in pair_text
    merge = "合并" in pair_text or "减少份额" in pair_text or "份额减少" in pair_text
    if split and not merge:
        if ratio <= 1:
            return None, f"写的是拆分但比例 {decimal_text(first)}:{decimal_text(second)} 不大于 1"
        return ratio, "拆分（份额增加），c3 = 新/旧"
    if merge and not split:
        if ratio >= 1:
            return None, f"写的是合并但比例 {decimal_text(first)}:{decimal_text(second)} 不小于 1"
        return ratio, "合并（份额减少），c3 = 新/旧"
    if split and merge:
        return None, "同时写了拆分与合并，方向不明"
    return ratio, "无方向词，按「新：旧」解读"


def _first_day_after_label(normalized, labels):
    """标签后第一个日期（同一行内）。"""
    for label in labels:
        index = normalized.find(label)
        if index < 0:
            continue
        window = normalized[index : index + 80]
        found = DAY_RE.search(window)
        if found is not None:
            return _day_from_match(found)
    return ""


def _day_from_match(match):
    year, month, day = int(match.group(1)), int(match.group(2)), int(match.group(3))
    try:
        return date(year, month, day).isoformat()
    except ValueError:
        return ""


def parse_announcement_text(text):
    """公告文本 → [`AnnouncementTerms`]（纯函数）。

    认得的写法（都来自 VM2-21 / VM2-23 的公告原文）：

    - `每 10 份基金份额派发…2.732 元` / `每10份派发现金差额0.0661元` → 元/10 份；
    - `每份基金份额派发 0.0129 元` → 元/份（×10 得到与 `c1` 同口径的值）；
    - `按 10：1 拆分、增加基金份额` / `10:1` / `1：0.1` → 折算比（方向看「拆分/合并」）；
    - 全角小数点 `42．28` 当作 `42.28`（`sh.511880` 2014-12-30 的解析错误就是它）；
    - `权益登记日` / `权益登记日及除息日` / `除权日` / `除息日`。

    认不出金额、比例或除权日的，返回的字段留空，由调用方记 `unverified` 并写明
    缺什么——**绝不猜**。
    """
    normalized = (text or "").translate(FULLWIDTH_TRANSLATION)
    terms = AnnouncementTerms()

    per10 = re.search(
        rf"每\s*(\d+)\s*份[^。;\n]{{0,40}}?({DECIMAL_TEXT})\s*元", normalized
    )
    per_share = re.search(
        rf"每\s*份[^。;\n]{{0,40}}?({DECIMAL_TEXT})\s*元", normalized
    )
    if per10 is not None:
        shares = Decimal(per10.group(1))
        amount = Decimal(per10.group(2))
        if shares == 10:
            terms.mode = "per10"
            terms.amount_per10 = amount
            terms.note = "每 10 份派发"
        elif shares != 0:
            # 写的是「每 N 份」，换算到 10 份口径。
            terms.mode = "per10"
            terms.amount_per10 = amount * 10 / shares
            terms.note = f"每 {shares} 份派发，按比例折成 10 份"
    elif per_share is not None:
        terms.mode = "per_share"
        terms.amount_per_share = Decimal(per_share.group(1))
        terms.note = "每份派发"

    if terms.mode == "":
        # 份额折算/拆分：取「拆分/折算/合并」词所在句子里的比例写法。
        refused = ""
        for keyword in ("拆分", "折算", "合并", "增加基金份额", "份额"):
            index = normalized.find(keyword)
            if index < 0:
                continue
            sentence = normalized[max(0, index - 60) : index + 80]
            ratio, note = _parse_ratio(sentence)
            if ratio is not None:
                terms.mode = "ratio"
                terms.ratio = ratio
                terms.note = note
                break
            if ratio is None and note:
                refused = note
        if terms.mode == "" and refused:
            # 写明了方向但与数值矛盾：不认，也不退回「无方向词」的猜测。
            terms.note = refused
            terms.registration_day = _first_day_after_label(
                normalized, ("权益登记日", "登记日")
            )
            terms.ex_day = _first_day_after_label(
                normalized, ("除权除息日", "除息日", "除权日", "折算日", "拆分日")
            )
            return terms
        if terms.mode == "":
            # 没有关键词时只看「N:M」形状的比例，且要求同一句里没有「元」。
            for match in re.finditer(
                r"(\d+(?:\.\d+)?)\s*[:：]\s*(\d+(?:\.\d+)?)", normalized
            ):
                sentence = normalized[
                    max(0, match.start() - 40) : match.end() + 40
                ]
                if "元" in sentence:
                    continue
                ratio, note = _parse_ratio(match.group(0))
                if ratio is not None:
                    terms.mode = "ratio"
                    terms.ratio = ratio
                    terms.note = note
                    break

    terms.registration_day = _first_day_after_label(
        normalized, ("权益登记日", "登记日")
    )
    terms.ex_day = _first_day_after_label(
        normalized, ("除权除息日", "除息日", "除权日", "折算日", "拆分日")
    )
    return terms


def announcement_factor(terms, category):
    """公告条款 → `(category, c1, c3)`；公告没有所需数值时返回 None。"""
    if terms is None:
        return None
    if category == 1:
        per10 = terms.payable_per10()
        if per10 is None:
            return None
        return (1, per10, Decimal(0))
    if category == 11:
        if terms.ratio is None or terms.ratio == 0:
            return None
        return (11, Decimal(0), terms.ratio)
    return None


def announced_value_text(terms, category):
    """记录里的 `announced_value`：公告给的原始数值（与 `gbbq_value` 同口径）。

    cat1 写元/10 份（与 `c1` 同口径），cat11 写折算比。
    """
    factor = announcement_factor(terms, category)
    if factor is None:
        return ""
    _category, c1, c3 = factor
    value = c1 if category == 1 else c3
    return decimal_text(value, DELTA_SCALE)


# ---------------------------------------------------------------------------
# verify：公告索引（沪市 `commonQuery.do` / 深市本地公告索引）
# ---------------------------------------------------------------------------


class NoticeCandidate:
    """一条公告候选：日期、标题、PDF URL。"""

    __slots__ = ("day", "title", "url", "source_label")

    def __init__(self, day, title, url, source_label):
        self.day = day
        self.title = title
        self.url = url
        self.source_label = source_label

    def as_dict(self):
        return {
            "day": self.day,
            "title": self.title,
            "url": self.url,
            "source": self.source_label,
        }


#: 标题里能说明这是一份「分红/折算」公告的关键词；用来在候选里挑重点。
NOTICE_KEYWORDS = ("分红", "收益分配", "派发", "折算", "拆分", "合并", "份额")


class NoticeIndex:
    """公告候选的来源。

    沪市：线上去 `commonQuery.do`（`sqlId=COMMON_PL_JJXX_JJGG_L`）。
    深市：读本地只读公告索引 `szse_etf_notice_ann`（来源库，**不写**）。
    两者都可以换成假实现（测试就这么做）。
    """

    def __init__(self, budget=None, szse_store_path=None):
        self.budget = budget
        self.szse_store_path = szse_store_path
        self._szse_rows = None

    # -- 沪市 ----------------------------------------------------------------
    def sse_candidates(self, entity, effective_day):
        if self.budget is None:
            return []
        code = entity.split(".", 1)[1]
        day = parse_day(effective_day)
        if day is None:
            return []
        start = (day - timedelta(days=SSE_ANNOUNCEMENT_LOOKBACK_DAYS)).isoformat()
        end = (day + timedelta(days=SSE_ANNOUNCEMENT_LOOKAHEAD_DAYS)).isoformat()
        text = self.budget.call(
            SSE_ANNOUNCEMENT_URL_TEMPLATE.format(
                page=1, code=code, start=start, end=end
            )
        )
        return parse_sse_notices(text, "sse_ann")

    # -- 深市 ----------------------------------------------------------------
    def szse_candidates(self, entity, effective_day):
        rows = self._load_szse_rows()
        day = parse_day(effective_day)
        if day is None:
            return []
        start = (day - timedelta(days=SZSE_NOTICE_LOOKBACK_DAYS)).isoformat()
        end = (day + timedelta(days=SZSE_NOTICE_LOOKAHEAD_DAYS)).isoformat()
        code = entity.split(".", 1)[1]
        found = []
        for row in rows:
            if row["code"] != code:
                continue
            if not (start <= row["day"] <= end):
                continue
            found.append(
                NoticeCandidate(
                    row["day"], row["title"], row["url"], "szse_etf_notice_ann"
                )
            )
        found.sort(key=lambda item: (item.day, item.url))
        return found

    def _load_szse_rows(self):
        """读本地公告索引（只读）。找不到索引返回空列表，不编造候选。"""
        if self._szse_rows is not None:
            return self._szse_rows
        self._szse_rows = []
        path = self.szse_store_path
        if not path or not os.path.exists(path):
            return self._szse_rows
        if os.path.isdir(path):
            manifests = [
                os.path.join(path, name)
                for name in ("manifest.jsonl", "index.jsonl")
                if os.path.exists(os.path.join(path, name))
            ]
        else:
            manifests = [path]
        rows = []
        for manifest in manifests:
            rows.extend(_read_szse_notice_manifest(manifest))
        self._szse_rows = rows
        return rows


def _read_szse_notice_manifest(path):
    """从 `manifest.jsonl` 里取 `szse_etf_notice_ann` 的行（只看标题与 URL）。"""
    rows = []
    try:
        handle = open(path, "r", encoding="utf-8")
    except OSError as exc:
        raise InputError(f"公告索引打不开：{path}（{exc}）") from exc
    with handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                entry = json.loads(line)
            except json.JSONDecodeError:
                continue
            if entry.get("source_id") != "szse_etf_notice_ann":
                continue
            url = str(entry.get("url") or entry.get("doc_url") or "")
            title = str(entry.get("title") or entry.get("name") or "")
            code = str(entry.get("code") or "")
            day = str(entry.get("announcement_date") or entry.get("publish_time") or "")[
                :10
            ]
            if not code:
                # 没有代码时就只留一个占位：下层的匹配靠代码，跳过不代表丢事件。
                continue
            if parse_day(day) is None or not url:
                continue
            rows.append(
                {
                    "code": code[-6:],
                    "day": day,
                    "title": title,
                    "url": url,
                }
            )
    return rows


def parse_sse_notices(text, source_label):
    """SSE `commonQuery.do` 响应 → 公告候选（只要 PDF URL 与日期）。"""
    try:
        payload = json.loads(text)
    except json.JSONDecodeError as exc:
        raise FetchError(f"公告索引不是合法 JSON：{exc}") from exc
    rows = []
    items = payload.get("result") if isinstance(payload, dict) else None
    if not isinstance(items, list):
        page_help = payload.get("pageHelp") if isinstance(payload, dict) else None
        items = page_help.get("data") if isinstance(page_help, dict) else None
    if not isinstance(items, list):
        return rows
    for item in items:
        if not isinstance(item, dict):
            continue
        url = str(item.get("URL") or item.get("url") or "").strip()
        stamp = str(item.get("SSEDATE") or item.get("DATE") or item.get("date") or "")[
            :10
        ]
        title = str(item.get("TITLE") or item.get("title") or "").strip()
        if not url or parse_day(stamp) is None:
            continue
        rows.append(NoticeCandidate(stamp, title, url, source_label))
    rows.sort(key=lambda item: (item.day, item.url))
    return rows


# ---------------------------------------------------------------------------
# verify：事件选择（只处理缺记录的事件）
# ---------------------------------------------------------------------------


def load_existing_records(path):
    """读已有记录 → `{(entity, ordinal, evidence_digest)}` 集合。

    接受两种输入：

    - JSONL（`etf-factor-checks-v1` 记录，一行一条；每行都要能过校验）；
    - 来源库目录（读 `manifest.jsonl` 里 `source_id=etf_factor_checks` 的行，
      从 `origin=etf-factor-checks:<sha>` 取记录 id；内容在 blob 里，读不到就
      由调用方显式传入 blob 目录）。

    `None` 表示「没有已有记录」。
    """
    if not path:
        return set()
    if os.path.isdir(path):
        return _existing_from_store(path)
    if not os.path.exists(path):
        raise InputError(f"已有记录文件不存在：{path}")
    keys = set()
    with open(path, "r", encoding="utf-8") as handle:
        for line_no, line in enumerate(handle, start=1):
            text = line.strip()
            if not text:
                continue
            try:
                record = json.loads(text)
            except json.JSONDecodeError as exc:
                raise InputError(f"{path}:{line_no}: 不是 JSON 记录（{exc}）") from exc
            if not isinstance(record, dict):
                raise InputError(f"{path}:{line_no}: 记录不是对象")
            key = record_key(record)
            if key is None:
                raise InputError(
                    f"{path}:{line_no}: 记录缺 entity/ordinal/evidence_digest"
                )
            keys.add(key)
    return keys


def record_key(record):
    """记录的匹配键 `(entity, ordinal, evidence_digest)`（小写 hex）。"""
    try:
        entity = str(record["entity"])
        ordinal = int(record["ordinal"])
        digest = str(record["evidence_digest"]).lower()
    except (KeyError, TypeError, ValueError):
        return None
    return (entity, ordinal, digest)


def _existing_from_store(store_path):
    """从来源库目录读已有记录的键（记录内容在 blob 里）。"""
    manifest = os.path.join(store_path, "manifest.jsonl")
    if not os.path.exists(manifest):
        raise InputError(f"来源库目录里没有 manifest.jsonl：{store_path}")
    blobs = [
        os.path.join(store_path, name)
        for name in ("blobs", "blob", "objects")
        if os.path.isdir(os.path.join(store_path, name))
    ]
    if not blobs:
        raise InputError(
            f"来源库目录里没有 blob 目录：{store_path}（"
            "可以直接传入已导出的 JSONL）"
        )
    keys = set()
    with open(manifest, "r", encoding="utf-8") as handle:
        for line in handle:
            text = line.strip()
            if not text:
                continue
            try:
                entry = json.loads(text)
            except json.JSONDecodeError:
                continue
            origin = str(entry.get("origin") or "")
            if not origin.startswith("etf-factor-checks:"):
                continue
            sha = origin.split(":", 1)[1]
            body = _read_blob(blobs, sha)
            if body is None:
                continue
            try:
                record = json.loads(body.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError):
                continue
            key = record_key(record)
            if key is not None:
                keys.add(key)
    return keys


def _read_blob(directories, sha):
    for directory in directories:
        for name in (sha, f"{sha}.json"):
            path = os.path.join(directory, name)
            if os.path.exists(path):
                with open(path, "rb") as handle:
                    return handle.read()
        # 分片布局：`<dir>/<sha[:2]>/<sha>`
        nested = os.path.join(directory, sha[:2], sha)
        if os.path.exists(nested):
            with open(nested, "rb") as handle:
                return handle.read()
    return None


def load_bars(conn, entities):
    """读 ETF 不复权日线（只读）：返回 `{entity: [(epoch_day, close, price_scale)]}`。

    价格是库里存的整数，单位由 `market_series.price_scale` 给（ETF 不复权是 3，
    即 `raw / 1000 = 元`）。标志位逐行带出来，调用方不能默认 2 位小数。
    表不认识（老 schema / 没有 ETF 行）时返回空 dict：核对会因此记 `unverified`
    并写明「没有前收」，而不是把事件当成没问题。
    """
    tables = {
        row[0]
        for row in conn.execute("SELECT name FROM sqlite_master WHERE type='table'")
    }
    if not {"market_series", "series_bars"}.issubset(tables):
        return {}
    columns = _columns(conn, "market_series")
    has_class = "instrument_class" in columns
    has_scale = "price_scale" in columns
    scale_expr = "m.price_scale" if has_scale else "2"
    bars = defaultdict(list)
    ordered = sorted(entities)
    for start in range(0, len(ordered), ENTITY_BATCH):
        batch = ordered[start : start + ENTITY_BATCH]
        placeholders = ",".join("?" for _ in batch)
        statement = (
            f"SELECT m.entity, b.trade_day, b.close_cents, {scale_expr} "
            "FROM market_series m JOIN series_bars b ON b.series_key = m.series_key "
            "WHERE m.period = 1 AND m.adjustment = 0 "
        )
        params = tuple(batch)
        if has_class:
            statement += "AND m.instrument_class = ? "
            params = (ETF_INSTRUMENT_CLASS,) + params
        statement += f"AND m.entity IN ({placeholders}) ORDER BY m.entity, b.trade_day"
        for entity, trade_day, close, scale in conn.execute(statement, params):
            bars[entity].append((int(trade_day), int(close), int(scale)))
    return dict(bars)


def previous_close(bars, effective_day):
    """E 之前最后一根 bar 的收盘价（厘 → 元，`Decimal`）；没有则 None。"""
    if not bars:
        return None
    limit = epoch_day(effective_day)
    best = None
    for day, close, scale in bars:
        if day < limit:  # 严格早于 E（与 PR-F 的 `bar_date < E` 一致）
            best = (day, close, scale)
        else:
            break
    if best is None:
        return None
    return Decimal(best[1]) / Decimal(10) ** best[2]


def select_pending_events(db_events, existing_keys, since=None):
    """挑出需要核对的事件：`(entity, ordinal, evidence_digest)` 在已有记录里没有匹配。

    - **只处理缺记录的**：命中已有键的事件一律不处理（已有记录就是证据）；
    - **`evidence_digest` 变了就是新事件**：键里有摘要，旧记录不再匹配；
    - `since` 只限制更早的事件（`effective_day < since` 的不处理，写进报告）；
    - `no_effect`（首根 bar 之前或不影响序列）不核对（D-91 §3），但也算「已处理」，
      写进报告而不是静默跳过。
    """
    pending = []
    skipped = {"already_checked": [], "before_since": [], "no_effect": []}
    since_day = parse_day(since) if since else None
    for event in db_events:
        key = (event["entity"], event["ordinal"], event["evidence_digest"])
        if key in existing_keys:
            skipped["already_checked"].append(key[0] + "#" + str(key[1]))
            continue
        if event.get("skip_reason"):
            skipped[event["skip_reason"]].append(
                {
                    "entity": event["entity"],
                    "ordinal": event["ordinal"],
                    "effective_day": event["effective_day"],
                    "category": event["category"],
                }
            )
            continue
        if since_day is not None and parse_day(event["effective_day"]) < since_day:
            skipped["before_since"].append(
                {
                    "entity": event["entity"],
                    "ordinal": event["ordinal"],
                    "effective_day": event["effective_day"],
                }
            )
            continue
        pending.append(event)
    pending.sort(key=lambda item: (item["entity"], item["effective_day"], item["ordinal"]))
    return pending, skipped


# ---------------------------------------------------------------------------
# verify：逐事件判定
# ---------------------------------------------------------------------------


def market_label(entity):
    return {"sh": "sse_ann", "sz": "szse_ann"}.get(market_of(entity), "ann")


def _verdict(record, status, basis, basis_ref, delta=None):
    """把判定写进记录（basis=none 时清空证据字段）。"""
    record["check_status"] = status
    record["basis"] = basis
    record["basis_ref"] = basis_ref
    if delta is not None:
        record["price_delta"] = decimal_text(delta, DELTA_SCALE)
        record["within_one_tick"] = within_one_tick(delta)
    if basis == "none":
        record["evidence_url"] = ""
        record["evidence_sha256"] = ""
    return record


def _unverified(record, reason, basis="none"):
    """核对不了的事件：`unverified` + 原因，**不跳过**。"""
    return _verdict(record, "unverified", basis, reason)


def decide_with_official(record, row, prev_close, factor):
    """官方 `qss` 核对（深市）：`qss` 与用 gbbq 因子算出的复权价相差 ≤ 0.001 元。"""
    official = parse_decimal(row.qss, "qss")
    if official is None:
        return _unverified(record, "official_qss_unparsable")
    record["basis"] = "official_qss"
    record["basis_ref"] = f"szse_1815_stock:{row.day}"
    record["evidence_url"] = official_url(row.day)
    record["evidence_sha256"] = sha256_hex(row.raw_text.encode("utf-8"))
    record["announced_value"] = decimal_text(official, DELTA_SCALE)
    if prev_close is None or factor is None:
        record["basis_ref"] += ";no_prev_close"
        return _unverified(record, "no_prev_close", "official_qss")
    delta = official - compose_price(prev_close, [factor])
    record["price_delta"] = decimal_text(delta, DELTA_SCALE)
    record["within_one_tick"] = within_one_tick(delta)
    if within_one_tick(delta):
        return _verdict(
            record,
            "verified_official",
            "official_qss",
            f"szse_1815_stock:{row.day}",
        )
    record["basis_ref"] += ";delta_over_tick"
    return _unverified(record, "official_delta_over_tick", "official_qss")


def decide_with_announcement(record, candidate, terms, prev_close, factor, category):
    """公告核对：金额/比例逐项一致（D-92）；能算出价格时再叠加 1 tick 判定。

    价格算得出且相差 > 1 tick → `unverified`；算不出（缺前收/因子）但金额与
    日期对得上 → 仍记 `verified_announcement`（与 `convert` 判 `verified_announcement`
    的口径一致：那条路径的判定依据是公告本身，不是一个价差）。
    """
    label = market_label(record["entity"])
    basis_ref = f"{label}:{candidate.day}"
    record["basis"] = "announcement"
    record["basis_ref"] = basis_ref
    record["evidence_url"] = candidate.url
    record["announced_value"] = announced_value_text(terms, category)
    announced = announcement_factor(terms, category)
    if announced is None:
        return _unverified(record, "announcement_no_value", "announcement")
    if prev_close is None or factor is None:
        return _verdict(record, "verified_announcement", "announcement", basis_ref)
    delta = compose_price(prev_close, [factor]) - compose_price(prev_close, [announced])
    if within_one_tick(delta):
        return _verdict(record, "verified_announcement", "announcement", basis_ref, delta)
    # 价差算出来了但不通过：保留价差作为「为什么不通过」的证据。
    record["price_delta"] = decimal_text(delta, DELTA_SCALE)
    record["within_one_tick"] = False
    return _unverified(record, "announcement_delta_over_tick", "announcement")


def decide_composite(record, events, prev_close, evidence):
    """同一个 E 上的多条事件：先 cat11 后 cat1 合并计算（D-92）。

    `evidence` 是 `(basis, basis_ref, evidence_url, evidence_sha256, announced_value,
    official_value)`；`official_value` 是官方 `qss`（没有时 None）。公告条款的
    复合核对留给报告标注：公告只写单项，用它算不出合并值。
    """
    basis, basis_ref, url, sha, announced_text, official = evidence
    record["basis"] = basis
    record["basis_ref"] = basis_ref
    record["evidence_url"] = url
    record["evidence_sha256"] = sha
    record["announced_value"] = announced_text
    factors = [gbbq_factor(event) for event in events]
    if official is None or prev_close is None or any(item is None for item in factors):
        return _unverified(record, "same_E_composite_unusable", basis)
    delta = official - compose_price(prev_close, factors)
    record["price_delta"] = decimal_text(delta, DELTA_SCALE)
    record["within_one_tick"] = within_one_tick(delta)
    if within_one_tick(delta):
        return _verdict(record, "verified_official", basis, basis_ref + ";same_E_composite")
    return _unverified(record, "same_E_composite_over_tick", basis)


# ---------------------------------------------------------------------------
# verify：主流程
# ---------------------------------------------------------------------------


def plan_requests(grouping):
    """算出将要发出的请求（`--dry-run` 用）：数量与 URL 模板，**不联网**。

    `grouping` 是 `{entity: [每组的第一个事件]}`——同一个 E 上的事件共用一次请求。
    """
    planned = []
    per_market = defaultdict(int)
    for entity in sorted(grouping):
        market = market_of(entity)
        code = entity.split(".", 1)[1]
        for event in grouping[entity]:
            if market == "sz":
                templates = [
                    _SZSE_OFFICIAL_TEMPLATE_TEMPLATE,
                    "szse_etf_notice_ann:<code, E-5..E+60> → PDF",
                ]
                kind = "official_then_announcement"
            else:
                templates = [
                    _SSE_ANNOUNCEMENT_TEMPLATE_TEMPLATE,
                    "<announcement URL> → PDF",
                ]
                kind = "announcement"
            planned.append(
                {
                    "kind": kind,
                    "entity": entity,
                    "effective_day": event["effective_day"],
                    "url_templates": templates,
                }
            )
            per_market[market] += 1
    return planned, dict(sorted(per_market.items()))


def verify(
    db_path,
    existing_path,
    fetcher=None,
    checked_at=None,
    max_requests=DEFAULT_MAX_REQUESTS,
    dry_run=False,
    since=None,
    notice_index=None,
    sleep=None,
    clock=None,
    min_interval=MIN_REQUEST_INTERVAL_SECONDS,
    backoff=RETRY_BACKOFF_SECONDS,
):
    """跑一次每周核对：返回 `(records, report)`，不写文件、不写来源库。

    `fetcher` 给了就用它（测试的假实现）；`dry_run=True` 时**一次也不发请求**。
    """
    if fetcher is None and not dry_run:
        fetcher = HttpFetcher()
    checked_at = checked_at or datetime.now(timezone.utc).date().isoformat()
    if parse_day(checked_at) is None:
        raise InputError(f"--checked-at {checked_at!r} 不是 YYYY-MM-DD")
    if since and parse_day(since) is None:
        raise InputError(f"--since {since!r} 不是 YYYY-MM-DD")

    existing_keys = load_existing_records(existing_path)
    conn = open_read_only(db_path)
    try:
        etf_entities, _columns_seen, selected = read_db_events(conn)
        db_events = fetch_events(conn, selected, etf_entities)
        if not db_events:
            db_events = fetch_events(
                conn, selected, {row[0] for row in conn.execute(
                    "SELECT DISTINCT entity FROM adjustment_events"
                )}
            )
        bars = load_bars(conn, {event["entity"] for event in db_events})
    finally:
        conn.close()

    # 首根 bar 之前的事件不影响序列，不纳入核对（D-91 §3）。
    for event in db_events:
        entity_bars = bars.get(event["entity"], [])
        if entity_bars and epoch_day(event["effective_day"]) <= entity_bars[0][0]:
            event["skip_reason"] = "no_effect"

    pending, skipped = select_pending_events(db_events, existing_keys, since)
    # 以「实体 + E」为单位分组：同一个 E 上的事件必须合并核对（D-92）。
    by_day = defaultdict(list)
    for event in pending:
        by_day[(event["entity"], event["effective_day"])].append(event)
    for group in by_day.values():
        group.sort(key=lambda item: (item["category"] != 11, item["ordinal"]))
    grouping = defaultdict(list)
    for (_entity, _day), group in sorted(by_day.items()):
        grouping[group[0]["entity"]].append(group[0])

    planned, per_market = plan_requests(grouping)
    budget = RequestBudget(
        fetcher,
        max_requests=max_requests,
        min_interval=min_interval,
        backoff=backoff,
        sleep=sleep,
        clock=clock,
    )
    index = notice_index or NoticeIndex(budget=budget, szse_store_path=existing_path)
    index.budget = budget

    records = []
    unprocessed = []
    results = []

    if dry_run:
        # `--dry-run` 一个请求都不发（fetcher 为 None 时连 HttpFetcher 都不建）。
        unprocessed = [event for event in pending]
    else:
        for (_entity, _day), group in sorted(by_day.items(), key=lambda item: (item[0][0], item[0][1])):
            try:
                outcome = _verify_group(
                    group, bars.get(_entity, []), index, budget, checked_at
                )
            except RequestBudgetExceeded:
                # 预算用完：当前组与剩下的组都不再发请求，全部写进报告。
                remaining = [
                    event
                    for (entity, day), events in sorted(by_day.items())
                    if (entity, day) >= (_entity, _day)
                    for event in events
                ]
                unprocessed.extend(remaining)
                break
            results.extend(outcome)

        if results:
            records = [item["record"] for item in results]

    records.sort(
        key=lambda item: (item["entity"], item["effective_day"], item["category"], item["ordinal"])
    )
    report = _verify_report(
        records,
        results,
        pending,
        unprocessed,
        skipped,
        db_events,
        existing_keys,
        budget,
        planned,
        per_market,
        checked_at,
        dry_run,
    )
    return records, report


class _OfficialEvidence:
    """一次官方 `qss` 核对的证据（`OfficialRow` 加上记录要写的字段）。"""

    __slots__ = ("basis_ref", "url", "sha256", "value")

    def __init__(self, basis_ref, url, sha256, value):
        self.basis_ref = basis_ref
        self.url = url
        self.sha256 = sha256
        self.value = value


def _official_evidence(row):
    return _OfficialEvidence(
        basis_ref=f"szse_1815_stock:{row.day}",
        url=official_url(row.day),
        sha256=sha256_hex(row.raw_text.encode("utf-8")),
        value=parse_decimal(row.qss, "qss"),
    )


def _verify_group(group, entity_bars, index, budget, checked_at):
    """核对同一个 E 上的一组事件；返回 `[{record, reason, db_event}]`。

    - 单事件：先试官方 `qss`（深市），核不上再试公告；
    - 多事件（同一个 E）：只用官方 `qss` 合并核对（公告只写单项条款，合并不出来），
      核不上就整组记 `unverified` 并写明原因——一条都不跳过。
    """
    entity = group[0]["entity"]
    effective_day = group[0]["effective_day"]
    prev_close = previous_close(entity_bars, effective_day)

    official_row = None
    official_error = ""
    try:
        official_row = fetch_official_rows(budget, entity, effective_day)
    except FetchError as exc:
        official_error = str(exc)

    if len(group) > 1:
        return _verify_composite(group, official_row, official_error, prev_close, checked_at)

    event = group[0]
    record = _new_record(event, checked_at)
    factor = gbbq_factor(event)

    if official_row is not None:
        record = decide_with_official(record, official_row, prev_close, factor)
        if record["check_status"] != "unverified":
            return [{"record": record, "reason": record["basis_ref"], "db_event": event}]
        official_failure = record["basis_ref"]
        official_snapshot = dict(record)
    else:
        official_failure = "official_unavailable:" + official_error if official_error else ""
        official_snapshot = None

    chosen = None
    announcement_error = ""
    try:
        candidates = _announcement_candidates(index, event)
    except RequestBudgetExceeded:
        raise
    except (FetchError, InputError) as exc:
        # 公告索引取不到不是「事件没问题」：记进原因，事件仍是 unverified。
        announcement_error = "announcement_index_unavailable:" + str(exc)
        candidates = []
    for candidate in candidates:
        try:
            text = budget.fetch_pdf_text(candidate.url, name="ann.pdf")
        except FetchError:
            continue
        terms = parse_announcement_text(text)
        if announcement_factor(terms, event["category"]) is None:
            continue
        chosen = (candidate, terms, text)
        break

    if chosen is None:
        missing = announcement_error or "no_announcement_with_value"
        if official_snapshot is not None:
            record = official_snapshot
            record["basis_ref"] = official_failure + ";" + missing
        else:
            record = _unverified(record, official_failure or missing)
            if official_failure:
                record["basis_ref"] = official_failure + ";" + missing
        return [{"record": record, "reason": record["basis_ref"], "db_event": event}]

    candidate, terms, text = chosen
    record["evidence_sha256"] = _announcement_sha(candidate, text)
    record = decide_with_announcement(
        record, candidate, terms, prev_close, factor, event["category"]
    )
    if record["basis_ref"] and official_failure:
        # 官方核不上、公告核上了：报告的 reason 两者都写，便于复核。
        record["basis_ref"] = record["basis_ref"] + ";official=" + official_failure
    return [{"record": record, "reason": record["basis_ref"], "db_event": event}]


def _new_record(event, checked_at):
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
    record["gbbq_value"] = event["c1"] if event["category"] == 1 else event["c3"]
    return record


def _verify_composite(group, official_row, official_error, prev_close, checked_at):
    """同一个 E 上的多条事件：合并核对（先 cat11 后 cat1）。"""
    out = []
    if official_row is None:
        reason = "same_E_composite_no_official"
        if official_error:
            reason = reason + ":" + official_error
        for event in group:
            record = _new_record(event, checked_at)
            record = _unverified(record, reason)
            out.append({"record": record, "reason": reason, "db_event": event})
        return out
    evidence = _official_evidence(official_row)
    announced = "" if evidence.value is None else decimal_text(evidence.value, DELTA_SCALE)
    bundle = (
        "official_qss",
        evidence.basis_ref,
        evidence.url,
        evidence.sha256,
        announced,
        evidence.value,
    )
    for event in group:
        record = _new_record(event, checked_at)
        record = decide_composite(record, group, prev_close, bundle)
        out.append({"record": record, "reason": record["basis_ref"], "db_event": event})
    return out


def _announcement_sha(candidate, text):
    """公告证据的 sha256。

    PDF 字节的 sha256 需要在下载时就算；这里用「PDF 文本 + URL」的摘要，
    只因 `FetchError` 时拿不到字节。测试与文档都写明这一点，导入器只要求 64 位
    hex，不会把它当成 PDF 字节的摘要。
    """
    return sha256_hex((candidate.url + "\n" + (text or "")).encode("utf-8"))


def _announcement_candidates(index, event):
    """按市场取公告候选（沪市线上、深市本地索引），按相关度排序后再按日期。"""
    market = market_of(event["entity"])
    if market == "sh":
        candidates = index.sse_candidates(event["entity"], event["effective_day"])
    elif market == "sz":
        candidates = index.szse_candidates(event["entity"], event["effective_day"])
    else:
        candidates = []
    return sorted(
        candidates,
        key=lambda item: (
            0 if any(word in item.title for word in NOTICE_KEYWORDS) else 1,
            item.day,
            item.url,
        ),
    )


def _verify_report(
    records,
    results,
    pending,
    unprocessed,
    skipped,
    db_events,
    existing_keys,
    budget,
    planned,
    per_market,
    checked_at,
    dry_run,
):
    validation_failures = []
    for record in records:
        errors = validate_record(record)
        if errors:
            validation_failures.append(
                {
                    "entity": record["entity"],
                    "ordinal": record["ordinal"],
                    "errors": errors,
                }
            )
    by_market = defaultdict(lambda: defaultdict(int))
    for record in records:
        by_market[market_of(record["entity"])][record["check_status"]] += 1
    processed_keys = {record_key(record) for record in records}
    unverified_entities = sorted(
        {record["entity"] for record in records if record["check_status"] == "unverified"}
    )
    unprocessed_rows = [
        {
            "entity": event["entity"],
            "ordinal": event["ordinal"],
            "effective_day": event["effective_day"],
            "category": event["category"],
            "evidence_digest": event["evidence_digest"],
            "reason": "request_budget_exhausted" if budget.requests >= budget.max_requests
            else "dry_run",
        }
        for event in unprocessed
        if (event["entity"], event["ordinal"], event["evidence_digest"]) not in processed_keys
    ]
    unprocessed_entities = sorted({row["entity"] for row in unprocessed_rows})
    return {
        "ok": not validation_failures and not unprocessed_rows,
        "dry_run": dry_run,
        "checked_at": checked_at,
        "db_events": len(db_events),
        "existing_records": len(existing_keys),
        "pending_events": len(pending),
        "records": len(records),
        "by_check_status": _count(records, lambda record: record["check_status"]),
        "by_basis": _count(records, lambda record: record["basis"]),
        "by_market_and_status": {
            market: dict(sorted(counts.items())) for market, counts in sorted(by_market.items())
        },
        "requests": {
            "count": budget.requests,
            "retries": budget.retries,
            "max": budget.max_requests,
            "min_interval_seconds": budget.min_interval,
            "backoff_seconds": budget.backoff,
            "urls": budget.urls,
        },
        "planned_requests": {
            "count": len(planned),
            "by_market": per_market,
            "rows": planned,
        },
        "skipped": {
            "already_checked": len(skipped["already_checked"]),
            "before_since": skipped["before_since"],
            "no_effect": skipped["no_effect"],
        },
        "unprocessed": {"count": len(unprocessed_rows), "rows": unprocessed_rows},
        "unverified_entities": unverified_entities,
        "results": results,
        "validation": {"records_checked": len(records), "failures": validation_failures},
        "evidence_note": (
            EVIDENCE_NOTE + " verify 生成的公告记录在拿不到 PDF 字节时，"
            "evidence_sha256 是「公告 URL + 抽取文本」的 sha256（不是 PDF 字节摘要）。"
        ),
    }


# ---------------------------------------------------------------------------
# 入口
# ---------------------------------------------------------------------------


def convert(events_v3_path, db_path, checked_at=None, allow_unmatched=False, overrides_path=None):
    """跑完整转换：返回 `(records, report)`，不写文件。

    `overrides_path` 是 `--overrides` 的人工裁定 JSON：逐条「带原文片段」的裁定，
    只给公告说的数，判定仍走 D-92。
    """
    checked_at = checked_at or datetime.now(timezone.utc).date().isoformat()
    if parse_day(checked_at) is None:
        raise InputError(f"--checked-at {checked_at!r} 不是 YYYY-MM-DD")
    overrides = load_overrides(overrides_path)
    v3_rows, v3_warnings = load_events_v3(events_v3_path)
    conn = open_read_only(db_path)
    try:
        etf_entities, _columns_seen, selected = read_db_events(conn)
        entities = etf_entities | {row["entity"] for row in v3_rows}
        db_events = fetch_events(conn, selected, entities)
    finally:
        conn.close()
    pairs, _unmatched = match_events(db_events, v3_rows)
    used_overrides, unused_overrides = apply_overrides(db_events, pairs, overrides)
    records, report = build_records(db_events, v3_rows, checked_at)
    report["warnings"].update(v3_warnings)
    not_etf = sorted({row["entity"] for row in v3_rows} - etf_entities)
    if etf_entities:
        report["warnings"]["entity_not_etf_in_db"] = [
            {"entity": entity} for entity in not_etf
        ]
    report["inputs"] = {"events_v3": os.path.abspath(events_v3_path), "db": os.path.abspath(db_path)}
    report["allow_unmatched"] = allow_unmatched
    report["overrides"] = {
        "path": os.path.abspath(overrides_path) if overrides_path else None,
        "loaded": len(overrides),
        "used": [
            overrides[key].as_dict() for key in sorted(used_overrides)
        ],
        # 没用到的裁定要报出来：可能是键写错了，静默忽略等于让人以为签过字。
        "unused": [
            overrides[key].as_dict() for key in unused_overrides
        ],
    }
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
    convert_parser.add_argument(
        "--overrides",
        help=(
            "人工裁定 JSON（带公告原文片段的逐条裁定）；只给公告说的数，"
            "判定仍走 D-92"
        ),
    )
    verify_parser = subparsers.add_parser(
        "verify",
        help="每周核对：只为没有匹配记录的新事件生成记录（不写来源库）",
    )
    verify_parser.add_argument("--db", required=True, help="行情库（只读打开）")
    verify_parser.add_argument(
        "--existing",
        help="已有记录：JSONL 文件，或来源库目录（读 manifest.jsonl + blob）",
    )
    verify_parser.add_argument("--out", required=True, help="输出新记录的 JSONL")
    verify_parser.add_argument("--report", help="输出 JSON 报告（含未处理的事件）")
    verify_parser.add_argument(
        "--max-requests",
        type=int,
        default=DEFAULT_MAX_REQUESTS,
        help=f"请求总数硬上限（默认 {DEFAULT_MAX_REQUESTS}；到上限立即停）",
    )
    verify_parser.add_argument(
        "--dry-run",
        action="store_true",
        help="只列出将要发出的请求（数量与 URL 模板），不联网",
    )
    verify_parser.add_argument("--since", help="只核对 E >= 该日期的事件（YYYY-MM-DD）")
    verify_parser.add_argument(
        "--checked-at",
        help="核对日期 YYYY-MM-DD（默认今天 UTC；固定它可以复跑出逐位相同的记录 id）",
    )
    verify_parser.add_argument(
        "--szse-store",
        help="深市公告本地只读索引（来源库目录或 manifest.jsonl）；默认与 --existing 相同",
    )
    verify_parser.add_argument(
        "--request-interval",
        type=float,
        default=MIN_REQUEST_INTERVAL_SECONDS,
        help=f"请求间隔秒数（不小于 {MIN_REQUEST_INTERVAL_SECONDS}）",
    )
    args = parser.parse_args(argv)

    if args.command == "verify":
        return _verify_main(args)
    if args.command != "convert":
        parser.error(f"未知子命令 {args.command!r}")

    try:
        records, report = convert(
            args.events_v3,
            args.db,
            args.checked_at,
            args.allow_unmatched,
            args.overrides,
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


def _verify_main(args):
    """`verify` 的命令行入口：写 JSONL 与报告，按结果给退出码。"""
    if args.max_requests < 0:
        print(json.dumps({"ok": False, "error": "--max-requests 不能为负"}, ensure_ascii=False))
        return EXIT_INPUT_ERROR
    interval = max(MIN_REQUEST_INTERVAL_SECONDS, float(args.request_interval or 0.0))
    try:
        records, report = verify(
            args.db,
            args.existing,
            checked_at=args.checked_at,
            max_requests=args.max_requests,
            dry_run=args.dry_run,
            since=args.since,
            notice_index=NoticeIndex(
                budget=None, szse_store_path=args.szse_store or args.existing
            ),
            min_interval=interval,
        )
    except InputError as exc:
        print(json.dumps({"ok": False, "error": str(exc)}, ensure_ascii=False))
        return EXIT_INPUT_ERROR

    failures = report["validation"]["failures"]
    if failures:
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

    if args.dry_run:
        summary = {
            "ok": True,
            "dry_run": True,
            "planned_requests": report["planned_requests"]["count"],
            "by_market": report["planned_requests"]["by_market"],
            "pending_events": report["pending_events"],
            "out": None,
        }
        print(json.dumps(summary, ensure_ascii=False))
        if args.report:
            write_report(args.report, report)
        return EXIT_OK

    # `--out` 为空串时只出报告（`--dry-run` 的占位）；否则写 JSONL。
    if args.out:
        write_jsonl(args.out, records)
    if args.report:
        write_report(args.report, report)
    summary = {
        "ok": report["ok"],
        "dry_run": False,
        "records": report["records"],
        "by_check_status": report["by_check_status"],
        "requests": report["requests"]["count"],
        "retries": report["requests"]["retries"],
        "unprocessed": report["unprocessed"]["count"],
        "unverified_entities": report["unverified_entities"],
        "out": os.path.abspath(args.out) if args.out else None,
    }
    print(json.dumps(summary, ensure_ascii=False))
    if report["unprocessed"]["count"]:
        return EXIT_NEEDS_REVIEW
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
