#!/usr/bin/env python3
"""Parse and render the `x-data-mcp-data` unit's ExecStart socket layout.

`data_service` (v8 phase 2, D-56) serves one entry per socket in a single
process (`src/bin/data_service.rs`):

    data_service <db> <sock> [--entry research|ops|maint] [<sock> ... ] [--poll-ms N]

`--entry` binds to the socket *before* it; a leading `--entry` (before any
socket) sets the entry of the next socket. The old single-socket shape
`<db> <sock>` keeps working with the default `ops` entry, so the deploy
scripts must accept both shapes and must not drop tail options: production
carries `--poll-ms 1000` in a drop-in (`poll-interval.conf`), appended after
the sockets.

Usage:
    data_unit_layout.py --exec-start "<ExecStart...>" [--format json|sh]
    data_unit_layout.py --exec-start "<ExecStart...>" --target
    data_unit_layout.py --render --db <db> --endpoint <entry>=<socket> [...]
                        [--binary <path>] [--tail "<args...>"]

`--target` renders the phase-2 three-socket ExecStart (checklist §3) from an
installed unit: sockets/entries already present are kept, missing standard
entries are added next to the first socket, and every tail option (`--poll-ms`
in production) is preserved. It prints one line and writes nothing.

`--format sh` prints shell assignments (shlex-quoted) that
`scripts/deploy_phase_g.sh` evals:

    DATA_UNIT_BIN='/opt/.../bin/data_service'
    DATA_UNIT_DB='/opt/.../state/preview.sqlite'
    DATA_UNIT_TAIL=('--poll-ms' '1000')
    DATA_UNIT_ENTRIES=('ops' 'research')
    DATA_UNIT_SOCKETS=('/opt/.../run/mcp.sock' '/opt/.../run/research.sock')

Exit codes: 0 ok, 2 unparsable/unrenderable layout (message on stderr).
"""
import argparse
import json
import shlex
import sys

ENTRIES = ("research", "ops", "maint")
# Checklist §3 order: mcp.sock (ops) stays first as the orchestration entry.
TARGET_ENTRY_ORDER = ("ops", "research", "maint")
DEFAULT_ENTRY = "ops"
# Flags whose next token is a value, not a positional socket. Keeping this
# explicit avoids swallowing a socket path as a flag value.
VALUE_FLAGS = ("--poll-ms",)


class LayoutError(ValueError):
    """The ExecStart line is not a data_service db + sockets layout."""


def parse(exec_start):
    """Parse a data unit ExecStart into {binary, db, endpoints, tail}."""
    try:
        tokens = shlex.split(exec_start)
    except ValueError as e:
        raise LayoutError("cannot tokenize ExecStart: %s" % e)
    if not tokens:
        raise LayoutError("empty ExecStart")
    binary = tokens[0]
    positionals = []
    entries = []
    tail = []
    pending = DEFAULT_ENTRY
    i = 1
    while i < len(tokens):
        token = tokens[i]
        if token == "--entry":
            i += 1
            if i >= len(tokens):
                raise LayoutError("--entry requires research|ops|maint")
            if tokens[i] not in ENTRIES:
                raise LayoutError(
                    "unknown --entry value %r (research|ops|maint)" % tokens[i]
                )
            if entries:
                entries[-1] = tokens[i]
            else:
                pending = tokens[i]
        elif token in VALUE_FLAGS:
            tail.append(token)
            i += 1
            if i >= len(tokens):
                raise LayoutError("%s requires a value" % token)
            tail.append(tokens[i])
        elif token.startswith("-"):
            tail.append(token)
        else:
            if positionals:
                entries.append(pending)
            positionals.append(token)
        i += 1
    if len(positionals) < 2:
        raise LayoutError(
            "ExecStart must carry <db> <socket> [<socket> ...]: %r" % exec_start
        )
    db, sockets = positionals[0], positionals[1:]
    if len(set(sockets)) != len(sockets):
        raise LayoutError("the same socket path appears twice")
    return {
        "binary": binary,
        "db": db,
        "endpoints": [
            {"entry": entry, "socket": socket}
            for entry, socket in zip(entries, sockets)
        ],
        "tail": tail,
    }


