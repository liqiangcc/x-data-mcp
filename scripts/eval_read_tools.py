#!/usr/bin/env python3
"""Read-tool evaluation: the ten questions of docs/tasks/issue-525-read-tools.md §9.

Every question runs in a fresh ``pi --mode json -p --no-session`` process that
sees only the MCP tools of the chosen scope (no bash, no file tools). The
script counts turns, tokens, tool calls, failed tool calls and wall time from
pi's JSON event stream, then checks the final answer against values it reads
itself with ``query_data``. No market data is stored in the repository: the
expected values are fetched from the live service at run time, so the script
only makes sense on a host that serves the data.

Read-only: the script's own MCP client may call ``query_data`` and
``tools/list`` and nothing else, and the model gets research tools only.

Run (on the host that serves the data; ``<run>`` is the service run directory):

    python3 scripts/eval_read_tools.py --scope baseline \\
        --socket <run>/research.sock --model <provider/model> \\
        --output /tmp/read-tools-baseline.json
    python3 scripts/eval_read_tools.py --scope new \\
        --socket <run>/research.sock --model <provider/model> \\
        --output /tmp/read-tools-new.json

Add ``--mode codemode`` for the codemode run (see "Tool modes" below).
Use the same model, thinking level, ``--today`` and mode for runs you compare. The report
holds prices and the model's answers: keep it out of the repository.

Grade a saved report again (after a change to the checks) without a model and
without the service. The report must hold the expected values (``oracle`` per
question, saved by runs of this version); a task without them keeps its old
result and is marked ``regrade: skipped``:

    python3 scripts/eval_read_tools.py --regrade <report.json> --output <new.json>

Tool modes (``--mode``, recorded as ``mode`` at the top of the report):

``direct`` (default) gives the model the scope's MCP tools as separate tools
and nothing else (``exposure: direct``, ``--no-builtin-tools``). The model
cannot write scripts. This is what a client that only has direct calls sees.

``codemode`` is how pi is normally used: the MCP server keeps the default
exposure (``codemode``), pi starts with ``--tools codemode`` and the model's
only tool is ``codemode``, which runs a JavaScript script that calls the MCP
tools. ``--no-builtin-tools`` and ``--no-extensions`` are NOT passed: with them
the ``codemode`` tool does not exist. bash, read, edit and write are still not
given, so after the start ``declared_tools`` must be exactly ``["codemode"]``,
otherwise the question is ``scope_error``. The scope's tools are then only
checked against the service (pre-flight) and against the tools the scripts
call; they do not limit what a script can reach on the research socket.

In ``codemode`` mode the tool calls happen inside scripts. The top-level
``tool_calls``, ``tools`` and ``tool_errors`` of a question keep counting the
model's own calls, which are all ``codemode`` calls. The question also has a
``codemode`` object, and the summary (and each group) has the sums of it:

- ``calls``: scripts the model started (equals the top-level ``tool_calls``).
- ``script_failures``: scripts that failed (the ``codemode`` result has
  ``isError``: syntax error, thrown error, timeout, worker limit). The
  top-level ``tool_errors`` counts these too.
- ``tool_calls`` and ``tools``: MCP calls made inside scripts, read from the
  ``details.calls`` of each ``codemode`` result.
- ``tool_errors``: calls inside scripts that ended with ``isError`` or were
  rejected (call status ``error``), whether or not the script caught the
  error. A script that completed can have ``tool_errors`` > 0.
- ``unexpected_tool_calls``: tools called inside scripts that are not in the
  scope (not summed). Scope tools are not declared to the model in this mode,
  so the scope is checked only here and by the pre-flight against the service.
- ``results_without_details``: ``codemode`` results without a call list. Their
  inner calls are not counted, so non-zero means the counts are lower bounds.

Scopes (``.devin/evals/read-tools-v1.json``): ``baseline`` is the five current
research tools; ``new`` adds the ten new ones; ``new-no-query`` is ``new``
without ``query_data``.

Exit status: 0 when every question ran and was checked (a wrong answer is a
measurement, not a failure), 1 when any question hit an infrastructure
problem (pi failed, timeout, oracle query failed), 2 for bad arguments or a
failed pre-flight (a scope tool the service does not offer).
"""
from __future__ import annotations

import argparse
import copy
import datetime as dt
import json
import math
import os
import re
import select
import signal
import subprocess
import sys
import tempfile
import time
from decimal import Decimal, InvalidOperation
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_CONFIG = ROOT / ".devin/evals/read-tools-v1.json"
# Tools of the other entries (ops, maint): they write, run work or read task
# state. A scope naming one is a configuration error: this evaluation is
# read-only and uses the research tools only.
OUT_OF_SCOPE_TOOLS = frozenset(
    {
        "plan_market_update",
        "run_market_update",
        "start_collection",
        "update_data",
        "backfill_data",
        "republish_data",
        "retry_task",
        "cancel_task",
        "run_worker_once",
        "refresh_exchange_sources",
        "verify_data_gap",
        "compute_metrics",
        "get_task",
        "get_task_progress",
        "list_tasks",
        "get_request",
        "get_yearly_base_years",
        "verify_publication_result",
        "verify_stopped_writes",
        "inspect_verification_task",
    }
)
ORACLE_TOOLS = frozenset({"query_data"})
CST = dt.timezone(dt.timedelta(hours=8))  # Asia/Shanghai has no daylight saving time
ERROR_TEXT_LIMIT = 300
MODES = ("direct", "codemode")
CODEMODE_TOOL = "codemode"


class EvalError(Exception):
    """A problem with the setup or the oracle, not with the model's answer."""


# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------


def load_config(path: Path = DEFAULT_CONFIG) -> dict:
    config = json.loads(Path(path).read_text(encoding="utf-8"))
    for name, tools in config["scopes"].items():
        bad = sorted(set(tools) & OUT_OF_SCOPE_TOOLS)
        if bad:
            raise EvalError(f"scope {name!r} names tools outside the research entry: {bad}")
    ids = [task["id"] for task in config["tasks"]]
    if len(ids) != len(set(ids)):
        raise EvalError("duplicate task ids")
    for task in config["tasks"]:
        for oracle_name in {check["oracle"] for check in task["checks"] if "oracle" in check}:
            if oracle_name not in task["oracles"]:
                raise EvalError(f"{task['id']}: check names unknown oracle {oracle_name!r}")
    return config


def pi_tool_name(server: str, tool: str) -> str:
    """pi's name for an MCP tool: every character outside [A-Za-z0-9_] becomes _."""
    clean = lambda text: re.sub(r"[^A-Za-z0-9_]", "_", text)  # noqa: E731
    return f"mcp__{clean(server)}__{clean(tool)}"


_PLACEHOLDER = re.compile(r"\{today(?:-(\d+))?\}")


