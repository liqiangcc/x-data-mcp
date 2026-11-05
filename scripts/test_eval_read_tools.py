"""Unit tests for scripts/eval_read_tools.py. No model, no service, no data."""
import datetime as dt
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("eval_read_tools", SCRIPTS / "eval_read_tools.py")
ev = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ev)

TODAY = dt.date(2026, 11, 5)


def usage(input_=0, output=0, cache_read=0, cache_write=0):
    return {
        "input": input_, "output": output, "cacheRead": cache_read, "cacheWrite": cache_write,
        "totalTokens": input_ + output + cache_read + cache_write,
    }


def assistant(text="", calls=(), stop="stop", **tokens):
    content = [{"type": "text", "text": text}] if text else []
    content += [
        {"type": "toolCall", "id": call_id, "name": name, "arguments": args}
        for call_id, name, args in calls
    ]
    return {
        "role": "assistant", "content": content, "provider": "p", "model": "m",
        "usage": usage(**tokens), "stopReason": stop, "timestamp": 1_000,
    }


def tool_result(call_id, name, text="ok", error=False):
    return {
        "role": "toolResult", "toolCallId": call_id, "toolName": name,
        "content": [{"type": "text", "text": text}], "isError": error, "timestamp": 2_000,
    }


def system(*tools):
    return {"role": "system", "content": "", "toolsAdded": [{"name": t} for t in tools], "timestamp": 500}


def json_events(messages):
    lines = [{"type": "session", "id": "s"}, {"type": "agent_start"}]
    for message in messages:
        lines.append({"type": "message_start", "message": message})
        lines.append({"type": "message_end", "message": message})
    lines.append({"type": "agent_end", "messages": messages})  # repeats the messages
    return [json.dumps(line) for line in lines]


def session_file(messages):
    lines = [{"type": "session", "version": 3, "id": "s"}]
    lines += [{"type": "message", "id": f"e{i}", "message": m} for i, m in enumerate(messages)]
    return [json.dumps(line) for line in lines]


CONVERSATION = [
    system("mcp__x_data_mcp__query_data", "mcp__x_data_mcp__get_daily_bars"),
    {"role": "user", "content": "q", "timestamp": 900},
    assistant(calls=[("c1", "mcp__x_data_mcp__get_daily_bars", {}), ("c2", "mcp__x_data_mcp__query_data", {})],
              stop="toolUse", input_=100, output=10, cache_write=5),
    tool_result("c1", "mcp__x_data_mcp__get_daily_bars", "bad arguments", error=True),
    tool_result("c2", "mcp__x_data_mcp__query_data", "rows"),
    assistant(calls=[("c3", "mcp__x_data_mcp__get_daily_bars", {})], stop="toolUse", input_=40, output=8, cache_read=100),
    tool_result("c3", "mcp__x_data_mcp__get_daily_bars", "rows"),
    assistant("最终答案 10.05", input_=60, output=20, cache_read=150),
]


class Statistics(unittest.TestCase):
    def check(self, stats):
        self.assertEqual(stats["turns"], 3)
        self.assertEqual(stats["tool_calls"], 3)
        self.assertEqual(stats["tool_errors"], 1)
        self.assertEqual(stats["unanswered_tool_calls"], 0)
        self.assertEqual(
            stats["tokens"],
            {"input": 200, "output": 38, "cache_read": 250, "cache_write": 5, "total": 493},
        )
        self.assertEqual(stats["tools"], {"mcp__x_data_mcp__get_daily_bars": 2, "mcp__x_data_mcp__query_data": 1})
        self.assertEqual(stats["answer"], "最终答案 10.05")
        self.assertEqual(stats["errors"], [{"tool": "mcp__x_data_mcp__get_daily_bars", "message": "bad arguments"}])
        self.assertEqual(stats["models"], ["p/m"])
        self.assertEqual(stats["tool_result_bytes"], len("bad arguments") + len("rows") * 2)
        self.assertEqual(stats["declared_tools"], ["mcp__x_data_mcp__get_daily_bars", "mcp__x_data_mcp__query_data"])

    def test_json_event_stream_counts_each_message_once(self):
        self.check(ev.collect_stats(ev.iter_records(json_events(CONVERSATION))))

    def test_session_file_gives_the_same_numbers(self):
        self.check(ev.collect_stats(ev.iter_records(session_file(CONVERSATION))))

    def test_garbage_lines_are_skipped(self):
        lines = ["", "not json", "[1]"] + json_events(CONVERSATION)
        self.check(ev.collect_stats(ev.iter_records(lines)))

    def test_failed_model_call_is_not_a_turn(self):
        messages = CONVERSATION + [assistant("", stop="error", input_=1)]
        stats = ev.collect_stats(ev.iter_records(json_events(messages)))
        self.assertEqual(stats["turns"], 3)
        self.assertEqual(stats["model_errors"], 1)
        self.assertEqual(stats["tokens"]["input"], 201)

    def test_call_without_result_is_reported(self):
        messages = CONVERSATION[:3]
        stats = ev.collect_stats(ev.iter_records(json_events(messages)))
        self.assertEqual(stats["unanswered_tool_calls"], 2)

    def test_tool_removals_are_replayed(self):
        removal = {"role": "system", "content": "", "toolsRemoved": [{"name": "a"}], "timestamp": 600}
        self.assertEqual(ev.declared_tools([system("a", "b"), removal]), {"b"})

    def test_empty_stream(self):
        stats = ev.collect_stats(ev.iter_records([]))
        self.assertEqual((stats["turns"], stats["tool_calls"], stats["answer"]), (0, 0, ""))


class Substitution(unittest.TestCase):
    def test_today_and_named_placeholders(self):
        value = {"a": ["{today}", "{today-45}"], "b": {"c": "x {as_of} y"}, "d": 3}
        out = ev.substitute(value, TODAY, {"as_of": "2020-01-02"})
        self.assertEqual(out, {"a": ["2026-11-05", "2026-09-21"], "b": {"c": "x 2020-01-02 y"}, "d": 3})

    def test_tool_names_follow_pi(self):
        self.assertEqual(ev.pi_tool_name("x-data-mcp", "get_daily_bars"), "mcp__x_data_mcp__get_daily_bars")


