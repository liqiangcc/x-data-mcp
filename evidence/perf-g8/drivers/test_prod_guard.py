import os, subprocess, sys, unittest
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import prod_guard
P = prod_guard.PROD_SOCKET
def run(env):
    code = "import sys;sys.path.insert(0,%r);import prod_guard;print(prod_guard.socket_path())" % os.path.dirname(os.path.abspath(__file__))
    e = {k: v for k, v in os.environ.items() if not k.startswith("XP015_")}; e.update(env)
    return subprocess.run([sys.executable, "-I", "-c", code], env=e, capture_output=True, text=True)
class Guard(unittest.TestCase):
    def test_private_default_unchanged(self):
        self.assertNotEqual(run({"XP015_SOCKET": "/opt/x-data-mcp-preview/run/mcp.sock"}).returncode, 0)  # production path alone is refused
        r = run({"XP015_SOCKET": "/root/x-perf-g8-xp015/final/run/mcp.sock"}); self.assertEqual(r.returncode, 0, r.stderr)
    def test_production_needs_both_exact(self):
        self.assertEqual(run({"XP015_PRODUCTION_SOCKET": P, "XP015_SOCKET": P}).returncode, 0)
        for bad in (P + "/", P + "x", "/opt/x-data-mcp-preview/run/../run/mcp.sock", "/opt/x-data-mcp-preview/run/./mcp.sock",
                    "/opt/x-data-mcp-preview/current/run/mcp.sock", "/opt/x-data-mcp-preview/run/acquire.sock", "/opt//x-data-mcp-preview/run/mcp.sock"):
            self.assertNotEqual(run({"XP015_PRODUCTION_SOCKET": bad, "XP015_SOCKET": bad}).returncode, 0, bad)
            self.assertNotEqual(run({"XP015_PRODUCTION_SOCKET": P, "XP015_SOCKET": bad}).returncode, 0, bad)
            self.assertNotEqual(run({"XP015_PRODUCTION_SOCKET": bad, "XP015_SOCKET": P}).returncode, 0, bad)
        self.assertNotEqual(run({"XP015_PRODUCTION_SOCKET": P}).returncode, 0)  # XP015_SOCKET missing
    def test_out_dir(self):
        os.environ["XP015_OUT"] = "/root/x-perf-g8-deploy31/out/../x"
        with self.assertRaises(SystemExit): prod_guard.out_dir("production")
        os.environ["XP015_OUT"] = "/root/x-perf-g8-xp015/final/out"
        with self.assertRaises(SystemExit): prod_guard.out_dir("production")  # private dir refused for production
        os.environ["XP015_OUT"] = "/root/x-perf-g8-deploy31/out2"
        with self.assertRaises(SystemExit): prod_guard.out_dir("production")  # prefix match refused
        os.environ.pop("XP015_OUT")
        with self.assertRaises(SystemExit): prod_guard.out_dir("private")
if __name__ == "__main__": unittest.main()