def substitute(value, today: dt.date, extra: dict | None = None):
    """Replace ``{today}``, ``{today-N}`` and ``{name}`` placeholders in any JSON value."""
    if isinstance(value, str):
        def today_repl(match):
            days = int(match.group(1) or 0)
            return (today - dt.timedelta(days=days)).isoformat()

        out = _PLACEHOLDER.sub(today_repl, value)
        for key, replacement in (extra or {}).items():
            out = out.replace("{" + key + "}", replacement)
        return out
    if isinstance(value, list):
        return [substitute(item, today, extra) for item in value]
    if isinstance(value, dict):
        return {key: substitute(item, today, extra) for key, item in value.items()}
    return value


# ---------------------------------------------------------------------------
# pi command and MCP project configuration
# ---------------------------------------------------------------------------


# Passed on to the MCP proxy when set in the caller's environment (dev and
# preview hosts that are not on a local file system need it, see AGENTS.md).
PASSTHROUGH_ENV = ("X_DATA_MCP_UNSAFE_NONLOCAL_FS",)


def mcp_project_config(server: str, mcp_connect: str, socket: str, environ=None, mode: str = "direct") -> dict:
    environ = os.environ if environ is None else environ
    entry = {
        "command": mcp_connect,
        "args": [socket],
        "timeout": 120,
    }
    if mode == "direct":
        entry["exposure"] = "direct"  # codemode: no key, pi's default exposure is codemode
    env = {name: environ[name] for name in PASSTHROUGH_ENV if name in environ}
    if env:
        entry["env"] = env
    return {"mcpServers": {server: entry}}


def build_pi_command(
    pi: str,
    allowed: list[str],
    system_prompt: str,
    prompt: str,
    model: str | None = None,
    thinking: str | None = None,
    mode: str = "direct",
) -> list[str]:
    """One fresh, non-persistent pi run with JSON events on stdout.

    ``direct``: ``--tools`` lists MCP tools only and ``--no-builtin-tools``
    removes bash, read, edit and write. ``--no-extensions -e builtin:mcp``
    keeps the MCP support and drops everything else the user may have installed.

    ``codemode``: ``--tools codemode`` is the only tool. ``--no-builtin-tools``
    and ``--no-extensions`` are left out on purpose: with them pi has no
    ``codemode`` tool. The run is checked through ``declared_tools``.
    """
    if mode not in MODES:
        raise EvalError(f"unknown mode {mode!r}; choose from {list(MODES)}")
    command = [pi, "--mode", "json", "-p", "--no-session", "--approve"]
    if mode == "direct":
        command += ["--no-extensions", "-e", "builtin:mcp", "--no-builtin-tools"]
    command += [
        "--no-context-files",
        "--no-skills",
        "--no-prompt-templates",
        "--no-themes",
        "--tools", ",".join(allowed) if mode == "direct" else CODEMODE_TOOL,
        "--system-prompt", system_prompt,
    ]
    if model:
        command += ["--model", model]
    if thinking:
        command += ["--thinking", thinking]
    return command + ["--", prompt]


# ---------------------------------------------------------------------------
# Statistics from pi's event stream or a session file
# ---------------------------------------------------------------------------


def iter_records(lines):
    for line in lines:
        line = line.strip()
        if not line:
            continue
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(record, dict):
            yield record


def messages_of(records) -> list[dict]:
    """Messages in order, from JSON-mode events (``message_end``) or a session
    file (``message`` entries). ``agent_end`` repeats them and is ignored."""
    out = []
    for record in records:
        if record.get("type") in ("message_end", "message") and isinstance(record.get("message"), dict):
            out.append(record["message"])
    return out


def text_of(content) -> str:
    if isinstance(content, str):
        return content
    return "".join(
        block.get("text", "")
        for block in content or []
        if isinstance(block, dict) and block.get("type") == "text"
    )


def declared_tools(messages) -> set[str]:
    """Replay the system messages' tool additions and removals."""
    tools: set[str] = set()
    for message in messages:
        if message.get("role") != "system":
            continue
        tools |= {tool["name"] for tool in message.get("toolsAdded", []) if "name" in tool}
        tools -= {tool["name"] for tool in message.get("toolsRemoved", []) if "name" in tool}
    return tools


def codemode_stats(messages, calls: dict[str, str], results: dict[str, dict], scope: set[str]) -> dict:
    """Counts of what the scripts did. ``details.calls`` of a ``codemode``
    toolResult lists the calls made inside the script (name, status, error)."""
    inner: dict[str, int] = {}
    inner_errors = 0
    without_details = 0
    for message in messages:
        if message.get("role") != "toolResult" or message.get("toolName") != CODEMODE_TOOL:
            continue
        listed = (message.get("details") or {}).get("calls")
        if not isinstance(listed, list):
            without_details += 1
            continue
        for call in listed:
            if not isinstance(call, dict):
                continue
            name = call.get("name", "")
            inner[name] = inner.get(name, 0) + 1
            if call.get("status") == "error":
                inner_errors += 1
    return {
        "calls": sum(1 for name in calls.values() if name == CODEMODE_TOOL),
        "script_failures": sum(
            1 for result in results.values() if result["tool"] == CODEMODE_TOOL and result["is_error"]
        ),
        "tool_calls": sum(inner.values()),
        "tools": dict(sorted(inner.items())),
        "tool_errors": inner_errors,
        "unexpected_tool_calls": sorted(set(inner) - scope),
        "results_without_details": without_details,
    }


def collect_stats(records, mode: str = "direct", scope: set[str] | None = None) -> dict:
    messages = messages_of(records)
    tokens = {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0}
    turns = 0
    model_errors = 0
    calls: dict[str, str] = {}  # tool call id -> tool name
    by_tool: dict[str, int] = {}
    results: dict[str, dict] = {}
    models: list[str] = []
    answer = ""
    stamps = []
    result_bytes = 0
    for message in messages:
        if isinstance(message.get("timestamp"), (int, float)):
            stamps.append(message["timestamp"] / 1000)
        role = message.get("role")
        if role == "assistant":
            usage = message.get("usage") or {}
            tokens["input"] += int(usage.get("input", 0) or 0)
            tokens["output"] += int(usage.get("output", 0) or 0)
            tokens["cache_read"] += int(usage.get("cacheRead", 0) or 0)
            tokens["cache_write"] += int(usage.get("cacheWrite", 0) or 0)
            if message.get("stopReason") in ("error", "aborted"):
                model_errors += 1
            else:
                turns += 1
                answer = text_of(message.get("content")).strip()
            label = "/".join(part for part in (message.get("provider"), message.get("model")) if part)
            if label and label not in models:
                models.append(label)
            for block in message.get("content") or []:
                if isinstance(block, dict) and block.get("type") == "toolCall":
                    name = block.get("name", "")
                    calls[block.get("id", f"call-{len(calls)}")] = name
                    by_tool[name] = by_tool.get(name, 0) + 1
        elif role == "toolResult":
            text = text_of(message.get("content"))
            result_bytes += len(text.encode("utf-8"))
            results[message.get("toolCallId", f"result-{len(results)}")] = {
                "tool": message.get("toolName", ""),
                "is_error": bool(message.get("isError")),
                "text": text,
            }
    errors = [
        {"tool": result["tool"], "message": result["text"][:ERROR_TEXT_LIMIT]}
        for result in results.values()
        if result["is_error"]
    ]
    tokens["total"] = sum(tokens.values())
    extra = {"codemode": codemode_stats(messages, calls, results, scope or set())} if mode == "codemode" else {}
    return {
        **extra,
        "turns": turns,
        "model_errors": model_errors,
        "tokens": tokens,
        "tool_calls": len(calls),
        "tool_errors": len(errors),
        "unanswered_tool_calls": len(set(calls) - set(results)),
        "tools": dict(sorted(by_tool.items())),
        "errors": errors,
        "tool_result_bytes": result_bytes,
        "answer": answer,
        "models": models,
        "declared_tools": sorted(declared_tools(messages)),
        "model_span_s": round(max(stamps) - min(stamps), 3) if len(stamps) > 1 else 0.0,
    }


