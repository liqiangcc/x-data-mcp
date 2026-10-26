#!/usr/bin/env python3
"""Token-budget assertions for the v8 read-surface fixtures (#387 §8.4).

Each fixture is a synthetic snapshot plus the expected v2 response. The
response is measured exactly as the model sees it: compact JSON, ASCII
characters at 4 per token and non-ASCII at 1.5 characters per token (the
reading-mcp convention recorded in the design). Every file under
``fixtures/v8/`` must be listed with a budget, so a new fixture cannot land
without one.
"""
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BASELINE = ROOT / ".devin/evals/v8-baseline.json"
FIXTURES = ROOT / "fixtures/v8"


def canonical(value) -> str:
    """The same compact JSON serde_json writes (sorted keys, no spaces)."""
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def token_count(text: str, ascii_per_token: float, non_ascii_per_token: float) -> float:
    ascii_chars = sum(1 for char in text if ord(char) < 128)
    non_ascii = len(text) - ascii_chars
    return ascii_chars / ascii_per_token + non_ascii / non_ascii_per_token


def fixture_files(root: Path = ROOT) -> set[str]:
    directory = root / "fixtures/v8"
    if not directory.is_dir():
        return set()
    return {
        path.relative_to(root).as_posix()
        for path in sorted(directory.iterdir())
        if path.is_file() and path.suffix == ".json"
    }


def check(baseline_path: Path = BASELINE, root: Path = ROOT):
    """(failures, measured tokens per task)."""
    baseline = json.loads(baseline_path.read_text(encoding="utf-8"))
    tokenizer = baseline["tokenizer"]
    ascii_per_token = float(tokenizer["ascii_chars_per_token"])
    non_ascii_per_token = float(tokenizer["non_ascii_chars_per_token"])
    failures = []
    measured = {}
    listed = set()
    for task in baseline["tasks"]:
        listed.add(task["fixture"])
        fixture = root / task["fixture"]
        if not fixture.is_file():
            failures.append(f"{task['id']}: missing fixture {task['fixture']}")
            continue
        response = json.loads(fixture.read_text(encoding="utf-8"))
        if "expected" not in response:
            failures.append(f"{task['id']}: fixture has no expected response")
            continue
        count = token_count(
            canonical(response["expected"]), ascii_per_token, non_ascii_per_token
        )
        measured[task["id"]] = count
        if count > task["budget_tokens"]:
            failures.append(
                f"{task['id']}: {count:.1f} tokens > budget "
                f"{task['budget_tokens']} ({task['fixture']})"
            )
    for missing in sorted(fixture_files(root) - listed):
        failures.append(f"{missing}: fixture has no token budget")
    return failures, measured


def main() -> int:
    failures, measured = check()
    for task_id, count in measured.items():
        print(f"{task_id}: {count:.1f} tokens")
    for failure in failures:
        print(f"FAIL {failure}")
    print("v8-token-budget OK" if not failures else "v8-token-budget FAILED")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
