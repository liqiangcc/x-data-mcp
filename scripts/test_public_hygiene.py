import unittest

from check_public_hygiene import in_scope, scan_text


class Rules(unittest.TestCase):
    def test_personal_email_rejected(self):
        self.assertEqual(len(scan_text("a.md", "contact me at someone@gmail.com")), 1)

    def test_noreply_and_transport_addresses_allowed(self):
        text = (
            "Co-Authored-By: Claude <noreply@anthropic.com>\n"
            "git@github.com:org/repo.git\n"
            "12345+user@users.noreply.github.com\n"
        )
        self.assertEqual(scan_text("a.md", text), [])

    def test_url_userinfo_and_version_pins_are_not_emails(self):
        self.assertEqual(scan_text("a.rs", 'let u = "https://evil@host.example.cn/x";'), [])
        self.assertEqual(scan_text("ci.yml", "tool: nextest@0.9.146"), [])

    def test_internal_hostnames_rejected(self):
        for line in (
            "host ip-172-31-10-15 up",
            "box.us-west-1.compute.internal",
            "node.tailabc123.ts.net",
        ):
            self.assertEqual(len(scan_text("a.md", line)), 1, line)

    def test_private_addresses_rejected_public_and_version_like_allowed(self):
        for line in ("10.0.0.5", "172.16.4.1", "192.168.1.9", "100.64.0.7", "100.127.1.1"):
            self.assertEqual(len(scan_text("a.md", line)), 1, line)
        for line in ("8.8.8.8", "172.32.0.1", "100.128.0.1", "v1.10.0.5-rc", "127.0.0.1"):
            self.assertEqual(scan_text("a.md", line), [], line)

    def test_scope_skips_captured_evidence_and_lockfiles(self):
        self.assertFalse(in_scope("evidence/perf-g8/env.json"))
        self.assertFalse(in_scope("fixtures/x.txt"))
        self.assertFalse(in_scope("Cargo.lock"))
        self.assertTrue(in_scope("docs/STATUS.md"))

    def test_scope_skips_the_gate_and_its_own_test_data(self):
        self.assertFalse(in_scope("scripts/check_public_hygiene.py"))
        self.assertFalse(in_scope("scripts/test_public_hygiene.py"))


if __name__ == "__main__":
    unittest.main()