# ---------------------------------------------------------------------------
# Reading the service: MCP client limited to read-only calls
# ---------------------------------------------------------------------------


class McpClient:
    """Minimal JSON-RPC-over-stdio MCP client, e.g. ``mcp_connect <socket>``."""

    def __init__(self, command: list[str], timeout: float = 120.0):
        self.timeout = timeout
        self.next_id = 1
        self.proc = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        self.request(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "eval-read-tools", "version": "1"},
            },
        )
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _send(self, message: dict) -> None:
        self.proc.stdin.write(json.dumps(message) + "\n")
        self.proc.stdin.flush()

    def request(self, method: str, params: dict):
        request_id = self.next_id
        self.next_id += 1
        self._send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        deadline = time.monotonic() + self.timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not select.select([self.proc.stdout], [], [], remaining)[0]:
                raise EvalError(f"MCP {method}: no answer within {self.timeout:g}s")
            line = self.proc.stdout.readline()
            if not line:
                raise EvalError(f"MCP {method}: connection closed")
            try:
                response = json.loads(line)
            except json.JSONDecodeError:
                continue
            if response.get("id") != request_id:
                continue
            if "error" in response:
                raise EvalError(f"MCP {method}: {response['error']}")
            return response.get("result")

    def list_tools(self) -> list[str]:
        return [tool["name"] for tool in self.request("tools/list", {})["tools"]]

    def call(self, name: str, arguments: dict) -> dict:
        if name not in ORACLE_TOOLS:
            raise EvalError(f"refusing to call {name!r}: the evaluation is read-only")
        result = self.request("tools/call", {"name": name, "arguments": arguments})
        if result.get("isError"):
            raise EvalError(f"{name} failed: {text_of(result.get('content'))[:ERROR_TEXT_LIMIT]}")
        if isinstance(result.get("structuredContent"), dict):
            return result["structuredContent"]
        try:
            return json.loads(text_of(result.get("content")))
        except json.JSONDecodeError as error:
            raise EvalError(f"{name}: response is not JSON ({error})") from error

    def close(self) -> None:
        try:
            self.proc.stdin.close()
            self.proc.wait(timeout=5)
        except Exception:  # noqa: BLE001 - best effort on shutdown
            self.proc.kill()


def envelope_rows(envelope: dict) -> list[dict]:
    """Flatten a v2 response (panel, cross_section or table) to one dict per row."""
    columns = envelope.get("columns") or {}
    layout = envelope.get("layout")
    entities = envelope.get("entities") or []
    rows: list[dict] = []
    if layout == "panel":
        for i, entity in enumerate(entities):
            dates = envelope["dates"][i]
            for j, date in enumerate(dates):
                row = {"entity": entity, "date": date}
                for field, grid in columns.items():
                    row[field] = grid[i][j]
                rows.append(row)
    elif layout == "cross_section":
        for d, day in enumerate(envelope.get("trade_days") or []):
            for e, entity in enumerate(entities):
                row = {"entity": entity, "date": day}
                for field, grid in columns.items():
                    row[field] = grid[d][e]
                rows.append(row)
    else:
        count = len(entities) if entities else int(envelope.get("row_count") or 0)
        for r in range(count):
            row = {"entity": entities[r]} if entities else {}
            for field, values in columns.items():
                row[field] = values[r]
            rows.append(row)
    for row in rows:
        if isinstance(row.get("date"), str) and len(row["date"]) >= 4:
            row["year"] = row["date"][:4]
    return rows


def fetch_rows(client, request: dict, first_page_only: bool = False, max_pages: int = 100) -> list[dict]:
    rows: list[dict] = []
    request = dict(request)
    for _ in range(max_pages):
        envelope = client.call("query_data", request)
        rows += envelope_rows(envelope)
        cursor = envelope.get("next_cursor")
        if first_page_only or not cursor:
            return rows
        request["cursor"] = cursor
    raise EvalError(f"more than {max_pages} pages for one oracle query")


def walk_as_of(client, spec: dict, today: dt.date) -> list[dict]:
    """History of an interval dataset with ``query_data`` only.

    ``query_data`` has no "all intervals in a range" mode: it returns the
    records in force on ``as_of``. Walk the timeline from ``start``: jump to
    each interval's ``effective_to`` (exclusive end), and step ``gap_step_days``
    over days that have no record.
    """
    day = dt.date.fromisoformat(spec["start"])
    step = dt.timedelta(days=int(spec["gap_step_days"]))
    seen = set()
    rows: list[dict] = []
    for _ in range(int(spec["max_queries"])):
        if day > today:
            return rows
        request = substitute(spec["request"], today, {"as_of": day.isoformat()})
        hits = fetch_rows(client, request)
        next_day = day + step
        if hits:
            ends = []
            for row in hits:
                key = (row.get("effective_from"), row.get("effective_to"), row.get("normalized_status"))
                if key not in seen:
                    seen.add(key)
                    rows.append(row)
                end = row.get("effective_to")
                ends.append(dt.date.fromisoformat(end) if end else None)
            if any(end is None for end in ends):
                return rows  # an open-ended interval reaches today
            next_day = max(max(ends), day + dt.timedelta(days=1))
        day = next_day
    raise EvalError(f"history walk exceeded {spec['max_queries']} queries")


def code_of(entity: str) -> str:
    return str(entity).split(".")[-1]


def screen_matches(rule: dict, rows: list[dict]) -> bool:
    """Does one entity pass the screen, judged from its own rows."""
    if rule["type"] == "yearly_down":
        by_year = {str(row["year"]): row for row in rows if row.get("year")}

        def down(year: int) -> bool:
            row = by_year.get(str(year))
            if not row or row.get("open") is None or row.get("close") is None:
                return False
            return Decimal(str(row["close"])) < Decimal(str(row["open"]))

        return all(str(year) in by_year for year in rule["present_years"]) and all(
            down(year) for year in rule["down_years"]
        )
    if rule["type"] == "close_below":
        limit = Decimal(str(rule["limit"]))
        return any(
            row.get("close") is not None and Decimal(str(row["close"])) < limit for row in rows
        )
    raise EvalError(f"unknown screen rule {rule['type']!r}")


