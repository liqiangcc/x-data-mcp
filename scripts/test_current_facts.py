"""Unit tests for the stale-deployment-claim rule of check_current_facts.py."""
import importlib.util
import subprocess
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location(
    "check_current_facts", SCRIPTS / "check_current_facts.py"
)
cf = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cf)

ALLOWED = {"31"}


def claims(text: str) -> list[str]:
    return [
        kind
        for _, kind, _ in cf.stale_deployment_claims(
            text, ALLOWED, 21, "generic-data-v5"
        )
    ]


class StaleDeploymentClaims(unittest.TestCase):
    def test_current_facts_pass(self):
        text = (
            "本机已部署 v0.2.0-preview.31；data server 21 data tools，"
            "capability revision `generic-data-v5`，10 acquisition tools。\n"
        )
        self.assertEqual(claims(text), [])

    def test_v6_claim_is_flagged_while_deployed_revision_is_v5(self):
        # The deployed instance still declares v5 until the deploy-day
        # switch; a current region naming the code revision (v6) is stale.
        text = "当前部署注册 21 data tools 并宣告 `generic-data-v6`。\n"
        found = claims(text)
        self.assertEqual(len(found), 1, found)
        self.assertIn("generic-data-v6", found[0])

    def test_the_old_sentences_are_flagged(self):
        # The wording that main@9189b68 still carried after the preview.31 deploy.
        old = (
            "当前部署 preview.29（`4493d45`）：data socket 注册 20 data tools，\n"
            "当前开发分支（XP-014，未部署）注册 21 data tools 并宣告 `generic-data-v5`；\n"
            "preview.29 部署实例仍为 20 data tools / `generic-data-v3`。\n"
        )
        found = claims(old)
        self.assertEqual(sum("preview.29" in k for k in found), 2, found)
        self.assertEqual(sum("20 data tools" in k for k in found), 2, found)
        self.assertEqual(sum("generic-data-v3" in k for k in found), 1, found)

    def test_chinese_count_form_is_flagged(self):
        self.assertEqual(len(claims("共 20 个 data tools。\n")), 1)
        self.assertEqual(claims("共 21 个 data 工具。\n"), [])

    def test_historical_paragraph_is_exempt(self):
        text = "历史记录（preview.29 时期）：20 data tools / generic-data-v3。\n"
        self.assertEqual(claims(text), [])
        text = "Historical note: preview.29 had 20 data tools.\n"
        self.assertEqual(claims(text), [])

    def test_marker_exempts_the_whole_paragraph_including_list_items(self):
        text = (
            "- 历史实测：preview.27 的数字\n"
            "- preview.28 的另一条（同一段落）\n"
            "\n"
            "preview.28 单独的当前段落。\n"
        )
        self.assertEqual(len(claims(text)), 1)

    def test_historical_heading_exempts_until_same_level_heading(self):
        text = (
            "## 当前\n\npreview.28 在当前段落。\n\n"
            "### 历史沿革\n\npreview.27 / 20 data tools\n\n#### 子节\n\npreview.26\n\n"
            "### 另一个小节\n\npreview.25\n"
        )
        found = claims(text)
        self.assertEqual(len(found), 2, found)  # preview.28 and preview.25

    def test_deployed_version_comes_from_status_row(self):
        status = "| 部署版本 | v0.2.0-preview.32 @ `abc` | x |\n"
        allowed = cf.deployed_preview_numbers(status, "0.2.0-preview.31")
        self.assertEqual(allowed, {"31", "32"})
        self.assertEqual(
            cf.deployed_preview_numbers(None, "0.2.0-preview.31"), {"31"}
        )

    def test_limitations_documented(self):
        # Known false negative: a stale claim in a paragraph that also holds
        # a historical marker word is not reported. Documented, not hidden.
        text = "历史上有 20 data tools，现在仍为 20 data tools。\n"
        self.assertEqual(claims(text), [])


class RealDocs(unittest.TestCase):
    def test_script_passes_on_the_repository(self):
        result = subprocess.run(
            ["python3", str(SCRIPTS / "check_current_facts.py")],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