class Configuration(unittest.TestCase):
    def setUp(self):
        self.config = ev.load_config()

    def test_thirteen_questions_with_prompts_and_checks(self):
        self.assertEqual([t["id"] for t in self.config["tasks"]], [f"q{i:02d}" for i in range(1, 14)])
        groups = {t["id"]: t["group"] for t in self.config["tasks"]}
        self.assertEqual({g for i, g in groups.items() if int(i[1:]) <= 10}, {"core"})
        self.assertEqual((groups["q11"], groups["q12"], groups["q13"]), ("screen", "screen", "etf_name"))
        for task in self.config["tasks"]:
            self.assertTrue(task["prompt"] and task["checks"] and "oracles" in task, task["id"])
            if task["id"] != "q13":
                self.assertTrue(task["oracles"], task["id"])

    def test_oracles_only_use_query_data_requests_without_stored_data(self):
        text = json.dumps(self.config, ensure_ascii=False)
        for forbidden in ("expected_values", "close\": [\""):
            self.assertNotIn(forbidden, text)
        for task in self.config["tasks"]:
            for oracle in task["oracles"].values():
                self.assertEqual(oracle["request"]["contract_version"], "v2")

    def test_scopes_are_read_only_research_tools(self):
        for name, tools in self.config["scopes"].items():
            self.assertFalse(set(tools) & ev.OUT_OF_SCOPE_TOOLS, name)
            self.assertNotIn("bash", tools)
        baseline, new = self.config["scopes"]["baseline"], self.config["scopes"]["new"]
        self.assertIn("query_data", baseline)
        self.assertNotIn("get_daily_bars", baseline)
        self.assertTrue(set(baseline) < set(new))
        self.assertNotIn("query_data", self.config["scopes"]["new-no-query"])

    def test_write_tool_in_scope_is_rejected(self):
        config = json.loads((ev.DEFAULT_CONFIG).read_text(encoding="utf-8"))
        config["scopes"]["baseline"].append("update_data")
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp, "bad.json")
            path.write_text(json.dumps(config), encoding="utf-8")
            with self.assertRaises(ev.EvalError):
                ev.load_config(path)

    def test_pi_command_has_no_builtin_tools_and_only_scope_tools(self):
        allowed = [ev.pi_tool_name("x-data-mcp", t) for t in self.config["scopes"]["baseline"]]
        command = ev.build_pi_command("pi", allowed, "sys", "- prompt", "prov/model", "off")
        self.assertIn("--no-session", command)
        self.assertIn("--no-builtin-tools", command)
        self.assertEqual(command[command.index("--tools") + 1], ",".join(allowed))
        for builtin in ("bash", "read", "edit", "write"):
            self.assertNotIn(builtin, command[command.index("--tools") + 1].split(","))
        self.assertEqual(command[-2:], ["--", "- prompt"])
        self.assertEqual(command[command.index("--model") + 1], "prov/model")

    def test_mcp_project_config_points_at_the_socket(self):
        config = ev.mcp_project_config("x-data-mcp", "/bin/mcp_connect", "/run/research.sock", environ={})
        entry = config["mcpServers"]["x-data-mcp"]
        self.assertEqual((entry["command"], entry["args"], entry["exposure"]), ("/bin/mcp_connect", ["/run/research.sock"], "direct"))
        self.assertNotIn("env", entry)
        with_env = ev.mcp_project_config("s", "c", "p", environ={"X_DATA_MCP_UNSAFE_NONLOCAL_FS": "1", "SECRET": "x"})
        self.assertEqual(with_env["mcpServers"]["s"]["env"], {"X_DATA_MCP_UNSAFE_NONLOCAL_FS": "1"})


class Flattening(unittest.TestCase):
    def test_panel(self):
        envelope = {
            "layout": "panel", "entities": ["sh.600000", "sh.600004"],
            "dates": [["2026-11-04", "2026-11-05"], ["2026-11-05"]],
            "columns": {"close": [["10.1", "10.2"], ["9.9"]]},
        }
        rows = ev.envelope_rows(envelope)
        self.assertEqual(
            [(r["entity"], r["date"], r["close"], r["year"]) for r in rows],
            [("sh.600000", "2026-11-04", "10.1", "2026"), ("sh.600000", "2026-11-05", "10.2", "2026"),
             ("sh.600004", "2026-11-05", "9.9", "2026")],
        )

    def test_cross_section(self):
        envelope = {
            "layout": "cross_section", "trade_days": ["2026-11-05"], "entities": ["sh.600000", "sh.600004"],
            "columns": {"close": [["10.1", None]]},
        }
        rows = ev.envelope_rows(envelope)
        self.assertEqual([(r["entity"], r["close"]) for r in rows], [("sh.600000", "10.1"), ("sh.600004", None)])

    def test_table_with_and_without_entities(self):
        with_entities = {"layout": "table", "entities": ["a", "b"], "columns": {"name": ["n1", "n2"]}, "row_count": 2}
        self.assertEqual([r["name"] for r in ev.envelope_rows(with_entities)], ["n1", "n2"])
        calendar = {"layout": "table", "columns": {"trade_day": ["2026-11-04", "2026-11-05"]}, "row_count": 2}
        self.assertEqual([r["trade_day"] for r in ev.envelope_rows(calendar)], ["2026-11-04", "2026-11-05"])


class Extraction(unittest.TestCase):
    def test_date_spellings(self):
        dates, rest = ev.extract_dates("2026-11-05, 2026/11/04, 2026年11月3日, 20261102, 11月5日 开盘 10.05")
        self.assertEqual(dates, [(2026, 11, 5), (2026, 11, 4), (2026, 11, 3), (2026, 11, 2), (None, 11, 5)])
        self.assertEqual(ev.extract_numbers(rest), {ev.Decimal("10.05")})

    def test_numbers_with_separators_and_units(self):
        numbers = ev.extract_numbers("成交量 1,234,567 股，总股本 12.5亿，区间 10.05-10.20")
        for expected in ("1234567", "1250000000", "10.05", "10.20", "10.2"):
            self.assertIn(ev.Decimal(expected), numbers)

    def test_trailing_zeros_do_not_matter(self):
        self.assertIn(ev.Decimal("10.1"), ev.extract_numbers("收盘 10.10"))
        self.assertIn(ev.Decimal("10.10"), ev.extract_numbers("收盘 10.1"))


def task(checks, prompt="问题"):
    return {"id": "t", "prompt": prompt, "checks": checks}


BARS = [
    {"entity": "sh.600000", "date": "2026-11-04", "open": "10.00", "close": "10.05", "volume": "1000"},
    {"entity": "sh.600000", "date": "2026-11-05", "open": "10.05", "close": "10.10", "volume": "2000"},
]
BAR_CHECK = {"kind": "oracle_values", "oracle": "o", "fields": ["date", "open", "close", "volume"], "dates_subset_of": "selected"}