def screen_rows(client, spec: dict, today: dt.date) -> list[dict]:
    """Whole-market screen: catalog members first, then the rule per batch of
    ``batch_size`` entities. Returns the matching entities sorted by their
    6-digit code (then exchange), nothing is stored in the repository."""
    members: list[str] = []
    for row in fetch_rows(client, substitute(spec["members"], today)):
        if row.get("entity") and row["entity"] not in members:
            members.append(row["entity"])
    if not members:
        raise EvalError("the catalog returned no members to screen")
    size = int(spec.get("batch_size", 100))
    matched: list[str] = []
    rows_seen = 0
    for start in range(0, len(members), size):
        chunk = members[start:start + size]
        request = substitute(spec["request"], today)
        request["selection"] = {"kind": "batch", "entities": chunk}
        by_entity: dict[str, list[dict]] = {}
        for row in fetch_rows(client, request):
            by_entity.setdefault(row.get("entity"), []).append(row)
            rows_seen += 1
        matched += [entity for entity in chunk if screen_matches(spec["rule"], by_entity.get(entity, []))]
    if not rows_seen:
        raise EvalError("the screen queries returned no rows at all")
    matched.sort(key=lambda entity: (code_of(entity), entity))
    return [{"entity": entity, "code": code_of(entity), "rank": i} for i, entity in enumerate(matched, 1)]


def read_oracle(client, spec: dict, today: dt.date) -> list[dict]:
    if spec.get("kind") == "walk_as_of":
        return walk_as_of(client, spec, today)
    if spec.get("kind") == "screen":
        return screen_rows(client, spec, today)
    return fetch_rows(client, substitute(spec["request"], today), spec.get("first_page_only", False))


# ---------------------------------------------------------------------------
# Answer checks
# ---------------------------------------------------------------------------

DATE_FIELDS = frozenset(
    {
        "date", "effective_from", "effective_to", "effective_day", "start_day", "end_day",
        "first_trading_date", "last_trading_date", "trade_day", "listing_date",
        "delisting_date", "known_at", "announced_at",
    }
)
TEXT_FIELDS = frozenset({"name", "industry_code", "industry_name", "normalized_status", "status"})
UNITS = {"万亿": Decimal(10) ** 12, "亿": Decimal(10) ** 8, "万": Decimal(10) ** 4}

_ISO_DATE = re.compile(r"(?<!\d)(\d{4})[-/.](\d{1,2})[-/.](\d{1,2})(?!\d)")
_CN_DATE = re.compile(r"(?<!\d)(\d{4})\s*年\s*(\d{1,2})\s*月\s*(\d{1,2})\s*[日号]?")
_CN_MONTH_DAY = re.compile(r"(?<!\d)(\d{1,2})\s*月\s*(\d{1,2})\s*[日号]")
_COMPACT_DATE = re.compile(r"(?<!\d)(\d{4})(\d{2})(\d{2})(?!\d)")
_NUMBER = re.compile(r"(?<![A-Za-z0-9_.])-?\d[\d,]*(?:\.\d+)?(?:\s*(?:万亿|亿|万))?")
_SEGMENT_SPLIT = re.compile(r"[\n。；;]")
_LIST_ITEM = re.compile(r"\s*(?:[-*•]|\d+[.)、])\s")
_VERDICT = re.compile(r"结论\s*[:：]\s*\**\s*(是|否|无法确定)")


def extract_dates(text: str) -> tuple[list[tuple], str]:
    """Dates as (year|None, month, day) and the text with those dates blanked out."""
    found: list[tuple] = []

    def take(pattern, with_year=True, valid=None):
        nonlocal text

        def repl(match):
            groups = [int(group) for group in match.groups()]
            year, month, day = groups if with_year else (None, *groups)
            if valid and not valid(year, month, day):
                return match.group(0)
            if not (1 <= month <= 12 and 1 <= day <= 31):
                return match.group(0)
            found.append((year, month, day))
            return " "

        text = pattern.sub(repl, text)

    take(_ISO_DATE)
    take(_CN_DATE)
    take(_COMPACT_DATE, valid=lambda year, month, day: 1990 <= year <= 2100)
    take(_CN_MONTH_DAY, with_year=False)
    return found, text


def extract_numbers(text: str) -> set[Decimal]:
    numbers: set[Decimal] = set()
    for match in _NUMBER.finditer(text):
        token = match.group(0)
        unit = next((u for u in UNITS if token.endswith(u)), None)
        body = token[: -len(unit)] if unit else token
        body = body.strip()
        candidates = {body.replace(",", "")}
        if "," in body:
            candidates |= {part for part in body.split(",") if part}
        for candidate in candidates:
            try:
                value = Decimal(candidate)
            except InvalidOperation:
                continue
            # A leading "-" is usually a range ("10.05-10.20"), so keep |value|.
            for plain in {value, abs(value)}:
                numbers.add(plain)
                if unit:
                    numbers.add(plain * UNITS[unit])
    return numbers


class Segment:
    """One line or sentence of the answer, with what it mentions."""

    def __init__(self, text: str):
        self.raw = text
        self.dates, blanked = extract_dates(text)
        self.numbers = extract_numbers(blanked)
        self.lower = text.lower()


def segments_of(answer: str, single_row: bool) -> list[Segment]:
    parts = [part for part in _SEGMENT_SPLIT.split(answer) if part.strip()]
    if single_row:
        parts.append(answer)  # one expected row: any sentence combination counts
    return [Segment(part) for part in parts]


def date_matches(found: tuple, wanted: dt.date) -> bool:
    year, month, day = found
    return month == wanted.month and day == wanted.day and (year is None or year == wanted.year)


def field_in_segment(segment: Segment, field: str, value, offsets: list[int]) -> bool:
    if value is None:
        return True
    if field == "entity":
        code = str(value).split(".")[-1]
        return re.search(rf"(?<!\d){re.escape(code)}(?!\d)", segment.raw) is not None
    if field == "year":
        return re.search(rf"(?<!\d){re.escape(str(value))}(?!\d)", segment.raw) is not None
    if field in DATE_FIELDS:
        try:
            base = dt.date.fromisoformat(str(value))
        except ValueError:
            return str(value) in segment.raw
        wanted = [base + dt.timedelta(days=offset) for offset in offsets]
        return any(date_matches(found, day) for found in segment.dates for day in wanted)
    if field in TEXT_FIELDS:
        return str(value).lower() in segment.lower
    try:
        return Decimal(str(value)) in segment.numbers
    except InvalidOperation:
        return str(value) in segment.raw


def select_rows(rows: list[dict], check: dict) -> list[dict]:
    for field, allowed in (check.get("where") or {}).items():
        rows = [row for row in rows if row.get(field) in allowed]
    select = check.get("select")
    if select:
        mode, _, count = select.partition(":")
        rows = rows[: int(count)] if mode == "head" else rows[-int(count):]
    return rows


def row_label(row: dict, fields: list[str]) -> str:
    return ",".join(f"{field}={row.get(field)}" for field in fields if row.get(field) is not None)