def parse_endpoint(spec):
    """`research=run/research.sock` -> (entry, socket)."""
    entry, sep, socket = spec.partition("=")
    if not sep or not entry or not socket:
        raise LayoutError("--endpoint wants <entry>=<socket>, got %r" % spec)
    if entry not in ENTRIES:
        raise LayoutError("unknown entry %r (research|ops|maint)" % entry)
    return entry, socket


def render(db, endpoints, tail, binary="data_service"):
    """Render an ExecStart line, preserving the given tail options verbatim."""
    if not db:
        raise LayoutError("--db is required")
    if not endpoints:
        raise LayoutError("at least one --endpoint is required")
    sockets = [socket for _, socket in endpoints]
    if len(set(sockets)) != len(sockets):
        raise LayoutError("the same socket path appears twice")
    parts = [binary, db]
    for entry, socket in endpoints:
        if entry not in ENTRIES:
            raise LayoutError("unknown entry %r (research|ops|maint)" % entry)
        parts.extend((socket, "--entry", entry))
    parts.extend(tail)
    return " ".join(shlex.quote(part) for part in parts)


def target_exec_start(parsed, entries=TARGET_ENTRY_ORDER):
    """Render the three-socket target for an installed unit, keeping the tail.

    Installed sockets keep their paths and order is the standard entry order
    (ops -> research -> maint); a missing entry is added as `<socket-dir>/
    <entry>.sock`. Rendering the target never drops `--poll-ms` or any other
    parsed trailing option.
    """
    if not parsed["endpoints"]:
        raise LayoutError("installed ExecStart has no socket")
    socket_dir = parsed["endpoints"][0]["socket"].rsplit("/", 1)[0]
    have = {e["entry"]: e["socket"] for e in parsed["endpoints"]}
    endpoints = [
        (entry, have.get(entry, "%s/%s.sock" % (socket_dir, entry)))
        for entry in entries
    ]
    return render(parsed["db"], endpoints, parsed["tail"], parsed["binary"])


def shell_assignments(layout):
    def quote_list(values):
        return "(" + " ".join(shlex.quote(v) for v in values) + ")"

    return "\n".join(
        [
            "DATA_UNIT_BIN=" + shlex.quote(layout["binary"]),
            "DATA_UNIT_DB=" + shlex.quote(layout["db"]),
            "DATA_UNIT_TAIL=" + quote_list(layout["tail"]),
            "DATA_UNIT_ENTRIES="
            + quote_list([e["entry"] for e in layout["endpoints"]]),
            "DATA_UNIT_SOCKETS="
            + quote_list([e["socket"] for e in layout["endpoints"]]),
        ]
    ) + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(
        prog="data_unit_layout.py",
        description="Parse or render the data unit ExecStart db + socket layout.",
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--exec-start", metavar="LINE", help="a unit ExecStart value")
    mode.add_argument("--render", action="store_true", help="render an ExecStart")
    parser.add_argument("--format", choices=("json", "sh"), default="json")
    parser.add_argument("--db", default=None, help="--render: database path")
    parser.add_argument(
        "--endpoint",
        action="append",
        default=[],
        metavar="ENTRY=SOCKET",
        help="--render: repeatable socket endpoint",
    )
    parser.add_argument(
        "--tail",
        default="",
        metavar="ARGS",
        help="--render: trailing options kept verbatim (e.g. '--poll-ms 1000')",
    )
    parser.add_argument(
        "--binary", default="data_service", help="--render: leading binary path"
    )
    parser.add_argument(
        "--target",
        action="store_true",
        help="--exec-start: print the three-socket target body instead of JSON",
    )
    args = parser.parse_args(argv)

    try:
        if args.target and not args.exec_start:
            raise LayoutError("--target requires --exec-start")
        if args.render:
            endpoints = [parse_endpoint(spec) for spec in args.endpoint]
            try:
                tail = shlex.split(args.tail)
            except ValueError as e:
                raise LayoutError("--tail is not shell-tokenizable: %s" % e)
            print(render(args.db, endpoints, tail, args.binary))
            return 0
        layout = parse(args.exec_start)
    except LayoutError as e:
        sys.stderr.write("data_unit_layout: %s\n" % e)
        return 2
    if args.target:
        print(target_exec_start(layout))
        return 0
    if args.format == "sh":
        sys.stdout.write(shell_assignments(layout))
    else:
        print(json.dumps(layout, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