class Checks(unittest.TestCase):
    def run_checks(self, answer, rows=BARS, check=BAR_CHECK):
        return ev.evaluate_answer(task([check]), answer, {"o": rows}, TODAY)[0]

    def test_table_answer_passes(self):
        answer = (
            "| 日期 | 开盘 | 收盘 | 成交量 |\n|---|---|---|---|\n"
            "| 2026-11-04 | 10.00 | 10.05 | 1,000 |\n| 2026-11-05 | 10.05 | 10.10 | 2000 |"
        )
        result = self.run_checks(answer)
        self.assertTrue(result["ok"], result)
        self.assertEqual(result["detail"]["matched_rows"], 2)

    def test_wrong_number_fails_and_names_the_row(self):
        answer = "2026-11-04 开 10.00 收 10.05 量 1000\n2026-11-05 开 10.05 收 10.11 量 2000"
        result = self.run_checks(answer)
        self.assertFalse(result["ok"])
        self.assertEqual(result["detail"]["missing_rows"], ["date=2026-11-05,open=10.05,close=10.10,volume=2000"])

    def test_values_spread_over_different_lines_do_not_count(self):
        answer = "2026-11-04 开 10.00\n收 10.05 量 1000\n2026-11-05 开 10.05 收 10.10 量 2000"
        self.assertFalse(self.run_checks(answer)["ok"])

    def test_month_day_without_year_is_accepted(self):
        answer = "11月4日：开 10.00 收 10.05 量 1000\n11月5日：开 10.05 收 10.10 量 2000"
        self.assertTrue(self.run_checks(answer)["ok"])

    def test_extra_date_fails_subset_check(self):
        answer = (
            "2026-11-03 开 9.9 收 10.0 量 5\n2026-11-04 开 10.00 收 10.05 量 1000\n"
            "2026-11-05 开 10.05 收 10.10 量 2000"
        )
        result = self.run_checks(answer)
        self.assertFalse(result["ok"])
        self.assertEqual(result["detail"]["unexpected_dates"], ["2026-11-03"])

    def test_single_row_may_span_sentences(self):
        rows = [{"entity": "sh.600000", "industry_code": "X2603", "industry_name": "银行", "name": "浦发银行"}]
        check = {"kind": "oracle_values", "oracle": "o", "fields": ["entity", "industry_code", "industry_name"]}
        result = self.run_checks("代码是 sh.600000。行业代码 X2603，行业名称：银行。", rows, check)
        self.assertTrue(result["ok"], result)
        wrong = self.run_checks("代码是 sh.600001。行业代码 X2603，行业名称：银行。", rows, check)
        self.assertFalse(wrong["ok"])

    def test_code_match_is_not_a_substring_match(self):
        rows = [{"entity": "sh.600000", "close": "1.5"}]
        check = {"kind": "oracle_values", "oracle": "o", "fields": ["entity", "close"]}
        self.assertFalse(self.run_checks("sh.6000001 收盘 1.5", rows, check)["ok"])

    def test_where_and_select(self):
        rows = [{"name": "A", "v": "1"}, {"name": "B", "v": "2"}, {"name": "B", "v": "3"}]
        self.assertEqual(ev.select_rows(rows, {"where": {"name": ["B"]}, "select": "head:1"}), [rows[1]])
        self.assertEqual(ev.select_rows(rows, {"select": "tail:2"}), rows[1:])

    def test_tail_selection_drops_older_rows(self):
        check = dict(BAR_CHECK, select="tail:1")
        result = self.run_checks("2026-11-05 开 10.05 收 10.10 量 2000", check=check)
        self.assertTrue(result["ok"], result)

    def test_empty_oracle_is_an_error_unless_allowed(self):
        with self.assertRaises(ev.EvalError):
            self.run_checks("x", rows=[])
        allowed = dict(BAR_CHECK, allow_empty=True)
        self.assertTrue(self.run_checks("没有记录", rows=[], check=allowed)["ok"])
        self.assertFalse(self.run_checks("2019-01-02 有记录", rows=[], check=allowed)["ok"])

    def test_interval_end_may_be_stated_as_last_day(self):
        rows = [{"normalized_status": "other_risk_warning", "effective_from": "2024-05-01", "effective_to": "2025-02-01"}]
        check = {
            "kind": "oracle_values", "oracle": "o", "fields": ["effective_from", "effective_to"],
            "date_offsets": {"effective_to": [0, -1]}, "dates_subset_of": "all",
        }
        self.assertTrue(self.run_checks("2024-05-01 至 2025-01-31 被实施风险警示", rows, check)["ok"])
        self.assertTrue(self.run_checks("2024-05-01 至 2025-02-01 被实施风险警示", rows, check)["ok"])
        self.assertFalse(self.run_checks("2024-05-01 至 2025-03-01 被实施风险警示", rows, check)["ok"])

    def test_verdict(self):
        check = {
            "kind": "verdict", "oracle": "o", "status_field": "normalized_status",
            "yes_status": ["other_risk_warning"], "no_status": ["normal"],
        }
        st = [{"normalized_status": "other_risk_warning"}]
        self.assertTrue(self.run_checks("依据……\n结论：是", st, check)["ok"])
        self.assertFalse(self.run_checks("结论：否", st, check)["ok"])
        self.assertTrue(self.run_checks("结论：否", [{"normalized_status": "normal"}], check)["ok"])
        self.assertTrue(self.run_checks("结论：无法确定", [], check)["ok"])
        self.assertTrue(self.run_checks("结论： **无法确定**", [{"normalized_status": "unknown"}], check)["ok"])
        self.assertFalse(self.run_checks("可能是", st, check)["ok"])
        # the last verdict line wins
        self.assertTrue(self.run_checks("结论：否\n更正，结论：是", st, check)["ok"])


class FakeClient:
    """Stands in for the read-only MCP client: answers as_of queries from interval rows."""

    def __init__(self, intervals):
        self.intervals = intervals
        self.requests = []

    def call(self, name, arguments):
        assert name == "query_data"
        self.requests.append(arguments)
        as_of = arguments["filter"]["as_of"]
        hits = [i for i in self.intervals if i[0] <= as_of and (i[1] is None or as_of < i[1])]
        return {
            "layout": "table", "entities": ["sz.002731"] * len(hits), "row_count": len(hits), "next_cursor": None,
            "columns": {
                "effective_from": [i[0] for i in hits],
                "effective_to": [i[1] for i in hits],
                "normalized_status": [i[2] for i in hits],
            },
        }


class HistoryWalk(unittest.TestCase):
    SPEC = {
        "kind": "walk_as_of", "start": "2020-01-01", "gap_step_days": 30, "max_queries": 200,
        "request": {
            "dataset": "market.risk_warnings", "contract_version": "v2",
            "selection": {"kind": "single", "entity": "sz.002731"},
            "filter": {"kind": "daily", "start_date": "{as_of}", "end_date": "{as_of}", "as_of": "{as_of}"},
        },
    }

    def test_finds_every_interval_with_few_queries(self):
        intervals = [
            ("2019-06-01", "2020-03-01", "other_risk_warning"),
            ("2020-03-01", "2022-01-01", "normal"),
            ("2022-01-01", "2022-09-15", "delisting_risk_warning"),
            ("2022-09-15", None, "normal"),
        ]
        client = FakeClient(intervals)
        rows = ev.walk_as_of(client, self.SPEC, TODAY)
        self.assertEqual(
            [(r["effective_from"], r["effective_to"], r["normalized_status"]) for r in rows],
            [(a, b, c) for a, b, c in intervals],
        )
        self.assertEqual(client.requests[0]["filter"]["as_of"], "2020-01-01")
        self.assertLess(len(client.requests), 10)

    def test_gap_is_stepped_over(self):
        client = FakeClient([("2020-05-10", "2020-06-01", "other_risk_warning")])
        rows = ev.walk_as_of(client, self.SPEC, TODAY)
        self.assertEqual(len(rows), 1)

    def test_query_budget_is_enforced(self):
        spec = dict(self.SPEC, max_queries=3, gap_step_days=1)
        with self.assertRaises(ev.EvalError):
            ev.walk_as_of(FakeClient([]), spec, TODAY)