def stray_data_dates(answer: str, known: set[dt.date], oracle_known: set[dt.date], numeric_fields: int):
    """Dates that are not expected, split into (in data rows, in notes).

    Only a data row can add a row to the answer. A segment (line or sentence)
    is a data row when it is a table row with a value in it, or when it
    carries values like a row does: at least half of the numeric fields (and
    two numbers) outside the dates. A check without numeric fields (intervals)
    treats a list item, or a segment that names an expected date, as a row.
    Everything else is a note and its dates are only reported."""
    rows: set[str] = set()
    notes: set[str] = set()
    for part in _SEGMENT_SPLIT.split(answer):
        if not part.strip():
            continue
        dates, blanked = extract_dates(part)
        bad = {
            f"{y or '----'}-{m:02d}-{d:02d}"
            for y, m, d in dates
            if not any(date_matches((y, m, d), day) for day in known)
        }
        if not bad:
            continue
        values = len(_NUMBER.findall(blanked))
        table = part.lstrip().startswith("|")
        if numeric_fields:
            is_row = values >= 1 if table else values >= max(2, math.ceil(numeric_fields / 2))
        else:
            is_row = table or _LIST_ITEM.match(part) is not None or any(
                date_matches(found, day) for found in dates for day in oracle_known
            )
        (rows if is_row else notes).update(bad)
    return sorted(rows), sorted(notes)


def check_oracle_values(answer: str, oracle_rows: list[dict], check: dict, allowed_dates: set[dt.date]) -> dict:
    fields = check["fields"]
    offsets_by_field = {field: check.get("date_offsets", {}).get(field, [0]) for field in fields}
    all_rows = oracle_rows
    rows = select_rows(oracle_rows, check)
    answer_dates, _ = extract_dates(answer)
    detail: dict = {"expected_rows": len(rows)}
    if not rows:
        if not check.get("allow_empty"):
            raise EvalError("oracle returned no rows for this question")
        detail["note"] = "oracle has no matching rows; the answer must list no dates"
        stray = sorted({f"{y or '----'}-{m:02d}-{d:02d}" for y, m, d in answer_dates
                        if not any(date_matches((y, m, d), allowed) for allowed in allowed_dates)})
        detail["unexpected_dates"] = stray
        return {"kind": "oracle_values", "ok": not stray, "detail": detail}
    single = len(rows) == 1
    segments = segments_of(answer, single)
    missing = []
    for row in rows:
        ok = any(
            all(field_in_segment(segment, field, row.get(field), offsets_by_field[field]) for field in fields)
            for segment in segments
        )
        if not ok:
            missing.append(row_label(row, fields))
    detail["matched_rows"] = len(rows) - len(missing)
    if missing:
        detail["missing_rows"] = missing[:5]
    ok = not missing
    subset = check.get("dates_subset_of")
    if subset:
        oracle_known: set[dt.date] = set()
        for row in all_rows if subset == "all" else rows:
            for field in fields:
                if field in DATE_FIELDS and row.get(field):
                    base = dt.date.fromisoformat(str(row[field]))
                    oracle_known |= {base + dt.timedelta(days=offset) for offset in offsets_by_field[field]}
        numeric = [
            field for field in fields
            if field not in DATE_FIELDS and field not in TEXT_FIELDS and field not in ("entity", "year")
        ]
        stray, notes = stray_data_dates(answer, set(allowed_dates) | oracle_known, oracle_known, len(numeric))
        if stray:
            ok = False
            detail["unexpected_dates"] = stray
        if notes:
            detail["dates_in_notes"] = notes
    return {"kind": "oracle_values", "ok": ok, "detail": detail}


def expected_verdict(rows: list[dict], check: dict) -> str:
    statuses = {row.get(check["status_field"]) for row in rows}
    if statuses & set(check["yes_status"]):
        return "是"
    if statuses & set(check["no_status"]):
        return "否"
    return "无法确定"


def check_verdict(answer: str, oracle_rows: list[dict], check: dict) -> dict:
    expected = expected_verdict(oracle_rows, check)
    matches = _VERDICT.findall(answer)
    got = matches[-1] if matches else None
    return {
        "kind": "verdict",
        "ok": got == expected,
        "detail": {
            "expected": expected,
            "got": got,
            "oracle_statuses": sorted({str(row.get(check["status_field"])) for row in oracle_rows}),
        },
    }


_CODE6 = re.compile(r"(?<!\d)(\d{6})(?!\d)")


def check_screen(answer: str, rows: list[dict], check: dict) -> dict:
    """Whole-market screen: the stated total and the first ``top`` codes in
    ascending order (first mention order in the answer). "By code" is read as
    the 6-digit code; the order of the full symbol (``sh.`` before ``sz.``) is
    accepted too and recorded as ``order``."""
    top = int(check.get("top", 10))
    expected_top = [row["code"] for row in rows[:top]]
    by_symbol = [code_of(entity) for entity in sorted(row["entity"] for row in rows)[:top]]
    _, blanked = extract_dates(answer)
    total_ok = Decimal(len(rows)) in extract_numbers(blanked)
    got: list[str] = []
    for code in _CODE6.findall(answer):
        if code not in got:
            got.append(code)
    got_top = got[:len(expected_top)]
    order = "code" if got_top == expected_top else "symbol" if got_top == by_symbol else None
    detail: dict = {
        "expected_total": len(rows),
        "total_stated": total_ok,
        "expected_top": expected_top,
        "got_top": got_top,
        "order": order,
    }
    missing = [code for code in expected_top if code not in got]
    if missing:
        detail["missing_codes"] = missing[:10]
    elif order is None:
        detail["order_mismatch"] = True
    return {"kind": "screen", "ok": total_ok and order is not None, "detail": detail}


def check_answer_text(answer: str, check: dict) -> dict:
    """Two honest outcomes, recorded apart: the code is given, or the answer
    says the tools cannot look it up. Anything else is wrong."""
    found = re.search(check["must_contain"], answer) is not None
    declined = any(re.search(pattern, answer) for pattern in check.get("declines", []))
    outcome = "found_code" if found else "declared_unsupported" if declined else "neither"
    return {
        "kind": "answer_text",
        "ok": outcome != "neither",
        "detail": {"outcome": outcome, "code_given": found, "declared_unsupported": declined},
    }


def dates_in(text: str) -> set[dt.date]:
    out = set()
    for year, month, day in extract_dates(text)[0]:
        if year:
            try:
                out.add(dt.date(year, month, day))
            except ValueError:
                pass
    return out



def check_tradability(answer: str, oracle_rows_dict: dict[str, list[dict]], check: dict) -> dict:
    """Can the stock be traded? Check if there's a bar and no suspension."""
    oracle_names = check["oracles"]
    bar_rows = oracle_rows_dict.get(oracle_names[0], [])
    suspension_rows = oracle_rows_dict.get(oracle_names[1], []) if len(oracle_names) > 1 else []
    
    has_bar = len(bar_rows) > 0
    suspended = len(suspension_rows) > 0
    
    expected = "能买" if has_bar and not suspended else "不能买" if not has_bar or suspended else "无法确定"
    
    matches = re.findall(r"结论\s*[:：]\s*\**\s*(能买|不能买|无法确定)", answer)
    got = matches[-1] if matches else None
    
    return {
        "kind": "tradability",
        "ok": got == expected,
        "detail": {
            "expected": expected,
            "got": got,
            "has_bar": has_bar,
            "suspended": suspended,
        },
    }


