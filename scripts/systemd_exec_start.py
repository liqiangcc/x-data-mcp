#!/usr/bin/env python3
"""Resolve the *effective* ExecStart of a systemd unit.

The deploy scripts used to read the first `ExecStart=` line out of
`systemctl cat`. That is the base unit, not what systemd runs: production
keeps the three-socket data layout in the `exec-start.conf` drop-in, which
also starts with an empty `ExecStart=` reset:

    # base unit
    ExecStart=<db> run/mcp.sock
    # exec-start.conf
    ExecStart=
    ExecStart=<db> run/research.sock --entry research run/mcp.sock --entry ops ...

so every socket except `ops` was invisible to the readiness probe (XDM-160,
observed on preview.41).

`systemctl show -p ExecStart --value` reports the post-drop-in, post-reset
exec vector, which is the ground truth. Its value is the `{ path=... ;
argv[]=... ; ... }` dump, and systemd C-escapes each argument (a space
becomes `\\x20`), so the vector is split and unescaped here before it is
re-quoted for the shell.

`systemctl cat` only concatenates the snippets, so the other mode replays
systemd's single-ExecStart rule: a later non-empty `ExecStart=` replaces the
earlier one and an empty `ExecStart=` clears it. It is the fallback for hosts
where `show` is unavailable (and the mode the tests can drive with fixtures).

Usage:
    systemd_exec_start.py --unit <unit>       # systemctl show, then cat
    systemd_exec_start.py --show-value TEXT   # a `show -p ExecStart --value`
    systemd_exec_start.py --cat-file PATH     # a `systemctl cat` output
    systemd_exec_start.py --cat-stdin

Prints the effective ExecStart as one shell-quoted line. Exit 0 on success,
1 when the unit has no effective ExecStart (missing/masked/reset), 2 when the
input cannot be parsed.
"""
import argparse
import re
import shlex
import subprocess
import sys

EXEC_START = re.compile(r"^\s*ExecStart\s*=\s*(.*)$")
# The argv[] dump ends at the next " ; " field separator; a literal " ; "
# cannot occur inside argv because systemd escapes ';' and ' ' as \x3b/\x20.
ARGV_END = " ; "
CONTINUATION = re.compile(r"[ \t]*\\\n[ \t]*")
SYSTEMCTL_TIMEOUT = 15


class ExecStartError(ValueError):
    """No usable ExecStart could be extracted from the input."""


_ESCAPES = {
    "a": "\a",
    "b": "\b",
    "f": "\f",
    "n": "\n",
    "r": "\r",
    "t": "\t",
    "v": "\v",
    "\\": "\\",
    '"': '"',
    "'": "'",
    "s": " ",
}
_HEX = set("0123456789abcdefABCDEF")


def _cunescape(text):
    """Undo systemd's C-style escaping of one argv element."""
    out = []
    i = 0
    while i < len(text):
        ch = text[i]
        if ch != "\\":
            out.append(ch)
            i += 1
            continue
        if i + 1 >= len(text):
            raise ExecStartError("argument ends with a lone backslash")
        nxt = text[i + 1]
        if nxt == "x":
            digits = text[i + 2 : i + 4]
            if len(digits) != 2 or not set(digits) <= _HEX:
                raise ExecStartError("bad \\x escape in argv")
            out.append(chr(int(digits, 16)))
            i += 4
            continue
        if nxt in _ESCAPES:
            out.append(_ESCAPES[nxt])
            i += 2
            continue
        raise ExecStartError("unknown escape \\%s in argv" % nxt)
    return "".join(out)


def _split_argv(text):
    """Split the `show` argv dump into arguments, honouring escapes."""
    tokens = []
    current = []
    i = 0
    while i < len(text):
        ch = text[i]
        if ch == "\\":
            length = 4 if text[i + 1 : i + 2] == "x" else 2
            if i + length > len(text):
                raise ExecStartError("truncated escape in argv")
            current.append(text[i : i + length])
            i += length
            continue
        if ch == " ":
            if current:
                tokens.append("".join(current))
                current = []
            i += 1
            continue
        current.append(ch)
        i += 1
    if current:
        tokens.append("".join(current))
    return [_cunescape(token) for token in tokens]


def from_show_value(value):
    """Parse `systemctl show -p ExecStart --value` (with or without a header)."""
    text = value.strip()
    if text.startswith("ExecStart="):
        text = text[len("ExecStart=") :].strip()
    start = text.find("argv[]=")
    if start < 0:
        raise ExecStartError("systemctl show output carries no argv[]")
    rest = text[start + len("argv[]=") :]
    end = rest.find(ARGV_END)
    if end >= 0:
        rest = rest[:end]
    argv = _split_argv(rest.strip())
    if not argv:
        raise ExecStartError("systemctl show reports an empty argv[]")
    return shlex.join(argv)


def from_cat(text):
    """Resolve the effective ExecStart from a `systemctl cat` concatenation."""
    joined = CONTINUATION.sub(" ", text)
    current = None
    for line in joined.splitlines():
        match = EXEC_START.match(line)
        if not match:
            continue
        value = match.group(1).strip()
        # systemd: a later non-empty assignment replaces the earlier one and
        # an empty assignment resets the list.
        current = value or None
    if current is None:
        raise ExecStartError("unit has no effective ExecStart")
    return current


def _systemctl(*args):
    try:
        return subprocess.run(
            ["systemctl", *args],
            capture_output=True,
            text=True,
            timeout=SYSTEMCTL_TIMEOUT,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise ExecStartError("cannot run systemctl: %s" % exc)


def from_unit(unit):
    """The effective ExecStart of an installed unit."""
    shown = _systemctl("show", unit, "-p", "ExecStart", "--value")
    if shown.returncode == 0 and shown.stdout.strip():
        try:
            return from_show_value(shown.stdout)
        except ExecStartError:
            pass  # fall through to `cat`
    # `|| true` semantics wanted by the callers: a missing unit is "unreadable".
    cat = _systemctl("cat", unit)
    if cat.returncode != 0 or not cat.stdout.strip():
        raise ExecStartError("cannot read unit %s" % unit)
    return from_cat(cat.stdout)


def main(argv=None):
    parser = argparse.ArgumentParser(
        prog="systemd_exec_start.py",
        description="Resolve the effective ExecStart of a systemd unit.",
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--unit", metavar="UNIT", help="installed unit name")
    mode.add_argument("--show-value", metavar="TEXT", help="a systemctl show value")
    mode.add_argument("--cat-file", metavar="PATH", help="a systemctl cat dump")
    mode.add_argument(
        "--cat-stdin", action="store_true", help="read a systemctl cat dump from stdin"
    )
    args = parser.parse_args(argv)

    try:
        if args.unit:
            print(from_unit(args.unit))
        elif args.show_value:
            print(from_show_value(args.show_value))
        elif args.cat_file:
            print(from_cat(open(args.cat_file, encoding="utf-8").read()))
        else:
            print(from_cat(sys.stdin.read()))
    except ExecStartError as exc:
        sys.stderr.write("systemd_exec_start: %s\n" % exc)
        return 2
    except OSError as exc:
        sys.stderr.write("systemd_exec_start: %s\n" % exc)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