class ReadOnlyClient(unittest.TestCase):
    def test_only_query_data_is_callable(self):
        client = ev.McpClient.__new__(ev.McpClient)  # no process started
        for name in ("update_data", "start_collection", "run_market_update", "get_task"):
            with self.assertRaises(ev.EvalError):
                client.call(name, {})


FAKE_SERVER = r"""
import json, sys
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request["method"]
    if method == "initialize":
        result = {"protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": {"name": "x-data-mcp"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "query_data"}, {"name": "find_securities"}]}
    elif request["params"]["arguments"].get("fail"):
        result = {"isError": True, "content": [{"type": "text", "text": "bad"}]}
    else:
        result = {"content": [{"type": "text", "text": json.dumps({"layout": "table", "row_count": 0})}]}
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
"""


class McpClientProtocol(unittest.TestCase):
    def test_talks_to_a_stdio_server(self):
        with tempfile.TemporaryDirectory() as tmp:
            script = Path(tmp, "server.py")
            script.write_text(FAKE_SERVER, encoding="utf-8")
            client = ev.McpClient([sys.executable, str(script)], timeout=10)
            try:
                self.assertEqual(client.list_tools(), ["query_data", "find_securities"])
                self.assertEqual(client.call("query_data", {})["layout"], "table")
                with self.assertRaises(ev.EvalError):
                    client.call("query_data", {"fail": True})
                with self.assertRaises(ev.EvalError):
                    client.call("find_securities", {})  # not an oracle tool
            finally:
                client.close()


class FakeRunner:
    def __init__(self, stdout, code=0, stderr="", wall=1.5):
        self.result = (code, stdout, stderr, wall)
        self.commands = []

    def __call__(self, command, cwd, timeout):
        self.commands.append(command)
        return self.result


class OneQuestion(unittest.TestCase):
    def setUp(self):
        self.config = ev.load_config()
        self.scope_tools = [ev.pi_tool_name("x-data-mcp", t) for t in self.config["scopes"]["baseline"]]
        self.task = {
            "id": "qx", "title": "t", "prompt": "查 {today}",
            "oracles": {"o": {"request": {"dataset": "market.daily", "contract_version": "v2"}}},
            "checks": [dict(BAR_CHECK)],
        }
        self.client = type("C", (), {"call": lambda self, name, args: {
            "layout": "panel", "entities": ["sh.600000"], "dates": [["2026-11-05"]],
            "columns": {"open": [["10.05"]], "close": [["10.10"]], "volume": [["2000"]]}, "next_cursor": None,
        }})()

    def ctx(self, runner, **extra):
        return {
            "config": self.config, "pi": "pi", "allowed": self.scope_tools, "model": "p/m", "thinking": "off",
            "today": TODAY, "timeout": 60, "client": self.client, "runner": runner, "workdir": "/tmp", **extra,
        }

    def stream(self, answer, extra_tool=None):
        messages = [
            system(*self.scope_tools),
            assistant(calls=[("c1", self.scope_tools[1], {})], stop="toolUse", input_=100, output=10),
            tool_result("c1", self.scope_tools[1]),
            assistant(answer, input_=50, output=20, cache_read=100),
        ]
        return "\n".join(json_events(messages))

    def test_correct_answer(self):
        runner = FakeRunner(self.stream("2026-11-05 开 10.05 收 10.10 量 2000"))
        report = ev.run_task(self.task, self.ctx(runner))
        self.assertEqual((report["status"], report["correct"]), ("ok", True))
        self.assertEqual((report["turns"], report["tool_calls"], report["tool_errors"]), (2, 1, 0))
        self.assertEqual(report["tokens"]["total"], 280)
        self.assertEqual(report["duration_s"], 1.5)
        self.assertIn("查 2026-11-05", runner.commands[0][-1])

    def test_report_keeps_the_expected_values_for_regrading(self):
        report = ev.run_task(self.task, self.ctx(FakeRunner(self.stream("x"))))
        self.assertEqual(report["oracle"]["o"][0]["close"], "10.10")
        self.assertEqual(report["group"], "core")

    def test_wrong_answer_is_a_measurement_not_an_error(self):
        report = ev.run_task(self.task, self.ctx(FakeRunner(self.stream("2026-11-05 开 10.05 收 99 量 2000"))))
        self.assertEqual((report["status"], report["correct"]), ("ok", False))

    def test_timeout_and_exit_code(self):
        timed_out = ev.run_task(self.task, self.ctx(FakeRunner("", code=None)))
        self.assertEqual((timed_out["status"], timed_out["correct"]), ("timeout", None))
        failed = ev.run_task(self.task, self.ctx(FakeRunner("", code=3, stderr="boom")))
        self.assertEqual(failed["status"], "pi_error")
        self.assertIn("boom", failed["error"])

    def test_scope_mismatch_is_reported(self):
        messages = [system(*self.scope_tools, "bash"), assistant("x")]
        report = ev.run_task(self.task, self.ctx(FakeRunner("\n".join(json_events(messages)))))
        self.assertEqual(report["status"], "scope_error")
        self.assertEqual(report["scope_violation"]["unexpected"], ["bash"])

    def test_oracle_failure_is_reported_without_running_pi(self):
        class Broken:
            def call(self, name, args):
                raise ev.EvalError("service down")

        runner = FakeRunner("")
        report = ev.run_task(self.task, self.ctx(runner, client=Broken()))
        self.assertEqual(report["status"], "oracle_error")
        self.assertEqual(runner.commands, [])

    def test_events_are_kept_when_asked(self):
        with tempfile.TemporaryDirectory() as tmp:
            ev.run_task(self.task, self.ctx(FakeRunner(self.stream("x")), events_dir=tmp))
            self.assertTrue(Path(tmp, "qx.jsonl").is_file())

    def test_report_and_summary(self):
        runner = FakeRunner(self.stream("2026-11-05 开 10.05 收 10.10 量 2000"))
        config = dict(self.config, tasks=[self.task, dict(self.task, id="qy")])
        report = ev.run_eval(config, "baseline", self.ctx(runner))
        summary = report["summary"]
        self.assertEqual((summary["tasks"], summary["correct"], summary["infrastructure_errors"]), (2, 2, 0))
        self.assertEqual(summary["avg_turns"], 2.0)
        self.assertEqual(summary["tokens"]["total"], 560)
        self.assertEqual(summary["fresh_tokens"], 360)
        self.assertEqual(summary["tool_calls"], 2)
        self.assertEqual(report["models_seen"], ["p/m"])
        json.dumps(report, ensure_ascii=False)  # serializable

    def test_only_filter(self):
        config = dict(self.config, tasks=[self.task, dict(self.task, id="qy")])
        report = ev.run_eval(config, "baseline", self.ctx(FakeRunner(self.stream("x"))), only=["qy"])
        self.assertEqual([t["id"] for t in report["tasks"]], ["qy"])


