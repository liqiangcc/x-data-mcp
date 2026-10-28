"""Unit tests for scripts/data_unit_layout.py (v8 phase 2 PR-3).

The data unit's ExecStart is the deploy scripts' only source for the database,
the sockets and the trailing options. These tests pin both unit shapes:

* the old single socket `<db> <sock> [--entry ops]`, and
* the phase-2 multi socket `<db> <sock> --entry k <sock> --entry k ... [tail]`,
  including the production `--poll-ms 1000` drop-in tail, which must never be
  dropped by parsing or by re-rendering the target unit body.
"""
import subprocess
import sys
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "data_unit_layout.py"
sys.path.insert(0, str(SCRIPT.parent))
import data_unit_layout as layout  # noqa: E402

BIN = "/opt/x/current/bin/data_service"
DB = "/opt/x/state/preview.sqlite"
RUN = "/opt/x/run"
OLD = f"{BIN} {DB} {RUN}/mcp.sock"
THREE = (
    f"{BIN} {DB} {RUN}/mcp.sock --entry ops "
    f"{RUN}/research.sock --entry research "
    f"{RUN}/maint.sock --entry maint"
)
POLL = "--poll-ms 1000"


def run(*args):
    return subprocess.run(
        [sys.executable, "-B", str(SCRIPT), *args],
        capture_output=True,
        text=True,
        timeout=60,
    )


class ParseOldShape(unittest.TestCase):
    def test_plain_single_socket_defaults_to_ops(self):
        got = layout.parse(OLD)
        self.assertEqual(got["db"], DB)
        self.assertEqual(got["endpoints"], [{"entry": "ops", "socket": f"{RUN}/mcp.sock"}])
        self.assertEqual(got["tail"], [])

    def test_explicit_single_entry_and_poll_tail(self):
        got = layout.parse(f"{OLD} --entry ops {POLL}")
        self.assertEqual(got["endpoints"][0]["entry"], "ops")
        self.assertEqual(got["tail"], ["--poll-ms", "1000"])


class ParseMultiSocket(unittest.TestCase):
    def test_three_sockets_with_tail_preserved(self):
        got = layout.parse(f"{THREE} {POLL}")
        self.assertEqual(
            got["endpoints"],
            [
                {"entry": "ops", "socket": f"{RUN}/mcp.sock"},
                {"entry": "research", "socket": f"{RUN}/research.sock"},
                {"entry": "maint", "socket": f"{RUN}/maint.sock"},
            ],
        )
        # Tail options stay in argument order, after every socket.
        self.assertEqual(got["tail"], ["--poll-ms", "1000"])

    def test_entry_applies_to_the_socket_before_it(self):
        got = layout.parse(f"{BIN} {DB} {RUN}/a.sock --entry research {RUN}/b.sock")
        self.assertEqual([e["entry"] for e in got["endpoints"]], ["research", "ops"])

    def test_leading_entry_sets_pending_default_for_later_sockets(self):
        # Mirrors data_service.rs: a leading --entry sets `pending`, which is
        # the default for every following socket until an --entry edits the
        # socket before it.
        got = layout.parse(f"{BIN} {DB} --entry research {RUN}/a.sock {RUN}/b.sock")
        self.assertEqual([e["entry"] for e in got["endpoints"]], ["research", "research"])

    def test_rejects_bad_layouts(self):
        for line in (
            "",
            BIN,
            f"{BIN} {DB}",
            f"{BIN} {DB} {RUN}/a.sock --entry",
            f"{BIN} {DB} {RUN}/a.sock --entry devops",
            f"{BIN} {DB} {RUN}/a.sock {RUN}/a.sock",
            f"{BIN} {DB} {RUN}/a.sock {POLL.replace(' 1000', '')}",
            "data_service 'unterminated",
        ):
            with self.subTest(line=line):
                with self.assertRaises(layout.LayoutError):
                    layout.parse(line)


class Render(unittest.TestCase):
    def test_round_trip_keeps_sockets_entries_and_tail(self):
        got = layout.parse(f"{THREE} {POLL}")
        line = layout.render(
            got["db"],
            [(e["entry"], e["socket"]) for e in got["endpoints"]],
            got["tail"],
            got["binary"],
        )
        self.assertIn(POLL, line)
        again = layout.parse(line)
        self.assertEqual(again["endpoints"], got["endpoints"])
        self.assertEqual(again["tail"], got["tail"])

    def test_render_rejects_duplicates_and_empty_input(self):
        with self.assertRaises(layout.LayoutError):
            layout.render(DB, [], [])
        with self.assertRaises(layout.LayoutError):
            layout.render(DB, [("ops", "/a.sock"), ("ops", "/a.sock")], [])
        with self.assertRaises(layout.LayoutError):
            layout.render(DB, [("devops", "/a.sock")], [])