def check_limit_price(answer: str, oracle_rows_dict: dict[str, list[dict]], check: dict) -> dict:
    """Check the limit-up price computed from prev close and band."""
    from decimal import Decimal, ROUND_HALF_UP
    
    oracle_names = check["oracles"]
    prev_rows = oracle_rows_dict.get(oracle_names[0], [])
    curr_rows = oracle_rows_dict.get(oracle_names[1], [])
    
    if not prev_rows or "close" not in prev_rows[0]:
        return {
            "kind": "limit_price",
            "ok": False,
            "detail": {"error": "no_prev_close", "expected": None, "got": None},
        }
    
    prev_close = Decimal(str(prev_rows[0]["close"]))
    band_bp = Decimal(str(check["band_bp"]))
    min_tick_cents = Decimal(str(check["min_tick_cents"]))
    
    # Compute limit-up: round_half_up(prev_close * (10000 + band_bp) / 10000)
    limit_up_raw = prev_close * (Decimal("10000") + band_bp) / Decimal("10000")
    limit_up = limit_up_raw.quantize(min_tick_cents / 100, rounding=ROUND_HALF_UP)
    
    # Extract numbers from answer
    numbers = extract_numbers(answer)
    # Match with tolerance (limit_up is in yuan, numbers might be in cents)
    expected_cents = int(limit_up * 100)
    got_match = any(
        abs(int(n * 100) - expected_cents) <= 1
        for n in numbers
        if n > 0
    )
    
    return {
        "kind": "limit_price",
        "ok": got_match,
        "detail": {
            "expected": float(limit_up),
            "prev_close": float(prev_close),
            "band_bp": int(band_bp),
            "numbers_in_answer": sorted([float(n) for n in numbers if n > 0])[:10],
        },
    }


def check_market_cap(answer: str, oracle_rows_dict: dict[str, list[dict]], check: dict) -> dict:
    """Check market cap computed from close price and shares."""
    from decimal import Decimal
    
    oracle_names = check["oracles"]
    bar_rows = oracle_rows_dict.get(oracle_names[0], [])
    capital_rows = oracle_rows_dict.get(oracle_names[1], [])
    
    if not bar_rows or "close" not in bar_rows[0]:
        return {
            "kind": "market_cap",
            "ok": False,
            "detail": {"error": "no_close_price", "expected": None},
        }
    
    close_price = Decimal(str(bar_rows[0]["close"]))
    
    # Find the latest capital event (should be sorted by effective_day desc or asc)
    scope = check["scope"]
    shares = None
    for row in capital_rows:
        if scope == "float_a" and "float_after" in row:
            shares = Decimal(str(row["float_after"]))
            break
        elif scope in ["a_share_total", "company_total"] and "total_after" in row:
            shares = Decimal(str(row["total_after"]))
            break
    
    if shares is None:
        return {
            "kind": "market_cap",
            "ok": False,
            "detail": {"error": "no_shares_data", "expected": None, "scope": scope},
        }
    
    # Market cap in yuan (close_price is yuan, shares is number of shares)
    market_cap_yuan = close_price * shares
    market_cap_yi = market_cap_yuan / Decimal("100000000")  # 亿
    
    numbers = extract_numbers(answer)
    # Match either yuan or yi yuan
    got_match = any(
        abs(n - market_cap_yuan) / market_cap_yuan < Decimal("0.01")
        or abs(n - market_cap_yi) / market_cap_yi < Decimal("0.01")
        for n in numbers
        if n > 0
    )
    
    return {
        "kind": "market_cap",
        "ok": got_match,
        "detail": {
            "expected_yuan": float(market_cap_yuan),
            "expected_yi": float(market_cap_yi),
            "close_price": float(close_price),
            "shares": float(shares),
            "scope": scope,
            "numbers_in_answer": sorted([float(n) for n in numbers if n > 0])[:10],
        },
    }


def check_unknown_status(answer: str, oracle_rows: list[dict], check: dict) -> dict:
    """Check if answer admits uncertainty when status is unknown."""
    has_status = len(oracle_rows) > 0 and any(
        row.get("normalized_status") not in [None, ""] for row in oracle_rows
    )
    
    # Look for uncertainty indicators
    uncertainty_patterns = [
        r"无法确定",
        r"不确定",
        r"不能确定",
        r"未知",
        r"unknown",
        r"结论\s*[:：]\s*\**\s*无法确定"
    ]
    admits_uncertainty = any(re.search(pat, answer, re.IGNORECASE) for pat in uncertainty_patterns)
    
    # If status is unknown (no rows), answer should admit uncertainty
    expected = "无法确定" if not has_status else "已确定"
    matches = re.findall(r"结论\s*[:：]\s*\**\s*(已确定|无法确定)", answer)
    got = matches[-1] if matches else ("无法确定" if admits_uncertainty else None)
    
    return {
        "kind": "unknown_status",
        "ok": got == expected,
        "detail": {
            "expected": expected,
            "got": got,
            "has_status_data": has_status,
            "admits_uncertainty": admits_uncertainty,
        },
    }


def evaluate_answer(task: dict, answer: str, oracle_rows: dict[str, list[dict]], today: dt.date) -> list[dict]:
    allowed = {today} | dates_in(task["prompt"])
    results = []
    for check in task["checks"]:
        if check["kind"] == "answer_text":
            results.append(check_answer_text(answer, check))
            continue
        # New check kinds that need multiple oracles
        if check["kind"] in ("tradability", "limit_price", "market_cap"):
            results.append(globals()[f"check_{check['kind']}"](answer, oracle_rows, check))
            continue
        # Single-oracle checks
        rows = oracle_rows[check["oracle"]]
        if check["kind"] == "oracle_values":
            results.append(check_oracle_values(answer, rows, check, allowed))
        elif check["kind"] == "verdict":
            results.append(check_verdict(answer, rows, check))
        elif check["kind"] == "screen":
            results.append(check_screen(answer, rows, check))
        elif check["kind"] == "unknown_status":
            results.append(check_unknown_status(answer, rows, check))
        else:
            raise EvalError(f"unknown check kind {check['kind']!r}")
    return results


# ---------------------------------------------------------------------------
# Running one question
# ---------------------------------------------------------------------------


def run_pi(command: list[str], cwd: str, timeout: float) -> tuple[int | None, str, str, float]:
    """(returncode or None on timeout, stdout, stderr, wall seconds)."""
    started = time.monotonic()
    env = dict(os.environ, PI_SKIP_VERSION_CHECK="1")
    proc = subprocess.Popen(
        command, cwd=cwd, env=env, text=True, stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True,
    )
    try:
        out, err = proc.communicate(timeout=timeout)
        return proc.returncode, out, err, time.monotonic() - started
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
        out, err = proc.communicate()
        return None, out or "", err or "", time.monotonic() - started


