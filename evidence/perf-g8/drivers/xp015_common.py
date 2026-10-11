"""XP-015 measurement-tool helpers (no product code). Shared by the perf-g8 drivers.

* `private_sock()` - the MCP socket MUST come from XP015_SOCKET (no default) and must live under
  /root/x-perf-g8-xp015/; the historical drivers hard-coded the frozen-DB stack's socket.
* `POLL_S` - result polling interval (0.25 s, recorded in every driver output).
* `classify_range_results()` - per-item outcome from a get_task_progress page: `no_change` and
  `published` come from RangeExecutionResult.no_change; failed from the task counters; whatever cannot
  be attributed stays `unknown` (never folded into success).
"""
import os, sys

PRIVATE_ROOT = "/root/x-perf-g8-xp015/"
POLL_S = 0.25


def private_sock() -> str:
    path = os.environ.get("XP015_SOCKET")
    if not path:
        sys.exit("XP015_SOCKET is required (no default; the old drivers pointed at the frozen stack)")
    real = os.path.realpath(os.path.dirname(path)) + "/" + os.path.basename(path)
    if not (real.startswith(PRIVATE_ROOT) and path.startswith(PRIVATE_ROOT)):
        sys.exit(f"refusing socket {path!r}: must be under {PRIVATE_ROOT}")
    return path


def classify_range_results(progress: dict) -> dict:
    """Counts for one finished task: considered/published/no_change/failed/needs_verification/unknown."""
    total = progress.get("total") or 0
    results = progress.get("range_results") or []
    no_change = sum(1 for r in results if r.get("no_change") is True)
    published = sum(1 for r in results if r.get("no_change") is False)
    failed = progress.get("failed") or 0
    status = (progress.get("task") or {}).get("status")
    needs_verification = total if status == "needs_verification" else 0
    unknown = max(0, total - published - no_change - failed - needs_verification)
    return {"considered": total, "published": published, "no_change": no_change, "failed": failed,
            "needs_verification": needs_verification, "unknown": unknown,
            "range_results_seen": len(results)}