class TargetRender(unittest.TestCase):
    """The target body keeps installed sockets/tail and adds missing entries."""

    def test_old_shape_becomes_three_sockets_with_the_same_tail(self):
        parsed = layout.parse(f"{OLD} {POLL}")
        line = layout.target_exec_start(parsed)
        self.assertIn(f"{RUN}/mcp.sock --entry ops", line)
        self.assertIn(f"{RUN}/research.sock --entry research", line)
        self.assertIn(f"{RUN}/maint.sock --entry maint", line)
        self.assertTrue(line.endswith(POLL), line)
        self.assertEqual(layout.parse(line)["tail"], ["--poll-ms", "1000"])

    def test_existing_multi_socket_shape_is_not_renamed(self):
        parsed = layout.parse(
            f"{BIN} {DB} {RUN}/orchestrate.sock --entry ops "
            f"{RUN}/study.sock --entry research --poll-ms 250"
        )
        line = layout.target_exec_start(parsed)
        self.assertIn(f"{RUN}/orchestrate.sock --entry ops", line)
        self.assertIn(f"{RUN}/study.sock --entry research", line)
        self.assertIn(f"{RUN}/maint.sock --entry maint", line)
        self.assertTrue(line.endswith("--poll-ms 250"), line)

    def test_target_keeps_the_checklist_order(self):
        parsed = layout.parse(f"{BIN} {DB} {RUN}/maint.sock --entry maint {RUN}/mcp.sock --entry ops")
        line = layout.target_exec_start(parsed)
        self.assertLess(line.index("--entry ops"), line.index("--entry research"))
        self.assertLess(line.index("--entry research"), line.index("--entry maint"))


class Cli(unittest.TestCase):
    def test_sh_format_evaluates_to_the_parsed_layout(self):
        proc = run("--exec-start", f"{THREE} {POLL}", "--format", "sh")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        check = subprocess.run(
            [
                "bash",
                "-c",
                'eval "$LAYOUT"; '
                'printf "%s\\n" "$DATA_UNIT_DB" "${DATA_UNIT_ENTRIES[*]}" '
                '"${DATA_UNIT_SOCKETS[*]}" "${DATA_UNIT_TAIL[*]}"',
            ],
            env={"LAYOUT": proc.stdout, "PATH": "/usr/bin:/bin"},
            capture_output=True,
            text=True,
        )
        self.assertEqual(check.returncode, 0, check.stderr)
        self.assertEqual(
            check.stdout.splitlines(),
            [DB, "ops research maint", f"{RUN}/mcp.sock {RUN}/research.sock {RUN}/maint.sock",
             "--poll-ms 1000"],
        )

    def test_render_mode_prints_the_exec_start(self):
        proc = run(
            "--render", "--binary", BIN, "--db", DB,
            "--endpoint", "ops=/opt/x/run/mcp.sock",
            "--endpoint", "research=/opt/x/run/research.sock",
            "--endpoint", "maint=/opt/x/run/maint.sock",
            "--tail", POLL,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        line = proc.stdout.strip()
        self.assertTrue(line.startswith(f"{BIN} {DB} "))
        self.assertIn("--entry research", line)
        self.assertTrue(line.endswith(POLL), line)
        self.assertEqual(layout.parse(line)["tail"], ["--poll-ms", "1000"])

    def test_target_mode_prints_one_line_that_reparses(self):
        proc = run("--exec-start", f"{OLD} {POLL}", "--target")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        lines = proc.stdout.strip().splitlines()
        self.assertEqual(len(lines), 1, proc.stdout)
        self.assertTrue(lines[0].endswith(POLL), lines[0])
        self.assertIn("--entry research", lines[0])
        self.assertIn("--entry maint", lines[0])

    def test_bad_input_exits_2_with_a_message(self):
        for args in (
            ("--exec-start", f"{BIN} {DB}"),
            ("--exec-start", f"{BIN} {DB} /a.sock --entry nope"),
            ("--render", "--db", DB, "--endpoint", "nope=/a.sock"),
            ("--render", "--db", DB),
        ):
            with self.subTest(args=args):
                proc = run(*args)
                self.assertEqual(proc.returncode, 2, proc.stdout)
                self.assertIn("data_unit_layout:", proc.stderr)


if __name__ == "__main__":
    unittest.main()