Q02_ROWS = [
    {"entity": "sh.600000", "date": d, "open": o, "high": h, "low": l, "close": c, "volume": v}
    for d, o, h, l, c, v in [
        ("2026-10-30", "10.00", "10.30", "9.90", "10.20", "1000"),
        ("2026-11-02", "10.20", "10.50", "10.10", "10.40", "1100"),
        ("2026-11-03", "10.40", "10.60", "10.30", "10.50", "1200"),
        ("2026-11-04", "10.50", "10.70", "10.40", "10.60", "1300"),
        ("2026-11-05", "10.60", "10.80", "10.50", "10.70", "1400"),
    ]
]
Q02_CHECK = {
    "kind": "oracle_values", "oracle": "o", "select": "tail:5",
    "fields": ["date", "open", "high", "low", "close", "volume"], "dates_subset_of": "selected",
}
Q02_TABLE = (
    "| 日期 | 开 | 高 | 低 | 收 | 量 |\n|---|---|---|---|---|---|\n"
    "| 2026-10-30 | 10.00 | 10.30 | 9.90 | 10.20 | 1000 |\n"
    "| 2026-11-02 | 10.20 | 10.50 | 10.10 | 10.40 | 1100 |\n"
    "| 2026-11-03 | 10.40 | 10.60 | 10.30 | 10.50 | 1200 |\n"
    "| 2026-11-04 | 10.50 | 10.70 | 10.40 | 10.60 | 1300 |\n"
    "| 2026-11-05 | 10.60 | 10.80 | 10.50 | 10.70 | 1400 |\n"
)


class StrayDates(unittest.TestCase):
    def grade(self, answer):
        return ev.evaluate_answer(task([Q02_CHECK]), answer, {"o": Q02_ROWS}, TODAY)[0]

    def test_date_in_a_note_does_not_fail_a_correct_table(self):
        # the q02 case: all 5 rows right, the note names a range that is not a row
        answer = Q02_TABLE + "\n备注：2026-10-01 到 2026-11-05 是 unresolved range，数据来自 get_daily_bars。"
        result = self.grade(answer)
        self.assertTrue(result["ok"], result)
        self.assertEqual(result["detail"]["matched_rows"], 5)
        self.assertNotIn("unexpected_dates", result["detail"])
        self.assertEqual(result["detail"]["dates_in_notes"], ["2026-10-01"])

    def test_note_in_bullet_and_table_cell_text(self):
        answer = Q02_TABLE + "\n- 说明：2026-09-15 之前的数据未请求\n注：2026年9月1日 起无停牌"
        self.assertTrue(self.grade(answer)["ok"])

    def test_extra_data_row_in_a_table_fails(self):
        answer = Q02_TABLE + "| 2026-10-29 | 9.90 | 10.10 | 9.80 | 10.00 | 900 |\n"
        result = self.grade(answer)
        self.assertFalse(result["ok"])
        self.assertEqual(result["detail"]["unexpected_dates"], ["2026-10-29"])

    def test_extra_data_row_in_prose_lines_fails(self):
        answer = (
            "2026-10-29 开 9.90 高 10.10 低 9.80 收 10.00 量 900\n"
            + Q02_TABLE.replace("|", " ").replace("---", "")
        )
        result = self.grade(answer)
        self.assertFalse(result["ok"])
        self.assertEqual(result["detail"]["unexpected_dates"], ["2026-10-29"])

    def test_table_row_with_only_a_date_and_a_value_counts(self):
        self.assertFalse(self.grade(Q02_TABLE + "| 2026-10-28 | 9.5 |\n")["ok"])

    def test_row_with_an_expected_value_is_a_row_even_in_a_sentence(self):
        answer = Q02_TABLE + "\n另外 2026-10-29 开盘 10.00 收盘 10.20 成交量 1000"
        result = self.grade(answer)
        self.assertFalse(result["ok"])
        self.assertEqual(result["detail"]["unexpected_dates"], ["2026-10-29"])

    def test_interval_check_note_vs_listed_period(self):
        rows = [{"normalized_status": "other_risk_warning", "effective_from": "2024-05-01", "effective_to": "2025-02-01"}]
        check = {
            "kind": "oracle_values", "oracle": "o", "fields": ["effective_from", "effective_to"],
            "date_offsets": {"effective_to": [0, -1]}, "dates_subset_of": "all",
        }
        run = lambda answer: ev.evaluate_answer(task([check]), answer, {"o": rows}, TODAY)[0]  # noqa: E731
        note = "2024-05-01 至 2025-01-31 被实施风险警示。\n说明：数据从 2013-01-01 开始查。"
        self.assertTrue(run(note)["ok"])
        extra = "- 2024-05-01 至 2025-01-31 被实施风险警示\n- 2021-03-01 至 2021-08-01 被实施风险警示"
        result = run(extra)
        self.assertFalse(result["ok"])
        self.assertEqual(result["detail"]["unexpected_dates"], ["2021-03-01", "2021-08-01"])


def screen_task(rule, request=None):
    return {
        "kind": "screen", "batch_size": 2,
        "members": {"dataset": "securities.catalog", "contract_version": "v2", "page_size": 1000},
        "request": request or {"dataset": "market.daily", "contract_version": "v2", "page_size": 1000},
        "rule": rule,
    }


class ScreenClient:
    """query_data stand-in: catalog members, then rows of whatever batch is asked."""

    def __init__(self, members, rows):
        self.members, self.rows, self.batches = members, rows, []

    def call(self, name, arguments):
        assert name == "query_data"
        if arguments["dataset"] == "securities.catalog":
            return {"layout": "table", "entities": self.members, "row_count": len(self.members),
                    "columns": {"name": ["n"] * len(self.members)}, "next_cursor": None}
        entities = arguments["selection"]["entities"]
        self.batches.append(entities)
        out = [(e, r) for e in entities for r in self.rows.get(e, [])]
        return {
            "layout": "table", "entities": [e for e, _ in out], "row_count": len(out), "next_cursor": None,
            "columns": {field: [r.get(field) for _, r in out] for field in ("year", "open", "close")},
        }


