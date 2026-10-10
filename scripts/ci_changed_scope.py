#!/usr/bin/env python3
"""Decide whether CI must run the Rust steps for a pull request (XP-024).

Prints `rust=true|false` (and appends it to $GITHUB_OUTPUT when set). Rust is
skipped only for pull_request events whose changed files ALL live in
docs/**, root-level *.md, or evidence/**/*.json|*.md. Every other case —
any other path, an empty/unknown change list, a non-PR event — is `true`.
"""
import argparse
import os
import subprocess
import sys


def docs_only_path(path: str) -> bool:
    if not path or path.startswith("/") or ".." in path.split("/"):
        return False
    if path.startswith("docs/"):
        return True
    if "/" not in path:
        return path.endswith(".md")
    return path.startswith("evidence/") and path.endswith((".json", ".md"))


def rust_needed(event: str, paths: list[str]) -> bool:
    if event != "pull_request":
        return True
    paths = [p for p in paths if p]
    if not paths:
        return True
    return not all(docs_only_path(p) for p in paths)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--event", required=True)
    parser.add_argument("--base")
    parser.add_argument("--head")
    args = parser.parse_args()
    paths: list[str] = []
    if args.event == "pull_request":
        if not (args.base and args.head):
            print("pull_request requires --base and --head", file=sys.stderr)
            return 2
        out = subprocess.run(
            ["git", "diff", "--name-only", "-z", f"{args.base}...{args.head}"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        paths = out.split("\0")
    line = f"rust={'true' if rust_needed(args.event, paths) else 'false'}"
    print(line)
    target = os.environ.get("GITHUB_OUTPUT")
    if target:
        with open(target, "a", encoding="utf-8") as f:
            f.write(line + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
