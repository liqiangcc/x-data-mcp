import unittest

from ci_changed_scope import docs_only_path, rust_needed

PR = "pull_request"


class DocsOnlyPath(unittest.TestCase):
    def test_docs_tree(self):
        self.assertTrue(docs_only_path("docs/conventions.md"))
        self.assertTrue(docs_only_path("docs/a/b/c.png"))

    def test_root_markdown_only(self):
        self.assertTrue(docs_only_path("README.md"))
        self.assertFalse(docs_only_path("Cargo.toml"))
        self.assertFalse(docs_only_path("build.rs"))

    def test_evidence_json_and_md_only(self):
        self.assertTrue(docs_only_path("evidence/perf-g8/result.json"))
        self.assertTrue(docs_only_path("evidence/issue-54/NOTES.md"))
        self.assertFalse(docs_only_path("evidence/perf-g8/drivers/yearly_g8.py"))
        self.assertFalse(docs_only_path("evidence/x/run.sh"))
        self.assertFalse(docs_only_path("evidence/data.json.bak"))

    def test_code_and_ci_paths(self):
        for p in [
            ".github/workflows/ci.yml",
            "scripts/ci_changed_scope.py",
            ".devin/skills/x/SKILL.md",
            "src/lib.rs",
            "tests/market/main.rs",
            "Cargo.lock",
            "migrations/tasks/001.sql",
            "subdir/NOTES.md",
        ]:
            self.assertFalse(docs_only_path(p), p)

    def test_prefix_lookalikes_and_traversal(self):
        self.assertFalse(docs_only_path("docs"))
        self.assertFalse(docs_only_path("docsx/a.md"))
        self.assertFalse(docs_only_path("evidencex/a.json"))
        self.assertFalse(docs_only_path("docs/../src/lib.rs"))
        self.assertFalse(docs_only_path("/docs/a.md"))
        self.assertFalse(docs_only_path(""))


class RustNeeded(unittest.TestCase):
    def test_all_docs_skips_rust(self):
        self.assertFalse(rust_needed(PR, ["docs/a.md", "README.md", "evidence/x/y.json"]))

    def test_one_non_doc_path_requires_rust(self):
        self.assertTrue(rust_needed(PR, ["docs/a.md", "src/lib.rs"]))
        self.assertTrue(rust_needed(PR, ["docs/a.md", ".github/workflows/ci.yml"]))

    def test_empty_or_blank_change_list_requires_rust(self):
        self.assertTrue(rust_needed(PR, []))
        self.assertTrue(rust_needed(PR, [""]))

    def test_non_pull_request_events_always_require_rust(self):
        self.assertTrue(rust_needed("push", ["docs/a.md"]))
        self.assertTrue(rust_needed("workflow_dispatch", ["README.md"]))

    def test_trailing_nul_separator_is_ignored(self):
        self.assertFalse(rust_needed(PR, ["docs/a.md", ""]))


if __name__ == "__main__":
    unittest.main()