class Screens(unittest.TestCase):
    YEARS = {
        "sz.000002": [("2022", "5", "6"), ("2023", "5", "4"), ("2024", "4", "3"), ("2025", "3", "2")],  # match
        "sh.600001": [("2022", "5", "6"), ("2023", "5", "4"), ("2024", "4", "3"), ("2025", "3", "2")],  # match
        "sh.600002": [("2023", "5", "4"), ("2024", "4", "3"), ("2025", "3", "2")],  # no 2022
        "sz.000003": [("2022", "5", "6"), ("2023", "5", "4"), ("2024", "4", "5"), ("2025", "3", "2")],  # 2024 up
        "sz.000004": [("2022", "5", "6"), ("2023", "5", "5"), ("2024", "4", "3"), ("2025", "3", "2")],  # 2023 flat
    }

    def rows(self):
        return {e: [{"year": y, "open": o, "close": c} for y, o, c in v] for e, v in self.YEARS.items()}

    def test_yearly_screen_in_batches_sorted_by_code(self):
        rule = {"type": "yearly_down", "present_years": [2022, 2023, 2024, 2025], "down_years": [2023, 2024, 2025]}
        client = ScreenClient(["sz.000002", "sh.600001", "sh.600002", "sz.000003", "sz.000004"], self.rows())
        out = ev.screen_rows(client, screen_task(rule), TODAY)
        self.assertEqual([r["entity"] for r in out], ["sz.000002", "sh.600001"])
        self.assertEqual([r["code"] for r in out], ["000002", "600001"])
        self.assertEqual([len(b) for b in client.batches], [2, 2, 1])

    def test_close_below(self):
        rule = {"type": "close_below", "limit": "2"}
        rows = {"sz.000001": [{"close": "1.99"}], "sz.000002": [{"close": "2.00"}], "sz.000003": []}
        out = ev.screen_rows(ScreenClient(["sz.000001", "sz.000002", "sz.000003"], rows), screen_task(rule), TODAY)
        self.assertEqual([r["entity"] for r in out], ["sz.000001"])

    def test_empty_catalog_or_no_rows_is_an_oracle_error(self):
        rule = {"type": "close_below", "limit": "2"}
        with self.assertRaises(ev.EvalError):
            ev.screen_rows(ScreenClient([], {}), screen_task(rule), TODAY)
        with self.assertRaises(ev.EvalError):
            ev.screen_rows(ScreenClient(["sz.000001"], {}), screen_task(rule), TODAY)

    def test_screen_check(self):
        rows = [{"entity": f"sz.{i:06d}", "code": f"{i:06d}", "rank": i} for i in range(1, 13)]
        check = {"kind": "screen", "oracle": "o", "top": 10}
        run = lambda answer: ev.evaluate_answer(task([check]), answer, {"o": rows}, TODAY)[0]  # noqa: E731
        codes = ", ".join(f"{i:06d}" for i in range(1, 11))
        good = run(f"共 12 只。前 10 只：{codes}")
        self.assertTrue(good["ok"], good)
        self.assertFalse(run(f"共 13 只。前 10 只：{codes}")["ok"])
        wrong = run("共 12 只。前 10 只：" + codes.replace("000010", "000011"))
        self.assertFalse(wrong["ok"])
        self.assertEqual(wrong["detail"]["missing_codes"], ["000010"])
        shuffled = run("共 12 只：" + ", ".join(f"{i:06d}" for i in (2, 1, *range(3, 11))))
        self.assertFalse(shuffled["ok"])
        self.assertTrue(shuffled["detail"]["order_mismatch"])
        # a date in the answer is not mistaken for the total
        self.assertFalse(run(f"2026-11-12 的结果：{codes}")["ok"])

    def test_screen_check_accepts_symbol_order_and_records_it(self):
        rows = [{"entity": "sz.000001", "code": "000001"}, {"entity": "sh.600000", "code": "600000"}]
        rows.sort(key=lambda r: (r["code"], r["entity"]))
        check = {"kind": "screen", "oracle": "o", "top": 10}
        result = ev.evaluate_answer(task([check]), "共 2 只：600000, 000001", {"o": rows}, TODAY)[0]
        self.assertTrue(result["ok"])
        self.assertEqual(result["detail"]["order"], "symbol")

    def test_screen_questions_in_the_config(self):
        config = ev.load_config()
        by_id = {t["id"]: t for t in config["tasks"]}
        for qid in ("q11", "q12"):
            spec = by_id[qid]["oracles"]["screen"]
            self.assertEqual((spec["kind"], spec["batch_size"]), ("screen", 100))
            self.assertEqual(spec["members"]["selection"]["scope"], "current_sh_sz_a")
        self.assertEqual(by_id["q11"]["oracles"]["screen"]["rule"]["down_years"], [2023, 2024, 2025])
        self.assertEqual(by_id["q11"]["oracles"]["screen"]["rule"]["present_years"], [2022, 2023, 2024, 2025])
        self.assertEqual(by_id["q12"]["oracles"]["screen"]["rule"], {"type": "close_below", "limit": "2"})
        self.assertEqual(by_id["q12"]["oracles"]["screen"]["request"]["filter"]["start_date"], "2026-11-05")


class EtfName(unittest.TestCase):
    def setUp(self):
        self.q13 = {t["id"]: t for t in ev.load_config()["tasks"]}["q13"]

    def outcome(self, answer):
        result = ev.evaluate_answer(self.q13, answer, {}, TODAY)[0]
        return result["ok"], result["detail"]["outcome"]

    def test_code_given(self):
        self.assertEqual(self.outcome("沪深 300 ETF 的代码是 sh.510300。"), (True, "found_code"))
        self.assertEqual(self.outcome("可确认：510300（华泰柏瑞），另有 159919"), (True, "found_code"))

    def test_honest_decline_is_recorded_apart(self):
        self.assertEqual(self.outcome("现有工具无法按名称查询 ETF，我不能确认代码。"), (True, "declared_unsupported"))
        self.assertEqual(self.outcome("目录里查不到沪深 300 ETF，find_securities 不覆盖 ETF。"), (True, "declared_unsupported"))

    def test_wrong_or_silent_answers_fail(self):
        self.assertEqual(self.outcome("代码是 sh.510330。"), (False, "neither"))
        self.assertEqual(self.outcome("沪深 300 ETF 有很多只。"), (False, "neither"))

    def test_no_oracle_is_needed(self):
        self.assertEqual(self.q13["oracles"], {})


class WrongReason(unittest.TestCase):
    def test_one_line_per_question_and_groups_apart(self):
        def made(task_id, group, correct, checks=(), status="ok", error=None):
            item = {
                "id": task_id, "group": group, "status": status, "correct": correct, "checks": list(checks),
                "turns": 2, "tokens": {k: 1 for k in ("input", "output", "cache_read", "cache_write", "total")},
                "tool_calls": 1, "tool_errors": 0, "tool_result_bytes": 3, "duration_s": 1.0,
            }
            if error:
                item["error"] = error
            return item

        bad = {"kind": "oracle_values", "ok": False,
               "detail": {"expected_rows": 5, "matched_rows": 3, "missing_rows": ["date=2026-11-04"]}}
        stray = {"kind": "oracle_values", "ok": False,
                 "detail": {"expected_rows": 1, "matched_rows": 1, "unexpected_dates": ["2026-10-29"]}}
        tasks = [
            made("q01", "core", True, [{"kind": "verdict", "ok": True}]),
            made("q02", "core", False, [bad]),
            made("q03", "core", False, [stray]),
            made("q07", "core", None, status="timeout", error="pi did not finish within 300s"),
            made("q11", "screen", False, [{"kind": "screen", "ok": False, "detail": {
                "expected_total": 12, "total_stated": False, "got_top": [], "expected_top": ["1"], "order": None}}]),
        ]
        summary = ev.summarize(tasks)
        reasons = summary["wrong_reason"]
        self.assertEqual(reasons["q01"], "")
        self.assertIn("2 of 5 expected rows missing", reasons["q02"])
        self.assertIn("unexpected dates in data rows: 2026-10-29", reasons["q03"])
        self.assertTrue(reasons["q07"].startswith("not graded: timeout"))
        self.assertIn("total 12 not stated", reasons["q11"])
        self.assertEqual(len(reasons), 5)
        # top-level numbers are the core group; screens are listed apart
        self.assertEqual((summary["tasks"], summary["correct"], summary["incorrect"]), (4, 1, 2))
        self.assertEqual(summary["groups"]["screen"]["tasks"], 1)
        self.assertEqual(list(summary["groups"]), ["core", "screen"])