def run_task(task: dict, ctx: dict) -> dict:
    """Run one question. ``ctx`` carries config, scope, client, runner, today."""
    today = ctx["today"]
    report: dict = {
        "id": task["id"], "title": task["title"], "group": task.get("group", "core"),
        "status": "ok", "correct": None,
    }
    try:
        oracle_rows = {
            name: read_oracle(ctx["client"], spec, today) for name, spec in task["oracles"].items()
        }
    except EvalError as error:
        report.update(status="oracle_error", error=str(error))
        return report
    report["oracle"] = oracle_rows  # expected values, so the answer can be graded again later
    system_prompt = ctx["config"]["system_prompt"].replace("{today}", today.isoformat())
    mode = ctx.get("mode", "direct")
    command = build_pi_command(
        ctx["pi"], ctx["allowed"], system_prompt, substitute(task["prompt"], today),
        ctx.get("model"), ctx.get("thinking"), mode,
    )
    code, out, err, wall = ctx["runner"](command, ctx["workdir"], ctx["timeout"])
    report["duration_s"] = round(wall, 3)
    if ctx.get("events_dir"):
        Path(ctx["events_dir"]).mkdir(parents=True, exist_ok=True)
        Path(ctx["events_dir"], f"{task['id']}.jsonl").write_text(out, encoding="utf-8")
    stats = collect_stats(iter_records(out.splitlines()), mode, set(ctx["allowed"]))
    report.update(stats)
    if code is None:
        report.update(status="timeout", error=f"pi did not finish within {ctx['timeout']:g}s")
    elif code != 0:
        report.update(status="pi_error", error=f"pi exited {code}: {err.strip()[-ERROR_TEXT_LIMIT:]}")
    elif not stats["answer"]:
        report.update(status="no_answer", error="the last assistant message has no text")
    expected = {CODEMODE_TOOL} if mode == "codemode" else set(ctx["allowed"])
    declared = set(stats["declared_tools"])
    if declared and declared != expected:
        report["scope_violation"] = {
            "missing": sorted(expected - declared),
            "unexpected": sorted(declared - expected),
        }
        report.update(status="scope_error", error="declared tools differ from the scope")
    unexpected_calls = sorted(set(stats["tools"]) - expected)  # what the model itself called
    if unexpected_calls:
        report["unexpected_tool_calls"] = unexpected_calls
    if report["status"] in ("ok", "no_answer"):
        try:
            report["checks"] = evaluate_answer(task, stats["answer"], oracle_rows, today)
            report["correct"] = all(check["ok"] for check in report["checks"])
        except EvalError as error:
            report.update(status="oracle_error", error=str(error))
    return report


# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------


def wrong_reason(task: dict) -> str:
    """One line on why a question is not correct; empty when it is."""
    if task.get("correct") is True:
        return ""
    if task.get("correct") is None:
        return f"not graded: {task['status']}" + (f" ({task['error']})" if task.get("error") else "")
    reasons = []
    for check in task.get("checks", []):
        if check.get("ok"):
            continue
        detail = check.get("detail", {})
        kind = check.get("kind")
        parts = []
        if kind == "oracle_values":
            if detail.get("missing_rows"):
                parts.append(
                    f"{detail['expected_rows'] - detail.get('matched_rows', 0)} of "
                    f"{detail['expected_rows']} expected rows missing, e.g. {detail['missing_rows'][0]}"
                )
            if detail.get("unexpected_dates"):
                parts.append("unexpected dates in data rows: " + ",".join(detail["unexpected_dates"]))
        elif kind == "verdict":
            parts.append(f"verdict {detail.get('got')!r}, expected {detail.get('expected')!r}")
        elif kind == "screen":
            if not detail.get("total_stated"):
                parts.append(f"total {detail.get('expected_total')} not stated")
            if detail.get("missing_codes"):
                parts.append("first codes missing: " + ",".join(detail["missing_codes"][:3]))
            elif detail.get("order") is None:
                parts.append("first codes differ or are out of order")
        elif kind == "answer_text":
            parts.append("neither the code nor an honest 'cannot look up'")
        reasons.append("; ".join(parts) or f"{kind} check failed")
    return " | ".join(reasons) or "incorrect"


def summarize_group(ran: list[dict], tasks: list[dict]) -> dict:
    tokens = {key: sum(task["tokens"][key] for task in ran) for key in ("input", "output", "cache_read", "cache_write", "total")}
    count = len(ran)
    codemode = [task["codemode"] for task in ran if "codemode" in task]
    extra = {}
    if codemode:
        extra["codemode"] = {
            key: sum(item[key] for item in codemode)
            for key in ("calls", "script_failures", "tool_calls", "tool_errors", "results_without_details")
        }
    return {
        **extra,
        "tasks": len(tasks),
        "ran": count,
        "correct": sum(1 for task in tasks if task.get("correct") is True),
        "incorrect": sum(1 for task in tasks if task.get("correct") is False),
        "unverified": sum(1 for task in tasks if task.get("correct") is None),
        "turns": sum(task["turns"] for task in ran),
        "avg_turns": round(sum(task["turns"] for task in ran) / count, 3) if count else 0.0,
        "tokens": tokens,
        "fresh_tokens": tokens["input"] + tokens["output"] + tokens["cache_write"],
        "tool_calls": sum(task["tool_calls"] for task in ran),
        "tool_errors": sum(task["tool_errors"] for task in ran),
        "tool_result_bytes": sum(task["tool_result_bytes"] for task in ran),
        "duration_s": round(sum(task.get("duration_s", 0.0) for task in ran), 3),
    }


def summarize(tasks: list[dict]) -> dict:
    """The top-level numbers cover the ``core`` questions (design section 9,
    including its 2-turn average). Questions of other groups (whole-market
    screens, the ETF-name comparison) are listed apart under ``groups``."""
    def group_of(task):
        return task.get("group", "core")

    core = [task for task in tasks if group_of(task) == "core"]
    summary = summarize_group([task for task in core if "tokens" in task], core)
    summary["infrastructure_errors"] = sum(1 for task in tasks if task["status"] not in ("ok", "no_answer"))
    summary["groups"] = {
        name: summarize_group([t for t in tasks if group_of(t) == name and "tokens" in t],
                              [t for t in tasks if group_of(t) == name])
        for name in sorted({group_of(task) for task in tasks}, key=lambda n: (n != "core", n))
    }
    summary["wrong_reason"] = {task["id"]: wrong_reason(task) for task in tasks}
    return summary


def run_eval(config: dict, scope: str, ctx: dict, only: list[str] | None = None) -> dict:
    tasks = [task for task in config["tasks"] if not only or task["id"] in only]
    started = dt.datetime.now(dt.timezone.utc)
    reports = [run_task(task, ctx) for task in tasks]
    models = sorted({model for report in reports for model in report.get("models", [])})
    return {
        "eval": config["id"],
        "mode": ctx.get("mode", "direct"),
        "scope": scope,
        "scope_tools": ctx["allowed"],
        "model_requested": ctx.get("model"),
        "thinking": ctx.get("thinking"),
        "models_seen": models,
        "today": ctx["today"].isoformat(),
        "started_at": started.isoformat(timespec="seconds"),
        "finished_at": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
        "tasks": reports,
        "summary": summarize(reports),
    }


