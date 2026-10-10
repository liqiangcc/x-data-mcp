"""Unittest coverage for scripts/check_deploy_facts.py (TASK-XP-026 S5)."""
import datetime
import tempfile
import unittest
from pathlib import Path

from check_deploy_facts import (
    check_current_host,
    check_status_md,
    top_current_region,
)

TODAY = datetime.date(2026, 10, 10)


def status_file(tmp: Path, text: str) -> Path:
    path = tmp / "STATUS.md"
    path.write_text(text, encoding="utf-8")
    return path


class StatusMd(unittest.TestCase):
    def test_missing_file_warns_not_fails(self):
        errors, warnings = check_status_md(Path("/nonexistent/STATUS.md"), TODAY)
        self.assertEqual(errors, [])
        self.assertEqual(len(warnings), 1)

    def test_present_without_date_fails(self):
        with tempfile.TemporaryDirectory() as td:
            path = status_file(
                Path(td), "# status\ndeployment version: preview.29\n"
            )
            errors, _ = check_status_md(path, TODAY)
            self.assertTrue(any("no '最后核实'" in e or "no '" in e for e in errors))

    def test_fresh_date_passes(self):
        with tempfile.TemporaryDirectory() as td:
            path = status_file(
                Path(td),
                "# status\n最后核实: 2026-10-09\ndeployment version: preview.29\n",
            )
            errors, warnings = check_status_md(path, TODAY)
            self.assertEqual(errors, [])
            self.assertEqual(warnings, [])

    def test_stale_date_warns(self):
        with tempfile.TemporaryDirectory() as td:
            path = status_file(
                Path(td),
                "last verified 2026-09-01\ndeployment version: v0.2.0-preview.29\n",
            )
            errors, warnings = check_status_md(path, TODAY)
            self.assertEqual(errors, [])
            self.assertTrue(any("days old" in w for w in warnings))

    def test_future_date_fails(self):
        with tempfile.TemporaryDirectory() as td:
            path = status_file(
                Path(td), "最后核实 2027-01-01\nversion: preview.29\n"
            )
            errors, _ = check_status_md(path, TODAY)
            self.assertTrue(any("future" in e for e in errors))

    def test_malformed_version_fails(self):
        with tempfile.TemporaryDirectory() as td:
            path = status_file(
                Path(td),
                "最后核实 2026-10-09\ndeployment version: preview.29-rc!!\n",
            )
            errors, _ = check_status_md(path, TODAY)
            self.assertTrue(any("malformed" in e for e in errors))

    def test_no_version_fails(self):
        with tempfile.TemporaryDirectory() as td:
            path = status_file(Path(td), "最后核实 2026-10-09\n")
            errors, _ = check_status_md(path, TODAY)
            self.assertTrue(any("no deployment version" in e for e in errors))


class CurrentHost(unittest.TestCase):
    def test_ecs_node_as_current_host_rejected(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / "README.md").write_text(
                "intro\n## 当前状态\ncurrent host: ecs-node\n## next\n", encoding="utf-8"
            )
            errors = check_current_host(root)
            self.assertTrue(any("README.md" in e for e in errors))

    def test_ecs_node_as_measurement_env_allowed(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / "README.md").write_text(
                "## 当前状态\n测量环境 ecs-node x86_64、2 vCPU\n", encoding="utf-8"
            )
            self.assertEqual(check_current_host(root), [])

    def test_ecs_node_outside_current_region_allowed(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / "README.md").write_text(
                "## 当前状态\nhost: web\n## 历史\ncurrent host: ecs-node\n",
                encoding="utf-8",
            )
            self.assertEqual(check_current_host(root), [])


class Regions(unittest.TestCase):
    def test_region_stops_at_next_heading(self):
        region = top_current_region("a\n## 当前状态\nmid\n## end\nafter", "## 当前状态")
        self.assertIn("mid", region)
        self.assertNotIn("after", region)

    def test_none_heading_uses_prefix(self):
        self.assertEqual(top_current_region("pre\n## h\nrest", None), "pre\n")


if __name__ == "__main__":
    unittest.main()