class Regrade(unittest.TestCase):
    def report(self, answer, with_oracle=True):
        item = {
            "id": "qa", "title": "t", "status": "ok", "correct": False, "answer": answer,
            "checks": [{"kind": "oracle_values", "ok": False, "detail": {"unexpected_dates": ["2026-10-01"]}}],
            "turns": 2, "tokens": {k: 1 for k in ("input", "output", "cache_read", "cache_write", "total")},
            "tool_calls": 1, "tool_errors": 0, "tool_result_bytes": 3, "duration_s": 1.0,
        }
        if with_oracle:
            item["oracle"] = {"o": Q02_ROWS}
        return {"eval": "e", "scope": "new", "today": "2026-11-05", "finished_at": "f", "tasks": [item]}

    def config(self):
        return {"tasks": [{"id": "qa", "prompt": "问题", "checks": [Q02_CHECK], "group": "core", "oracles": {"o": {}}}]}

    def test_regrade_uses_saved_answer_and_expected_values(self):
        answer = Q02_TABLE + "\n备注：2026-10-01 到 2026-11-05 是 unresolved range"
        new, notes = ev.regrade_report(self.report(answer), self.config())
        item = new["tasks"][0]
        self.assertEqual((item["correct"], item["previous_correct"], item["regrade"]), (True, False, "regraded"))
        self.assertEqual(new["summary"]["correct"], 1)
        self.assertEqual(new["summary"]["wrong_reason"], {"qa": ""})
        self.assertEqual(notes, [])
        self.assertEqual(new["regraded_from"]["finished_at"], "f")

    def test_report_without_expected_values_keeps_its_old_result(self):
        original = self.report("x", with_oracle=False)
        new, notes = ev.regrade_report(original, self.config())
        item = new["tasks"][0]
        self.assertEqual((item["correct"], item["regrade"]), (False, "skipped: the report holds no expected values"))
        self.assertEqual(len(notes), 1)
        self.assertNotIn("previous_correct", item)

    def test_regrade_runs_without_model_or_service(self):
        with tempfile.TemporaryDirectory() as tmp:
            source, target = Path(tmp, "old.json"), Path(tmp, "new.json")
            source.write_text(json.dumps(self.report(Q02_TABLE)), encoding="utf-8")
            config = Path(tmp, "eval.json")
            base = ev.load_config()
            config.write_text(json.dumps(dict(base, tasks=self.config()["tasks"])), encoding="utf-8")
            import contextlib
            import io

            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                code = ev.main(["--regrade", str(source), "--output", str(target), "--eval", str(config)])
            self.assertEqual(code, 0)
            self.assertIn("qa False->True", out.getvalue())
            self.assertTrue(json.loads(target.read_text(encoding="utf-8"))["tasks"][0]["correct"])
            self.assertEqual(json.loads(source.read_text(encoding="utf-8"))["tasks"][0]["correct"], False)

    def test_bad_report_file_is_exit_2(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp, "bad.json")
            path.write_text("not json", encoding="utf-8")
            self.assertEqual(ev.main(["--regrade", str(path), "--output", str(Path(tmp, "o.json"))]), 2)


def codemode_result(call_id, inner, error=False, details=True):
    """A ``codemode`` toolResult as pi writes it: ``details.calls`` lists the calls made inside the script."""
    message = tool_result(call_id, "codemode", "Script failed" if error else "Script completed", error=error)
    if details:
        message["details"] = {"calls": inner}
    return message


def inner_call(name, status="ok"):
    call = {"id": "x", "name": name, "args": "{}", "status": status}
    if status == "error":
        call["error"] = "boom"
    return call