def regrade_report(report: dict, config: dict) -> tuple[dict, list[str]]:
    """Grade the saved answers again with the current checks. No model, no
    service. Returns the new report and a note per question that could not be
    graded again (it keeps its old result)."""
    new = copy.deepcopy(report)
    tasks_by_id = {task["id"]: task for task in config["tasks"]}
    today = dt.date.fromisoformat(report["today"])
    notes: list[str] = []
    for item in new["tasks"]:
        task = tasks_by_id.get(item["id"])
        if item.get("status") not in ("ok", "no_answer"):
            item["regrade"] = "skipped: not graded in the original run"
        elif task is None:
            item["regrade"] = "skipped: question is not in the evaluation file"
        elif "oracle" not in item:
            item["regrade"] = "skipped: the report holds no expected values"
        elif not item.get("answer"):
            item["regrade"] = "skipped: the report holds no answer"
        else:
            item["group"] = task.get("group", "core")
            item["previous_correct"] = item.get("correct")
            try:
                item["checks"] = evaluate_answer(task, item["answer"], item["oracle"], today)
            except (EvalError, KeyError) as error:
                item["regrade"] = f"skipped: {error}"
            else:
                item["correct"] = all(check["ok"] for check in item["checks"])
                item["status"] = "ok"
                item.pop("error", None)
                item["regrade"] = "regraded"
        if item["regrade"] != "regraded":
            notes.append(f"{item['id']}: {item['regrade']}")
    new["regraded_from"] = {"finished_at": report.get("finished_at"), "eval": report.get("eval")}
    new["summary"] = summarize(new["tasks"])
    return new, notes


def regrade_main(args) -> int:
    try:
        config = load_config(Path(args.eval))
        report = json.loads(Path(args.regrade).read_text(encoding="utf-8"))
        new, notes = regrade_report(report, config)
    except (EvalError, OSError, KeyError, ValueError) as error:
        print(f"cannot regrade: {error}", file=sys.stderr)
        return 2
    Path(args.output).write_text(json.dumps(new, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    summary = new["summary"]
    changed = [
        f"{t['id']} {t['previous_correct']}->{t['correct']}"
        for t in new["tasks"] if t.get("regrade") == "regraded" and t["previous_correct"] != t["correct"]
    ]
    print(
        f"regraded {sum(1 for t in new['tasks'] if t.get('regrade') == 'regraded')}/{len(new['tasks'])} questions: "
        f"{summary['correct']}/{summary['tasks']} core correct; changed: {', '.join(changed) or 'none'}; "
        f"report {args.output}"
    )
    for note in notes:
        print(f"  kept old result, {note}", file=sys.stderr)
    return 0


def parse_args(argv=None, config_names=None):
    parser = argparse.ArgumentParser(description="Evaluate the read tools with the section 9 question set.")
    parser.add_argument("--scope", help="tool scope: baseline, new or new-no-query")
    parser.add_argument("--mode", choices=MODES, default="direct", help="direct: the scope's MCP tools as separate tools; codemode: only the codemode tool, tools are called from scripts")
    parser.add_argument("--socket", help="research entry socket of the data service")
    parser.add_argument("--output", required=True, help="path of the JSON report")
    parser.add_argument("--regrade", metavar="REPORT", help="grade the answers saved in this report again; runs no model and reads no service")
    parser.add_argument("--model", help="pi model pattern, for example provider/id")
    parser.add_argument("--thinking", help="pi thinking level")
    parser.add_argument("--eval", default=str(DEFAULT_CONFIG), help="evaluation file")
    parser.add_argument("--tasks", help="comma-separated question ids (default: all)")
    parser.add_argument("--today", help="YYYY-MM-DD, default today in Asia/Shanghai")
    parser.add_argument("--timeout", type=float, default=300.0, help="seconds per question")
    parser.add_argument("--pi", default="pi", help="pi executable")
    parser.add_argument("--mcp-connect", default="mcp_connect", help="mcp_connect executable")
    parser.add_argument("--events-dir", help="keep each question's pi event stream here")
    parser.add_argument("--dry-run", action="store_true", help="print the pi commands and exit; runs nothing")
    args = parser.parse_args(argv)
    if not args.regrade and not (args.scope and args.socket):
        parser.error("--scope and --socket are required unless --regrade is given")
    return args


def main(argv=None) -> int:
    args = parse_args(argv)
    if args.regrade:
        return regrade_main(args)
    try:
        config = load_config(Path(args.eval))
    except (EvalError, OSError, KeyError, json.JSONDecodeError) as error:
        print(f"bad evaluation file: {error}", file=sys.stderr)
        return 2
    if args.scope not in config["scopes"]:
        print(f"unknown scope {args.scope!r}; choose from {sorted(config['scopes'])}", file=sys.stderr)
        return 2
    only = [part for part in (args.tasks or "").split(",") if part] or None
    known = {task["id"] for task in config["tasks"]}
    if only and set(only) - known:
        print(f"unknown question ids: {sorted(set(only) - known)}", file=sys.stderr)
        return 2
    today = dt.date.fromisoformat(args.today) if args.today else dt.datetime.now(CST).date()
    server = config["server_name"]
    allowed = [pi_tool_name(server, tool) for tool in config["scopes"][args.scope]]
    system_prompt = config["system_prompt"].replace("{today}", today.isoformat())

    if args.dry_run:
        for task in config["tasks"]:
            if only and task["id"] not in only:
                continue
            command = build_pi_command(args.pi, allowed, system_prompt, substitute(task["prompt"], today), args.model, args.thinking, args.mode)
            print(task["id"], json.dumps(command, ensure_ascii=False))
        return 0

    client = None
    try:
        client = McpClient([args.mcp_connect, args.socket])
        offered = set(client.list_tools())
        missing = sorted(set(config["scopes"][args.scope]) - offered)
        if missing:
            print(f"the service at {args.socket} does not offer scope tools: {missing}", file=sys.stderr)
            return 2
        with tempfile.TemporaryDirectory(prefix="eval-read-tools-") as workdir:
            (Path(workdir) / ".pi").mkdir()
            (Path(workdir) / ".pi" / "mcp.json").write_text(
                json.dumps(mcp_project_config(server, args.mcp_connect, args.socket, mode=args.mode)), encoding="utf-8"
            )
            ctx = {
                "config": config, "pi": args.pi, "allowed": allowed, "model": args.model,
                "thinking": args.thinking, "today": today, "timeout": args.timeout,
                "client": client, "runner": run_pi, "workdir": workdir, "events_dir": args.events_dir,
                "mode": args.mode,
            }
            report = run_eval(config, args.scope, ctx, only)
    except (EvalError, OSError) as error:
        print(f"setup failed: {error}", file=sys.stderr)
        return 2
    finally:
        if client:
            client.close()
    Path(args.output).write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    summary = report["summary"]
    print(
        f"{args.scope}/{args.mode}: {summary['correct']}/{summary['tasks']} core correct, "
        f"avg turns {summary['avg_turns']}, tokens {summary['tokens']['total']}, "
        f"tool errors {summary['tool_errors']}; report {args.output}"
    )
    return 1 if summary["infrastructure_errors"] else 0


if __name__ == "__main__":
    sys.exit(main())
