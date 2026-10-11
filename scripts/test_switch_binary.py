"""CI-runnable checks for scripts/switch_binary.sh (TASK-XP-026 S4).

Covers `bash -n` syntax and dry-run behaviour on a simulated deploy root:
a tempdir with releases/<prev> and a `current` symlink. Dry-run must print
the plan, mutate nothing, and never reach /opt or any real service.
"""
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SCRIPT = REPO / "scripts" / "switch_binary.sh"
TAG = "v0.2.0-preview.30"
SHA = "1" * 40
PREV = "0" * 40


def deploy_root(tmp: Path) -> Path:
    root = tmp / "preview"
    (root / "releases" / PREV / "bin").mkdir(parents=True)
    (root / "current").symlink_to(root / "releases" / PREV)
    return root


def run_script(root: Path, *args: str) -> subprocess.CompletedProcess:
    env = dict(os.environ, XDM_DEPLOY_ROOT=str(root))
    return subprocess.run(
        ["bash", str(SCRIPT), *args], env=env, capture_output=True, text=True
    )


class SwitchBinary(unittest.TestCase):
    def test_bash_syntax(self):
        self.assertEqual(
            subprocess.run(["bash", "-n", str(SCRIPT)]).returncode, 0
        )

    def test_dry_run_prints_plan_and_mutates_nothing(self):
        with tempfile.TemporaryDirectory() as td:
            root = deploy_root(Path(td))
            proc = run_script(root, TAG, SHA)
            self.assertEqual(proc.returncode, 0, proc.stderr + proc.stdout)
            out = proc.stdout
            for marker in (
                "DRY: gh release download",
                "DRY: systemctl stop x-data-mcp-acquire",
                "DRY: systemctl stop x-data-mcp-data",
                "DRY: ln -sfn",
                "DRY: mv -T",
                "DRY: systemctl start",
            ):
                self.assertIn(marker, out)
            self.assertIn("ROLLBACK", out)
            self.assertEqual((root / "current").readlink(), root / "releases" / PREV)
            self.assertFalse((root / "current.new").exists())
            self.assertFalse((root / "current.rb").exists())
            self.assertFalse((root / "releases" / SHA).exists())

    def test_dry_run_is_default(self):
        with tempfile.TemporaryDirectory() as td:
            root = deploy_root(Path(td))
            proc = run_script(root, TAG, SHA)
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertNotIn("+ systemctl", proc.stdout)

    def test_bad_args_rejected(self):
        with tempfile.TemporaryDirectory() as td:
            root = deploy_root(Path(td))
            for args in [
                ("not-a-tag", SHA),
                (TAG, "deadbeef"),
                ("v0.2.0-preview.30x", SHA),
            ]:
                proc = run_script(root, *args)
                self.assertEqual(proc.returncode, 2, args)

    def test_missing_previous_release_dir_fails(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td) / "preview"
            (root / "releases").mkdir(parents=True)
            (root / "current").symlink_to(root / "releases" / "gone")
            proc = run_script(root, TAG, SHA)
            self.assertEqual(proc.returncode, 1)
            self.assertIn("previous release dir missing", proc.stderr)


if __name__ == "__main__":
    unittest.main()