class CodemodeMode(unittest.TestCase):
    def setUp(self):
        self.config = ev.load_config()
        self.scope_tools = [ev.pi_tool_name("x-data-mcp", t) for t in self.config["scopes"]["baseline"]]
        self.query = self.scope_tools[1]
        self.task = {
            "id": "qx", "title": "t", "prompt": "查 {today}",
            "oracles": {"o": {"request": {"dataset": "market.daily", "contract_version": "v2"}}},
            "checks": [dict(BAR_CHECK)],
        }
        self.client = type("C", (), {"call": lambda self, name, args: {
            "layout": "panel", "entities": ["sh.600000"], "dates": [["2026-11-05"]],
            "columns": {"open": [["10.05"]], "close": [["10.10"]], "volume": [["2000"]]}, "next_cursor": None,
        }})()

    def ctx(self, runner, mode="codemode"):
        return {
            "config": self.config, "pi": "pi", "allowed": self.scope_tools, "model": "p/m", "thinking": "off",
            "today": TODAY, "timeout": 60, "client": self.client, "runner": runner, "workdir": "/tmp", "mode": mode,
        }

    def stream(self, declared=("codemode",), answer="2026-11-05 开 10.05 收 10.10 量 2000", extra=()):
        messages = [
            system(*declared),
            assistant(calls=[("c1", "codemode", {"code": "..."})], stop="toolUse", input_=100, output=10),
            codemode_result("c1", [inner_call(self.query), inner_call(self.query, "error"), inner_call("bash")]),
            assistant(calls=[("c2", "codemode", {"code": "bad"})], stop="toolUse", input_=40, output=5),
            codemode_result("c2", [], error=True),
            assistant(calls=[("c3", "codemode", {"code": "..."})], stop="toolUse", input_=40, output=5),
            codemode_result("c3", [inner_call(self.query)]),
            *extra,
            assistant(answer, input_=50, output=20, cache_read=100),
        ]
        return "\n".join(json_events(messages))

    def test_command_has_only_the_codemode_tool_and_keeps_extensions(self):
        command = ev.build_pi_command("pi", self.scope_tools, "sys", "p", "prov/m", "off", "codemode")
        self.assertEqual(command[command.index("--tools") + 1], "codemode")
        self.assertNotIn("--no-builtin-tools", command)
        self.assertNotIn("--no-extensions", command)
        self.assertNotIn("-e", command)
        self.assertIn("--no-session", command)
        self.assertEqual(command[-2:], ["--", "p"])

    def test_direct_command_is_unchanged_by_default(self):
        default = ev.build_pi_command("pi", self.scope_tools, "sys", "p", "prov/m", "off")
        direct = ev.build_pi_command("pi", self.scope_tools, "sys", "p", "prov/m", "off", "direct")
        self.assertEqual(default, direct)
        for flag in ("--no-builtin-tools", "--no-extensions"):
            self.assertIn(flag, direct)
        with self.assertRaises(ev.EvalError):
            ev.build_pi_command("pi", [], "s", "p", mode="nope")

    def test_mcp_config_uses_default_exposure_in_codemode(self):
        entry = ev.mcp_project_config("s", "c", "p", environ={}, mode="codemode")["mcpServers"]["s"]
        self.assertNotIn("exposure", entry)
        self.assertEqual((entry["command"], entry["args"]), ("c", ["p"]))
        direct = ev.mcp_project_config("s", "c", "p", environ={})["mcpServers"]["s"]
        self.assertEqual(direct["exposure"], "direct")

    def test_script_counts(self):
        stats = ev.collect_stats(ev.iter_records(self.stream().splitlines()), "codemode", set(self.scope_tools))
        cm = stats["codemode"]
        self.assertEqual(cm["calls"], 3)
        self.assertEqual(cm["script_failures"], 1)
        self.assertEqual(cm["tool_calls"], 4)
        self.assertEqual(cm["tools"], {"bash": 1, self.query: 3})
        self.assertEqual(cm["tool_errors"], 1)
        self.assertEqual(cm["unexpected_tool_calls"], ["bash"])
        self.assertEqual(cm["results_without_details"], 0)
        # the model's own calls: three scripts, one of them failed
        self.assertEqual((stats["tool_calls"], stats["tools"], stats["tool_errors"]), (3, {"codemode": 3}, 1))

    def test_direct_stats_have_no_codemode_block(self):
        stats = ev.collect_stats(ev.iter_records(json_events(CONVERSATION)))
        self.assertNotIn("codemode", stats)

    def test_result_without_details_is_flagged(self):
        messages = [system("codemode"), assistant(calls=[("c1", "codemode", {})], stop="toolUse"),
                    codemode_result("c1", [], details=False), assistant("x")]
        stats = ev.collect_stats(ev.iter_records(json_events(messages)), "codemode", set())
        self.assertEqual(stats["codemode"]["results_without_details"], 1)
        self.assertEqual(stats["codemode"]["tool_calls"], 0)

    def test_question_in_codemode(self):
        runner = FakeRunner(self.stream())
        report = ev.run_task(self.task, self.ctx(runner))
        self.assertEqual((report["status"], report["correct"]), ("ok", True))
        self.assertEqual(report["codemode"]["tool_calls"], 4)
        self.assertNotIn("--no-builtin-tools", runner.commands[0])

    def test_other_declared_tools_are_a_scope_error(self):
        for declared in (("codemode", "bash"), ("codemode", self.query), ("bash",)):
            report = ev.run_task(self.task, self.ctx(FakeRunner(self.stream(declared))))
            self.assertEqual(report["status"], "scope_error", declared)
            self.assertIsNone(report["correct"])
        self.assertEqual(report["scope_violation"], {"missing": ["codemode"], "unexpected": ["bash"]})

    def test_direct_scope_tools_declared_in_codemode_mode_are_a_scope_error(self):
        report = ev.run_task(self.task, self.ctx(FakeRunner(self.stream(tuple(self.scope_tools)))))
        self.assertEqual(report["status"], "scope_error")

    def test_report_and_summary_record_the_mode(self):
        config = dict(self.config, tasks=[self.task, dict(self.task, id="qy")])
        report = ev.run_eval(config, "baseline", self.ctx(FakeRunner(self.stream())))
        self.assertEqual(report["mode"], "codemode")
        self.assertEqual(report["summary"]["correct"], 2)
        self.assertEqual(
            report["summary"]["codemode"],
            {"calls": 6, "script_failures": 2, "tool_calls": 8, "tool_errors": 2, "results_without_details": 0},
        )
        self.assertEqual(report["summary"]["groups"]["core"]["codemode"]["calls"], 6)

    def test_direct_report_records_mode_and_has_no_codemode_summary(self):
        runner = FakeRunner("\n".join(json_events([system(*self.scope_tools), assistant("2026-11-05 开 10.05 收 10.10 量 2000")])))
        config = dict(self.config, tasks=[self.task])
        report = ev.run_eval(config, "baseline", self.ctx(runner, mode="direct"))
        self.assertEqual(report["mode"], "direct")
        self.assertNotIn("codemode", report["summary"])
        no_mode = self.ctx(runner)
        del no_mode["mode"]
        self.assertEqual(ev.run_eval(config, "baseline", no_mode)["mode"], "direct")

    def test_regrade_keeps_mode_and_script_counts(self):
        config = dict(self.config, tasks=[self.task])
        report = ev.run_eval(config, "baseline", self.ctx(FakeRunner(self.stream())))
        new, _ = ev.regrade_report(report, config)
        self.assertEqual(new["mode"], "codemode")
        self.assertEqual(new["summary"]["codemode"]["calls"], 3)


class CommandLine(unittest.TestCase):
    def test_dry_run_prints_commands_and_runs_nothing(self):
        import contextlib
        import io

        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = ev.main(["--scope", "new", "--socket", "/nonexistent", "--output", "/nonexistent/x.json", "--dry-run", "--today", "2026-11-05"])
        self.assertEqual(code, 0)
        lines = out.getvalue().splitlines()
        self.assertEqual(len(lines), 13)
        self.assertIn("mcp__x_data_mcp__get_daily_bars", lines[1])
        self.assertNotIn("bash", lines[1].replace("--no-builtin-tools", ""))

    def test_dry_run_in_codemode(self):
        import contextlib
        import io

        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = ev.main(["--scope", "new", "--socket", "/nonexistent", "--output", "/nonexistent/x.json", "--dry-run",
                            "--today", "2026-11-05", "--mode", "codemode"])
        self.assertEqual(code, 0)
        lines = out.getvalue().splitlines()
        self.assertEqual(len(lines), 13)
        command = json.loads(lines[0].split(" ", 1)[1])
        self.assertEqual(command[command.index("--tools") + 1], "codemode")
        self.assertNotIn("--no-builtin-tools", command)

    def test_mode_defaults_to_direct_and_rejects_others(self):
        base = ["--scope", "new", "--socket", "s", "--output", "o"]
        self.assertEqual(ev.parse_args(base).mode, "direct")
        self.assertEqual(ev.parse_args(base + ["--mode", "codemode"]).mode, "codemode")
        with self.assertRaises(SystemExit):
            ev.parse_args(base + ["--mode", "bash"])

    def test_regrade_needs_no_scope_or_socket_and_unknown_scope_still_fails(self):
        args = ev.parse_args(["--regrade", "a.json", "--output", "b.json"])
        self.assertEqual((args.regrade, args.scope, args.socket), ("a.json", None, None))
        with self.assertRaises(SystemExit):
            ev.parse_args(["--output", "b.json"])

    def test_unknown_scope_and_tasks_are_rejected(self):
        self.assertEqual(ev.main(["--scope", "nope", "--socket", "s", "--output", "o"]), 2)
        self.assertEqual(ev.main(["--scope", "new", "--socket", "s", "--output", "o", "--tasks", "q99"]), 2)


if __name__ == "__main__":
    unittest.main()
