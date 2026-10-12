"""Deploy-time guard for the production rebuild drivers (operations tooling, no product code).

Default behaviour is the XP-015 private-socket guard (`xp015_common.private_sock`), unchanged. The
production socket is allowed ONLY when both XP015_PRODUCTION_SOCKET and XP015_SOCKET are exactly
/opt/x-data-mcp-preview/run/mcp.sock - an exact, normalised, symlink-free path (no `..`, no prefix
match, no symlink components). Output goes to a mandatory directory: /root/x-perf-g8-deploy31/out/
(production) or /root/x-perf-g8-xp015/... (private rehearsal).
"""
import os, sys
from xp015_common import private_sock, PRIVATE_ROOT

PROD_SOCKET = "/opt/x-data-mcp-preview/run/mcp.sock"
PROD_OUT_ROOT = "/root/x-perf-g8-deploy31/out/"


def _exact(path: str) -> bool:
    return (path == PROD_SOCKET and os.path.normpath(path) == path and ".." not in path.split("/")
            and os.path.realpath(path) == path)


def socket_path() -> tuple:
    """Returns (path, mode) with mode in {'production', 'private'}; exits on any violation."""
    prod = os.environ.get("XP015_PRODUCTION_SOCKET")
    if prod is None:
        return private_sock(), "private"
    sock = os.environ.get("XP015_SOCKET")
    if prod != PROD_SOCKET or sock != PROD_SOCKET:
        sys.exit(f"refusing production socket: XP015_PRODUCTION_SOCKET={prod!r} XP015_SOCKET={sock!r} must both equal {PROD_SOCKET}")
    if not _exact(PROD_SOCKET):
        sys.exit(f"refusing production socket: {PROD_SOCKET} is not a canonical symlink-free path")
    return PROD_SOCKET, "production"


def out_dir(mode: str, env="XP015_OUT") -> str:
    out = os.environ.get(env)
    if not out:
        sys.exit(f"{env} is required")
    root = PROD_OUT_ROOT if mode == "production" else PRIVATE_ROOT
    real = os.path.realpath(out)
    if ".." in out.split("/") or not out.startswith(root) or not (real + "/").startswith(root):
        sys.exit(f"{env}={out!r} must be under {root}")
    os.makedirs(out, exist_ok=True)
    return out.rstrip("/")
