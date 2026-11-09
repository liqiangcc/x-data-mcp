#!/usr/bin/env python3
"""Read a research snapshot with the third-party readers a researcher uses.

Two modes, both offline:

``--self-check``
    Build a small Parquet file from the §7.4 column contract with ``pyarrow``
    and read it back with ``pyarrow`` and ``duckdb``. This proves the installed
    wheels and the *type choices* (DECIMAL(18,6) on int64, DECIMAL(20,2) and
    DECIMAL(28,18) on fixed-length byte arrays, DATE, INT8, UINT32) are
    readable by both consumers. It does not exercise the Rust writer.

    It also exercises the rules ``--snapshot-dir`` applies that are pure
    functions of the manifest — the byte check, the per-file schema versions
    and, since `#607` PR-E, the ETF 前复权 admission
    ([`etf_admission_problems`]) — against hand-built manifests, including the
    broken shapes each rule exists to reject. CI runs this mode before the
    real-snapshot mode, so a rule that stops rejecting a bad snapshot fails
    even when the snapshot of the day happens to be consistent.

``--snapshot-dir DIR``
    Read a real export directory: ``manifest.json`` first, then every file it
    lists, with ``pyarrow`` (row count, column order, arrow types) and
    ``duckdb`` (streaming aggregation). Every mismatch is a failure. This is
    the check that a Rust-only test cannot make: that a third-party reader
    accepts the bytes the exporter wrote.

Before any reader runs, ``--snapshot-dir`` also checks the bytes themselves
    (the cross-machine check of #549): every listed file must exist with the
    ``bytes`` and ``sha256`` the manifest records, ``snapshot_id`` must equal
    the id recomputed from the manifest's identity, and the directory must hold
    nothing the manifest does not list. One changed byte in a listed file, or
    in the identity part of ``manifest.json``, fails. The script prints the
    sha256 of ``manifest.json`` itself so two machines can compare it, and
    ``--manifest-sha256`` fails when it differs (the manifest's ``producer``,
    ``limitations`` and ``export`` blocks are in no other hash).

``--measure``
    Write production-shaped daily tables with the §7.4 schema and snappy and
    print bytes/row plus the extrapolation to the full history. This is the
    fixture behind the §7.7 size correction (M1 fix B); it uses pyarrow, so it
    measures the schema and encoding, not the Rust writer's exact bytes.

All modes exit non-zero on the first contract violation and print one line
per check, so CI can attribute a failure to a file and a column.

Versions are pinned by the caller (see ``.github/workflows/ci.yml`` and
``docs/tasks/issue-383-m1-research-data.md`` §7.5); this script never installs
anything and never touches the network.
"""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import pathlib
import sys
import tempfile

import pyarrow as pa
import pyarrow.parquet as pq

try:
    import duckdb
except ImportError:  # pragma: no cover - CI always installs it
    duckdb = None

# §7.4: the logical type of every column, per file. `arrow` is what pyarrow
# must report; the physical layout is checked by duckdb's aggregation below.
DECIMAL_18_6 = pa.decimal128(18, 6)
DECIMAL_18_2 = pa.decimal128(18, 2)
DECIMAL_20_2 = pa.decimal128(20, 2)
DECIMAL_28_18 = pa.decimal128(28, 18)

CONTRACT = {
    "daily_raw.parquet": {
        "entity": pa.string(),
        "trade_day": pa.date32(),
        "open": DECIMAL_18_6,
        "high": DECIMAL_18_6,
        "low": DECIMAL_18_6,
        "close": DECIMAL_18_6,
        "price_scale": pa.int8(),
        "volume": pa.int64(),
        "amount": DECIMAL_20_2,
        "r7_raw": pa.uint32(),
        "volume_quantum": pa.int32(),
        # v4 (M3-2 PR-D): 0 stock / 2 ETF; `no_trade` is NULL on stock rows and
        # true/false on ETF rows (derived: volume = 0 AND amount = 0, D-72).
        "instrument_class": pa.int8(),
        "no_trade": pa.bool_(),
    },
    "daily_qfq.parquet": {
        "entity": pa.string(),
        "trade_day": pa.date32(),
        "open": DECIMAL_18_6,
        "high": DECIMAL_18_6,
        "low": DECIMAL_18_6,
        "close": DECIMAL_18_6,
        "price_scale": pa.int8(),
        "volume": pa.int64(),
        "amount": DECIMAL_20_2,
        "r7_raw": pa.uint32(),
        "volume_quantum": pa.int32(),
        # v4 (M3-2 PR-D): 0 stock / 2 ETF; `no_trade` is NULL on stock rows and
        # true/false on ETF rows (derived: volume = 0 AND amount = 0, D-72).
        "instrument_class": pa.int8(),
        "no_trade": pa.bool_(),
    },
    "factors.parquet": {
        "entity": pa.string(),
        "ordinal": pa.int32(),
        "effective_day": pa.date32(),
        "known_at": pa.date32(),
        "category": pa.int8(),
        "c1": DECIMAL_18_2,
        "c2": DECIMAL_18_2,
        "c3": DECIMAL_18_2,
        "c4": DECIMAL_18_2,
        "factor_a": DECIMAL_28_18,
        "factor_b": DECIMAL_28_18,
        # `#607` PR-E (design §7.3): the evidence behind an event entering the
        # 前复权 chain. ETF events carry their source-store check verdict
        # (`verified_official` / `verified_announcement` / `no_effect`) or
        # `unverified` when no record covers them; A-share events carry the
        # fixed `gbbq-verified`.
        "basis_status": pa.string(),
    },
    "calendar.parquet": {
        "trade_day": pa.date32(),
        "sources": pa.int32(),
    },
    "index_daily.parquet": {
        "entity": pa.string(),
        "aliases": pa.string(),
        "trade_day": pa.date32(),
        "open": DECIMAL_18_6,
        "high": DECIMAL_18_6,
        "low": DECIMAL_18_6,
        "close": DECIMAL_18_6,
        "price_scale": pa.int8(),
        "volume": pa.int64(),
        "amount": DECIMAL_20_2,
        "aggregate_scope": pa.string(),
        "member_count": pa.int32(),
    },
    # M2 security-master files (§12.4). `flags` is a comma-separated list in
    # one STRING column: the M2 vocabulary is small, closed and per-row, and a
    # list column would make the file unreadable for the SQL-first consumers
    # this snapshot targets.
    "security_master.parquet": {
        "exchange": pa.string(),
        "code": pa.string(),
        "effective_from": pa.date32(),
        "effective_to": pa.date32(),
        "instrument_class": pa.int8(),
        "issuer_key": pa.string(),
        "issuer_resolution": pa.string(),
        "listing_date": pa.date32(),
        "listing_source": pa.string(),
        "delisting_date": pa.date32(),
        "delisting_source": pa.string(),
        "arrangement_start": pa.date32(),
        "arrangement_end": pa.date32(),
        "known_at": pa.date32(),
        "flags": pa.string(),
    },
    "security_names.parquet": {
        "exchange": pa.string(),
        "code": pa.string(),
        "effective_from": pa.date32(),
        "effective_to": pa.date32(),
        "name": pa.string(),
        "certainty": pa.string(),
        "known_at": pa.date32(),
        "source_kind": pa.string(),
    },
    "capital_events.parquet": {
        "exchange": pa.string(),
        "code": pa.string(),
        "ordinal": pa.int64(),
        "effective_day": pa.date32(),
        "category": pa.int8(),
        "float_after": pa.int64(),
        "total_after": pa.int64(),
        "known_at": pa.date32(),
        "suspicious": pa.bool_(),
    },
    "risk_warnings.parquet": {
        "exchange": pa.string(),
        "code": pa.string(),
        "effective_from": pa.date32(),
        "effective_to": pa.date32(),
        "normalized_status": pa.string(),
        "raw_status_code": pa.string(),
        "announced_at": pa.string(),
        "known_at": pa.date32(),
        "source_kind": pa.string(),
    },
    "suspensions.parquet": {
        "exchange": pa.string(),
        "code": pa.string(),
        "start_day": pa.date32(),
        "end_day": pa.date32(),
        "certainty": pa.string(),
        "missing_days": pa.int32(),
        "reason": pa.string(),
        "known_at": pa.date32(),
    },
    "price_limit_rules.parquet": {
        "rule_id": pa.string(),
        "market": pa.string(),
        "board": pa.string(),
        "status_class": pa.string(),
        "listing_phase": pa.string(),
        "band_bp": pa.int64(),
        "min_band_ticks": pa.int64(),
        "band_side": pa.string(),
        "rounding": pa.string(),
        "effective_from": pa.date32(),
        "effective_to": pa.date32(),
        "basis_ref": pa.string(),
        "basis_status": pa.string(),
    },
    "universe_intervals.parquet": {
        "entity": pa.string(),
        "canonical_entity": pa.string(),
        "aliases": pa.string(),
        "member_from": pa.date32(),
        "member_to": pa.date32(),
        "basis": pa.string(),
        "flags": pa.string(),
        # v4 (M3-2 PR-D): ETF rows carry both D-72 dates; both are NULL on
        # stock rows (a stock's end is `member_to`).
        "instrument_class": pa.int8(),
        "last_trade_date": pa.date32(),
        "delisting_date": pa.date32(),
    },
    # M3 whole-market files (#446 §10.4). `source_snapshot_sha256` is the
    # `tdxhy.cfg` content that proved the interval, so an assignment can be
    # traced past the export. `certainty` is the column that keeps a candidate
    # migration from reading as proof.
    "industry_assignments.parquet": {
        "entity": pa.string(),
        "exchange": pa.string(),
        "code": pa.string(),
        "scheme": pa.string(),
        "level": pa.int64(),
        "industry_code": pa.string(),
        "name": pa.string(),
        "name_status": pa.string(),
        "effective_from": pa.date32(),
        "effective_to": pa.date32(),
        "known_at": pa.date32(),
        "effective_from_basis": pa.string(),
        "evidence_grade": pa.string(),
        "source_snapshot_sha256": pa.string(),
    },
    "code_migrations.parquet": {
        "from": pa.string(),
        "to": pa.string(),
        "effective_from": pa.date32(),
        "certainty": pa.string(),
        "source_kind": pa.string(),
        "known_at": pa.date32(),
    },
}

# The snapshots this reader accepts. v2 stays readable: a directory written
# before #446 PR-9 has no M3 files and the five-column universe file. v4
# (XDM-157) adds `derived[].verified_through` (optional, only when a complete
# refresh run proves it) and the security-names/risk-warning registrations. The
# ETF delivery (M3-2 PR-D) appended the trailing columns in `V4_ADDED` to the
# two daily files and the universe file. It shipped under the `v4` string that
# preview.42/43 had already used for the shorter layout, so a `v4` directory has
# one of two layouts and only `files[].columns` says which (`etf_layout`). `v5`
# names the ETF layout, so it never needs that look; it also carries
# `producer`. A v2/v3 directory, and a v4 one without ETF columns, is checked
# against the shorter contract it was written with.
#
# `#607` PR-E adds `factors.parquet.basis_status` and the `etf_forward` identity
# block **without** moving the snapshot string: the daily files keep their v5
# columns (design §7.2 (a)), and the factors file is versioned on its own, in
# `files[].schema_version`. A v5 directory therefore has an eleven- or a
# twelve-column factors file, and that field says which.
SCHEMAS = {
    "x-data-mcp.research-snapshot.v2": 2,
    "x-data-mcp.research-snapshot.v3": 3,
    "x-data-mcp.research-snapshot.v4": 4,
    "x-data-mcp.research-snapshot.v5": 5,
}
V3_ONLY_FILES = ("industry_assignments.parquet", "code_migrations.parquet")
V2_UNIVERSE_COLUMNS = ("entity", "member_from", "member_to", "basis", "flags")
V4_ADDED = {
    "daily_raw.parquet": ("instrument_class", "no_trade"),
    "daily_qfq.parquet": ("instrument_class", "no_trade"),
    "universe_intervals.parquet": (
        "instrument_class",
        "last_trade_date",
        "delisting_date",
    ),
}


def etf_layout(version: int, files: list[dict]) -> bool | None:
    """Whether a snapshot's files use the ETF column layout (`V4_ADDED`).

    v5 always does and v2/v3 never do. A v4 directory is either: the
    preview.42/43 exporter wrote the short layout, preview.44/45 the long one
    under the same string, so the manifest's own column lists decide. `None`
    means the lists disagree with each other (some files long, some short),
    which no exporter writes.
    """
    if version >= 5:
        return True
    if version < 4:
        return False
    seen = {
        "instrument_class" in (entry.get("columns") or [])
        for entry in files
        if entry.get("path") in V4_ADDED
    }
    if len(seen) == 2:
        return None
    return seen == {True}


def contract_for(
    name: str, version: int, entry: dict | None = None
) -> dict[str, pa.DataType] | None:
    """The column contract of one listed file for the manifest's schema.

    `entry` is the file's own manifest record when the caller has it: a file
    versioned on its own (`files[].schema_version`) is checked against the
    version it declares, not against the snapshot string.
    """
    expected = CONTRACT.get(name)
    if expected is None:
        return None
    if version < 3:
        if name in V3_ONLY_FILES:
            return None
        if name == "universe_intervals.parquet":
            return {column: expected[column] for column in V2_UNIVERSE_COLUMNS}
    if version < 4 and name in V4_ADDED:
        return {
            column: dtype
            for column, dtype in expected.items()
            if column not in V4_ADDED[name]
        }
    if name == "factors.parquet":
        # `#607` PR-E added `basis_status` to the factors file only. A record
        # without `schema_version` predates it (or was written by a binary that
        # did not version the file), so it is the eleven-column layout.
        if (entry or {}).get("schema_version") is None:
            return {
                column: dtype
                for column, dtype in expected.items()
                if column != "basis_status"
            }
    return expected

# The day column each file orders by; the M2 files are intervals, so their
# first day is the lower bound (§12.4).
DAY_COLUMN = {
    "factors.parquet": "effective_day",
    "security_master.parquet": "effective_from",
    "security_names.parquet": "effective_from",
    "capital_events.parquet": "effective_day",
    "risk_warnings.parquet": "effective_from",
    "suspensions.parquet": "start_day",
    "price_limit_rules.parquet": "effective_from",
    "universe_intervals.parquet": "member_from",
    "industry_assignments.parquet": "effective_from",
    "code_migrations.parquet": "effective_from",
}


# Keys `manifest::document` adds on top of the identity (src/research/manifest.rs).
# `snapshot_id` is the hash of everything else, so a reader recomputes it from
# the manifest minus these. The same rule holds for v2 to v5.
DOCUMENT_ONLY = ("snapshot_id", "producer", "files", "limitations", "export")
ID_PREFIX = "snapshot-"


def sha256_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def canonical_json(value) -> str:
    """RFC 8785 for the value kinds an identity holds.

    The Rust side hashes `serde_json_canonicalizer` output. The identity has
    only objects, arrays, strings, integers, booleans and null, so sorting keys
    by UTF-16 code units and writing minimal escapes reproduces it. A float is
    refused rather than guessed: its ES6 text form is not Python's.
    """
    if isinstance(value, float):
        raise ValueError("a float in the identity has no canonical form here")
    if isinstance(value, dict):
        items = sorted(value.items(), key=lambda item: item[0].encode("utf-16-be"))
        return "{" + ",".join(
            json.dumps(key, ensure_ascii=False) + ":" + canonical_json(item)
            for key, item in items
        ) + "}"
    if isinstance(value, list):
        return "[" + ",".join(canonical_json(item) for item in value) + "]"
    return json.dumps(value, ensure_ascii=False)


def recompute_snapshot_id(manifest: dict) -> str:
    identity = {k: v for k, v in manifest.items() if k not in DOCUMENT_ONLY}
    digest = hashlib.sha256(canonical_json(identity).encode("utf-8")).hexdigest()
    return ID_PREFIX + digest[:32]


def byte_problems(directory: pathlib.Path, manifest: dict) -> list[str]:
    """Every way the directory's bytes differ from what the manifest records.

    Reads each listed file once. Needs no reader library, so it is the part of
    this script a machine without pyarrow can still run in spirit; it returns
    the problems instead of recording them so `--self-check` can assert both
    outcomes.
    """
    problems: list[str] = []
    try:
        expected_id = recompute_snapshot_id(manifest)
    except ValueError as error:
        problems.append(f"snapshot_id cannot be recomputed: {error}")
    else:
        if manifest.get("snapshot_id") != expected_id:
            problems.append(
                f"snapshot_id {manifest.get('snapshot_id')!r} != {expected_id} "
                "recomputed from the manifest identity"
            )
    listed = set()
    for entry in manifest.get("files") or []:
        name = entry.get("path")
        if not isinstance(name, str) or name != pathlib.PurePath(name).name or name in ("", ".", ".."):
            problems.append(f"files[].path {name!r} is not a plain file name")
            continue
        listed.add(name)
        path = directory / name
        if not path.is_file():
            problems.append(f"{name}: listed in the manifest but missing")
            continue
        want_sha = entry.get("sha256")
        want_bytes = entry.get("bytes")
        if not isinstance(want_sha, str) or not isinstance(want_bytes, int):
            problems.append(f"{name}: the manifest records no bytes/sha256 (a --dry-run manifest?)")
            continue
        size = path.stat().st_size
        if size != want_bytes:
            problems.append(f"{name}: {size} bytes on disk, manifest says {want_bytes}")
        actual = sha256_file(path)
        if actual != want_sha:
            problems.append(f"{name}: sha256 {actual} != manifest {want_sha}")
    for path in sorted(directory.iterdir()):
        if path.name != "manifest.json" and path.name not in listed:
            problems.append(f"{path.name}: in the directory but not listed in the manifest")
    return problems


failures: list[str] = []


def check(ok: bool, message: str) -> None:
    if not ok:
        failures.append(message)
        print(f"FAIL {message}")


def physical_of(dtype: pa.DataType) -> tuple[str, int | None]:
    """The physical layout §7.4 asks for, derived from the logical type.

    Prices fit `INT64` (18 digits), `amount` needs 20 digits so it goes to a
    9-byte fixed-length integer, and the 28-digit factor affine needs 16.
    """
    if pa.types.is_decimal(dtype):
        if dtype.precision <= 18:
            return ("INT64", None)
        return ("FIXED_LEN_BYTE_ARRAY", 9 if dtype.precision <= 20 else 16)
    if dtype in (pa.int64(),):
        return ("INT64", None)
    if dtype in (pa.int32(), pa.int8(), pa.uint32(), pa.date32()):
        return ("INT32", None)
    if dtype == pa.bool_():
        return ("BOOLEAN", None)
    if pa.types.is_string(dtype):
        return ("BYTE_ARRAY", None)
    raise AssertionError(f"no physical mapping for {dtype}")


def check_physical(
    name: str,
    path: pathlib.Path,
    expected: dict[str, pa.DataType],
) -> None:
    """Assert the stored physical types, not just the logical ones.

    A reader happily widens a `DECIMAL(20,2)` written as `BYTE_ARRAY`, so the
    logical check alone would not notice a layout change. This runs on a real
    export only: it asserts the exact layout the Rust writer produces, while a
    file written by pyarrow uses its own (equally legal) widths.
    """
    parquet = pq.ParquetFile(path)
    stored = {column.path: column for column in parquet.schema}
    for column, dtype in expected.items():
        if column not in stored:
            continue
        actual = stored[column]
        want_type, want_length = physical_of(dtype)
        check(
            str(actual.physical_type) == want_type,
            f"{name}.{column}: physical {actual.physical_type} != {want_type}",
        )
        if want_length is not None:
            check(
                actual.length == want_length,
                f"{name}.{column}: fixed length {actual.length} != {want_length}",
            )


def check_schema(name: str, path: pathlib.Path, expected: dict[str, pa.DataType]) -> int:
    parquet = pq.ParquetFile(path)
    schema = parquet.schema_arrow
    names = list(schema.names)
    check(names == list(expected), f"{name}: columns {names} != {list(expected)}")
    for column, dtype in expected.items():
        if column not in names:
            continue
        actual = schema.field(column).type
        # `decimal128(p, s)` equality is exact; everything else compares by
        # equality too, so a wrong physical type shows up here.
        check(actual == dtype, f"{name}.{column}: type {actual} != {dtype}")
    return parquet.metadata.num_rows


def sql_path(path: pathlib.Path) -> str:
    """A literal `read_parquet` argument: paths here are CI-controlled."""
    return "'" + str(path).replace("'", "''") + "'"


def check_with_duckdb(name: str, path: pathlib.Path, rows: int) -> None:
    if duckdb is None:
        check(False, "duckdb is not installed")
        return
    connection = duckdb.connect()
    try:
        # Streaming aggregate over the file: never materializes the columns in
        # Python, which is how a researcher is told to read the 16.7M-row
        # files (§7.5).
        day = DAY_COLUMN.get(name, "trade_day")
        count, first, last = connection.execute(
            f"SELECT count(*), min({day}), max({day}) FROM read_parquet({sql_path(path)})"
        ).fetchone()
        check(count == rows, f"{name}: duckdb counted {count} rows, manifest says {rows}")
        if rows:
            check(
                first is not None and last is not None and first <= last,
                f"{name}: duckdb day range {first}..{last} is empty or reversed",
            )
        print(f"ok   {name}: {count} rows, {first}..{last} (duckdb)")
    finally:
        connection.close()


def check_etf_rows(directory: pathlib.Path, manifest: dict | None = None) -> None:
    """M3-2 PR-D / `#607` PR-E semantics a column-type check cannot see.

    D-70 Q7 / D-72:

    * `daily_raw`: class is 0 or 2; `no_trade` is NULL exactly on stock rows;
      an ETF `no_trade` row really has volume 0 and amount 0 and a traded ETF
      row does not; ETF prices are 3dp (`price_scale` 3).
    * `universe_intervals`: the two dates exist on ETF rows only, and the last
      traded day never falls after the termination date.

    `#607` PR-E (design §7.1) — `daily_qfq` now carries the ETFs the read
    surface's 前复权 gate admitted:

    * every ETF row is `price_scale=6` (a 3dp ETF row here would be the 不复权
      block leaking into the 前复权 file);
    * every ETF entity in `daily_qfq` has a `qfq` entry in `manifest.series`
      (i.e. a stored `Forward` row in `market_series`) **and** appears in
      `manifest.etf_forward.admitted` — the file and the admission cannot
      disagree;
    * the admitted and blocked lists partition the ETF entities in `daily_raw`:
      a blocked ETF keeps its 不复权 rows and is named with a reason, so no ETF
      is silently missing from the snapshot.
    """
    if duckdb is None:
        return
    connection = duckdb.connect()
    try:
        raw = directory / "daily_raw.parquet"
        if raw.is_file():
            source = f"read_parquet({sql_path(raw)})"
            bad_class = connection.execute(
                f"SELECT count(*) FROM {source} WHERE instrument_class NOT IN (0, 2)"
            ).fetchone()[0]
            check(bad_class == 0, f"daily_raw: {bad_class} rows with a class other than 0/2")
            marker = connection.execute(
                f"SELECT count(*) FROM {source} WHERE (instrument_class = 0) <> (no_trade IS NULL)"
            ).fetchone()[0]
            check(marker == 0, f"daily_raw: {marker} rows where no_trade nullness disagrees with the class")
            wrong = connection.execute(
                f"SELECT count(*) FROM {source} WHERE instrument_class = 2 AND "
                "no_trade <> (coalesce(volume, -1) = 0 AND coalesce(amount, -1) = 0)"
            ).fetchone()[0]
            check(wrong == 0, f"daily_raw: {wrong} ETF rows whose no_trade is not volume=0 AND amount=0")
            scale = connection.execute(
                f"SELECT count(*) FROM {source} WHERE instrument_class = 2 AND price_scale <> 3"
            ).fetchone()[0]
            check(scale == 0, f"daily_raw: {scale} ETF rows not at price_scale 3")
            etf_rows, flagged = connection.execute(
                f"SELECT count(*), count(*) FILTER (WHERE no_trade) FROM {source} "
                "WHERE instrument_class = 2"
            ).fetchone()
            print(f"ok   daily_raw.parquet: {etf_rows} ETF rows, {flagged} no_trade")
        qfq = directory / "daily_qfq.parquet"
        if qfq.is_file():
            source = f"read_parquet({sql_path(qfq)})"
            classes = connection.execute(
                f"SELECT count(*) FROM {source} WHERE instrument_class NOT IN (0, 2)"
            ).fetchone()[0]
            check(classes == 0, f"daily_qfq: {classes} rows with a class other than 0/2")
            scale = connection.execute(
                f"SELECT count(*) FROM {source} WHERE instrument_class = 2 AND price_scale <> 6"
            ).fetchone()[0]
            check(scale == 0, f"daily_qfq: {scale} ETF rows not at price_scale 6")
            etf_entities = sorted(
                row[0]
                for row in connection.execute(
                    f"SELECT DISTINCT entity FROM {source} WHERE instrument_class = 2"
                ).fetchall()
            )
            if etf_entities:
                print(f"ok   daily_qfq.parquet: {len(etf_entities)} ETF entities at price_scale 6")
            if manifest is not None:
                forward = manifest.get("etf_forward") or {}
                admitted = set(forward.get("admitted") or [])
                blocked = {
                    entry["entity"]: entry.get("reason") or ""
                    for entry in forward.get("blocked") or []
                }
                qfq_series = {
                    entry["entity"]
                    for entry in manifest.get("series") or []
                    if entry.get("adjustment") == "qfq"
                    and entry.get("instrument_class") == "etf"
                }
                raw_etfs = []
                if raw.is_file():
                    raw_etfs = [
                        row[0]
                        for row in connection.execute(
                            f"SELECT DISTINCT entity FROM read_parquet({sql_path(raw)}) "
                            "WHERE instrument_class = 2"
                        ).fetchall()
                    ]
                # One rule, exercised offline by `--self-check`: the file and
                # the admission must tell one story, and no ETF may be missing
                # from both lists.
                problems = etf_admission_problems(
                    qfq_entities=etf_entities,
                    raw_entities=raw_etfs,
                    qfq_series=qfq_series,
                    admitted=admitted,
                    blocked=blocked,
                )
                for problem in problems:
                    check(False, f"etf_forward: {problem}")
                print(
                    f"ok   manifest.etf_forward: {len(admitted)} admitted, "
                    f"{len(blocked)} blocked, {len(raw_etfs)} ETFs in daily_raw"
                )
        universe = directory / "universe_intervals.parquet"
        if universe.is_file():
            source = f"read_parquet({sql_path(universe)})"
            stray = connection.execute(
                f"SELECT count(*) FROM {source} WHERE instrument_class <> 2 AND "
                "(last_trade_date IS NOT NULL OR delisting_date IS NOT NULL)"
            ).fetchone()[0]
            check(stray == 0, f"universe_intervals: {stray} non-ETF rows carry an ETF date")
            inverted = connection.execute(
                f"SELECT count(*) FROM {source} WHERE last_trade_date > delisting_date"
            ).fetchone()[0]
            check(inverted == 0, f"universe_intervals: {inverted} rows trade after the termination date")
            print("ok   universe_intervals.parquet: ETF dates are consistent")
    finally:
        connection.close()


def check_factor_basis(directory: pathlib.Path) -> None:
    """`#607` PR-E (design §7.3): the `basis_status` column's value space.

    A value outside the four ETF verdicts and the one A-share value would be a
    verdict no reader can interpret, and an A-share event claiming a check
    record would be a verification that does not exist. Both are cheap to see
    here and expensive to notice downstream.
    """
    if duckdb is None:
        return
    path = directory / "factors.parquet"
    if not path.is_file():
        return
    connection = duckdb.connect()
    try:
        source = f"read_parquet({sql_path(path)})"
        # `DESCRIBE` needs a relation, not a table function call, so the file is
        # registered as a view first.
        connection.execute(f"CREATE VIEW factors AS SELECT * FROM {source}")
        columns = [
            row[0] for row in connection.execute("DESCRIBE factors").fetchall()
        ]
        if "basis_status" not in columns:
            return
        source = "factors"
        allowed = (
            "verified_official",
            "verified_announcement",
            "no_effect",
            "unverified",
            "gbbq-verified",
        )
        quoted = ", ".join(f"'{value}'" for value in allowed)
        unknown = connection.execute(
            f"SELECT count(*) FROM {source} WHERE basis_status NOT IN ({quoted})"
        ).fetchone()[0]
        check(unknown == 0, f"factors: {unknown} rows with an unknown basis_status")
        # An ETF event can never carry the A-share constant, and vice versa:
        # the value records which evidence admitted the event.
        wrong_class = connection.execute(
            f"SELECT count(*) FROM {source} WHERE "
            "(basis_status = 'gbbq-verified' AND (entity LIKE 'sh.51%' OR entity LIKE 'sz.159%'))"
        ).fetchone()[0]
        check(
            wrong_class == 0,
            f"factors: {wrong_class} ETF rows claim the A-share basis value",
        )
        counts = connection.execute(
            f"SELECT basis_status, count(*) FROM {source} GROUP BY 1 ORDER BY 1"
        ).fetchall()
        summary = ", ".join(f"{status}={count}" for status, count in counts)
        print(f"ok   factors.parquet: basis_status {{{summary}}}")
    finally:
        connection.close()


def check_producer(manifest: dict) -> None:
    """v5 `producer`: the writing binary and its derivation labels (recorded,
    not part of `snapshot_id`)."""
    producer = manifest.get("producer")
    check(isinstance(producer, dict), "v5 manifest has no producer block")
    if not isinstance(producer, dict):
        return
    sha = producer.get("git_sha")
    check(
        isinstance(sha, str)
        and (sha == "unknown" or (len(sha) == 40 and all(c in "0123456789abcdef" for c in sha))),
        f"producer.git_sha {sha!r} is not a 40-hex git SHA (or 'unknown')",
    )
    algorithms = producer.get("algorithms")
    check(
        isinstance(algorithms, dict)
        and {"price_limit", "market_cap"} <= set(algorithms)
        and all(isinstance(v, str) and v for v in algorithms.values()),
        f"producer.algorithms {algorithms!r} must name at least price_limit and market_cap",
    )
    # `algorithms` (identity) and `producer` (record) are different blocks.
    check("git_sha" not in (manifest.get("algorithms") or {}), "git_sha leaked into the identity")
    print(f"ok   producer: git_sha {sha}, {len(algorithms or {})} derivation labels")


def check_snapshot(directory: pathlib.Path, manifest_sha256: str | None = None) -> None:
    manifest_path = directory / "manifest.json"
    check(manifest_path.is_file(), f"{directory}: manifest.json is missing")
    if not manifest_path.is_file():
        return
    manifest_actual = sha256_file(manifest_path)
    print(f"manifest.json sha256 {manifest_actual}")
    if manifest_sha256 is not None:
        check(
            manifest_actual == manifest_sha256.lower(),
            f"manifest.json sha256 {manifest_actual} != expected {manifest_sha256}",
        )
    manifest = json.loads(manifest_path.read_text())
    version = SCHEMAS.get(manifest.get("schema"))
    check(
        version is not None,
        f"manifest schema {manifest.get('schema')!r} is not one of {sorted(SCHEMAS)}",
    )
    if version is None:
        return
    check(
        directory.name == manifest.get("snapshot_id"),
        f"directory {directory.name} != manifest snapshot_id {manifest.get('snapshot_id')}",
    )
    files = manifest.get("files") or []
    check(len(files) > 0, "manifest lists no file")
    # Bytes first: a reader would only report a corrupt file as a type error.
    problems = byte_problems(directory, manifest)
    for problem in problems:
        check(False, problem)
    if not problems:
        total = sum(entry["bytes"] for entry in files)
        print(
            f"ok   bytes: {len(files)} files, {total} bytes match files[].sha256; "
            f"{manifest.get('snapshot_id')} recomputes from the identity"
        )
    listed = {entry.get("path") for entry in files}
    # §10.4: a v3 snapshot always carries the M3 files, even when the intervals
    # are empty (a real empty file, not an absent one). A v2 snapshot never
    # does.
    for name in V3_ONLY_FILES:
        check(
            (name in listed) == (version >= 3),
            f"{name}: listed={name in listed} for schema v{version}",
        )
    layout = etf_layout(version, files)
    check(
        layout is not None,
        "v4 manifest mixes the ETF column layout with the short one across files",
    )
    if layout is None:
        return
    # The contract version the columns follow: a v4 directory without the ETF
    # columns is the preview.42/43 layout, which is the v3 one.
    columns_version = version if layout else min(version, 3)
    for entry in files:
        name = entry["path"]
        path = directory / name
        check(path.is_file(), f"{name}: listed in the manifest but missing")
        if not path.is_file():
            continue
        expected = contract_for(name, columns_version, entry)
        if expected is None:
            check(False, f"{name}: no type contract in this script for schema v{version}")
            continue
        check(
            list(entry.get("columns") or []) == list(expected),
            f"{name}: manifest columns {entry.get('columns')} != {list(expected)}",
        )
        rows = check_schema(name, path, expected)
        check_physical(name, path, expected)
        check(rows == entry.get("rows"), f"{name}: {rows} rows, manifest says {entry.get('rows')}")
        check_with_duckdb(name, path, rows)
        print(f"ok   {name}: {rows} rows, schema matches §7.4")

    # XDM-157 (#446 §5.3): `derived[].verified_through` is optional — a fresh
    # init has no complete refresh run to name — but when present it is a day,
    # not a timestamp or a free-form string.
    for entry in manifest.get("derived") or []:
        if "verified_through" not in entry:
            continue
        day = entry["verified_through"]
        valid = isinstance(day, str) and len(day) == 10 and day[4] == "-" and day[7] == "-"
        if valid:
            try:
                datetime.date.fromisoformat(day)
            except ValueError:
                valid = False
        check(
            valid,
            f"derived {entry.get('dataset')}: verified_through {day!r} is not YYYY-MM-DD",
        )
        print(f"ok   derived {entry.get('dataset')}: verified_through {day}")

    if version >= 5:
        check_producer(manifest)
    elif "producer" in manifest:
        check(False, f"schema v{version} must not carry a producer block")
    if layout:
        check_etf_rows(directory, manifest)
        check_factor_basis(directory)

    # The owner decision (#381 option 1, D-34): the column exists in the
    # exported schema even while M1 stores NULL.
    if (directory / "daily_raw.parquet").is_file() and duckdb is not None:
        connection = duckdb.connect()
        try:
            present = connection.execute(
                "SELECT count(volume_quantum) FROM read_parquet("
                f"{sql_path(directory / 'daily_raw.parquet')})"
            ).fetchone()[0]
            print(f"ok   daily_raw.parquet: {present} non-NULL volume_quantum")
        finally:
            connection.close()


def self_check_bytes() -> None:
    """The byte check passes on a consistent directory and fails on each
    kind of difference (one byte, a size, a missing, an extra and a renamed
    file, an identity edit)."""
    identity = {
        "schema": "x-data-mcp.research-snapshot.v5",
        "request": {"entities": "catalog", "start": None},
        "series": [{"entity": "sh.600000", "record_count": 3}],
    }
    payload = {"a.parquet": b"alpha" * 100, "b.parquet": b"beta"}

    def build(root: pathlib.Path, producer: bool = True) -> dict:
        files = []
        for name, data in payload.items():
            (root / name).write_bytes(data)
            files.append(
                {
                    "path": name,
                    "bytes": len(data),
                    "sha256": hashlib.sha256(data).hexdigest(),
                }
            )
        manifest = {
            **identity,
            "snapshot_id": recompute_snapshot_id(identity),
            "files": files,
            "limitations": [],
            "export": {},
        }
        # v2 to v4 manifests carry no `producer`; the id rule is the same.
        if producer:
            manifest["producer"] = {"git_sha": "unknown"}
        (root / "manifest.json").write_text(json.dumps(manifest))
        return manifest

    # The same canonical form the Rust identity hash uses: keys sorted, no
    # spaces, null kept. A fixed id pins the hash input against drift.
    check(
        canonical_json({"b": [1, None], "a": "x\u00e9"}) == '{"a":"x\u00e9","b":[1,null]}',
        "canonical_json does not sort keys / keep non-ASCII text",
    )

    def mutate(label: str, change) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            manifest = build(root)
            change(root, manifest)
            check(
                len(byte_problems(root, manifest)) > 0,
                f"byte check missed: {label}",
            )

    with tempfile.TemporaryDirectory() as tmp:
        root = pathlib.Path(tmp)
        manifest = build(root)
        check(byte_problems(root, manifest) == [], "byte check fails a consistent directory")
    with tempfile.TemporaryDirectory() as tmp:
        root = pathlib.Path(tmp)
        manifest = build(root, producer=False)
        check(
            byte_problems(root, manifest) == [],
            "byte check fails a consistent directory without a producer block",
        )

    def flip(root, manifest):
        data = bytearray((root / "a.parquet").read_bytes())
        data[7] ^= 1
        (root / "a.parquet").write_bytes(bytes(data))

    def truncate(root, manifest):
        (root / "b.parquet").write_bytes(b"bet")

    def remove(root, manifest):
        (root / "b.parquet").unlink()

    def extra(root, manifest):
        (root / "stray.parquet").write_bytes(b"x")

    def identity_edit(root, manifest):
        manifest["series"][0]["record_count"] = 4

    def traversal(root, manifest):
        manifest["files"][0]["path"] = "../a.parquet"

    mutate("one flipped byte", flip)
    mutate("a shortened file", truncate)
    mutate("a missing file", remove)
    mutate("an unlisted file", extra)
    mutate("an edited identity", identity_edit)
    mutate("a path outside the directory", traversal)
    print("ok   byte check: consistent directory passes, 6 differences fail")


def self_check_etf_admission() -> None:
    """`#607` PR-E (design §7.1) admission rules, without duckdb.

    The directory-level assertions need duckdb and run in `--snapshot-dir`; the
    rules themselves are pure, so they are exercised here on the small
    manifest shapes a broken exporter could produce.
    """
    admitted = {"sh.510050", "sz.159919"}
    blocked = {"sz.159912": "etf_factor_unverified"}
    # An ETF in daily_qfq that is not admitted is the disagreement this check
    # exists for: the file and the manifest must tell one story.
    check(
        etf_admission_problems(
            qfq_entities=["sh.510050"],
            raw_entities=["sh.510050", "sz.159912"],
            qfq_series={"sh.510050"},
            admitted=admitted,
            blocked=blocked,
        )
        == [],
        "a consistent admission was rejected",
    )
    check(
        etf_admission_problems(
            qfq_entities=["sh.510050", "sh.510880"],
            raw_entities=["sh.510050", "sh.510880"],
            qfq_series={"sh.510050", "sh.510880"},
            admitted=admitted,
            blocked=blocked,
        ),
        "an unadmitted ETF in daily_qfq was accepted",
    )
    check(
        etf_admission_problems(
            qfq_entities=["sh.510050"],
            raw_entities=["sh.510050"],
            qfq_series=set(),
            admitted=admitted,
            blocked=blocked,
        ),
        "an ETF in daily_qfq without a qfq series entry was accepted",
    )
    check(
        etf_admission_problems(
            qfq_entities=[],
            raw_entities=["sh.510050", "sh.510880"],
            qfq_series=set(),
            admitted=admitted,
            blocked=blocked,
        ),
        "an ETF that is neither admitted nor blocked was accepted",
    )
    check(
        etf_admission_problems(
            qfq_entities=[],
            raw_entities=[],
            qfq_series=set(),
            admitted=admitted | {"sz.159912"},
            blocked=blocked,
        ),
        "an ETF both admitted and blocked was accepted",
    )
    print("ok   etf admission: 1 consistent shape accepted, 4 broken shapes rejected")


def etf_admission_problems(
    *,
    qfq_entities: list[str],
    raw_entities: list[str],
    qfq_series: set[str],
    admitted: set[str],
    blocked: dict[str, str],
) -> list[str]:
    """Every way a snapshot's ETF admission can contradict itself (§7.1).

    Returns one message per problem, so `--snapshot-dir` can print them and
    `--self-check` can prove the rule rejects each shape. Pure: no file, no
    duckdb.
    """
    problems = []
    for entity in qfq_entities:
        if entity not in qfq_series:
            problems.append(
                f"{entity} is in daily_qfq but has no qfq series entry (no stored Forward row)"
            )
        if entity not in admitted:
            problems.append(f"{entity} is in daily_qfq but not in etf_forward.admitted")
    for entity in sorted(admitted & set(blocked)):
        problems.append(f"{entity} is both admitted and blocked")
    for entity in sorted(set(raw_entities) - admitted - set(blocked)):
        problems.append(
            f"{entity} has daily_raw rows but is neither admitted nor blocked "
            "(a silently missing ETF)"
        )
    for entity, reason in blocked.items():
        if not reason:
            problems.append(f"{entity} is blocked with no reason")
    return problems


def self_check() -> None:
    """Exercise the readers and the §7.4 type contract without Rust."""
    import decimal

    # #446 PR-9: a v2 snapshot stays readable. Its universe file has the five
    # pre-M3 columns and it never lists the M3 files, so the reader must not
    # demand them (checked on a real directory by `--snapshot-dir`).
    check(
        list(contract_for("universe_intervals.parquet", 2)) == list(V2_UNIVERSE_COLUMNS),
        "v2 universe_intervals contract changed",
    )
    for name in V3_ONLY_FILES:
        check(
            contract_for(name, 2) is None and contract_for(name, 3) is not None,
            f"{name}: version-gated contract is wrong",
        )
    # M3-2 PR-D: the ETF columns exist from v4 on only, appended after the v3
    # columns, so a v3 directory is still read with the contract it was
    # written with.
    for name, added in V4_ADDED.items():
        before = list(contract_for(name, 3))
        after = list(contract_for(name, 4))
        check(
            after == before + list(added),
            f"{name}: v4 columns are not the v3 ones plus {list(added)}",
        )
        check(
            list(contract_for(name, 5)) == after,
            f"{name}: v5 columns are not the ETF layout",
        )
    # One `v4` string, two layouts: the manifest's column lists decide; `v5`
    # and `v2`/`v3` never need to look.
    short = [{"path": "daily_raw.parquet", "columns": ["entity"]}]
    long = [{"path": "daily_raw.parquet", "columns": ["entity", "instrument_class"]}]
    mixed = short + [{"path": "daily_qfq.parquet", "columns": ["instrument_class"]}]
    check(etf_layout(4, short) is False, "v4 short layout not recognised")
    check(etf_layout(4, long) is True, "v4 ETF layout not recognised")
    check(etf_layout(4, mixed) is None, "v4 mixed layout not rejected")
    check(etf_layout(5, short) is True, "v5 is the ETF layout whatever it lists")
    check(etf_layout(3, long) is False, "v3 has no ETF layout")

    # `#607` PR-E: `factors.parquet` is versioned on its own, so the snapshot
    # string stays v5 and `files[].schema_version` says which layout the file
    # is. An unversioned record is the pre-PR-E eleven-column file.
    v1_factors = list(contract_for("factors.parquet", 5))
    v2_factors = list(contract_for("factors.parquet", 5, {"schema_version": 2}))
    check(
        "basis_status" not in v1_factors,
        "an unversioned factors file must not require basis_status",
    )
    check(
        v2_factors == v1_factors + ["basis_status"],
        "factors v2 columns are not the v1 ones plus basis_status",
    )
    check(
        list(contract_for("factors.parquet", 5, {"schema_version": None}))
        == v1_factors,
        "an explicit null schema_version is the v1 layout",
    )
    # The daily files are not versioned on their own: PR-E added no column.
    for name in ("daily_raw.parquet", "daily_qfq.parquet"):
        check(
            list(contract_for(name, 5, {"schema_version": None}))
            == list(contract_for(name, 5, {"schema_version": 2})),
            f"{name}: an own schema version must not change the daily columns",
        )

    self_check_bytes()
    self_check_etf_admission()

    for name, expected in CONTRACT.items():
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / name
            columns = {}
            for column, dtype in expected.items():
                if pa.types.is_decimal(dtype):
                    columns[column] = pa.array(
                        [decimal.Decimal("1.5"), None], type=dtype
                    )
                elif dtype == pa.date32():
                    columns[column] = pa.array(
                        [datetime.date(2026, 10, 20), None], type=dtype
                    )
                elif pa.types.is_string(dtype):
                    columns[column] = pa.array(["sh.600000", None], type=dtype)
                elif dtype == pa.bool_():
                    columns[column] = pa.array([True, None], type=dtype)
                else:
                    columns[column] = pa.array([1, None], type=dtype)
            table = pa.table(columns)
            pq.write_table(table, path)
            rows = check_schema(name, path, expected)
            check(rows == 2, f"{name}: {rows} rows written, expected 2")
            check_with_duckdb(name, path, rows)
            print(f"ok   {name}: self-check round trip")


def measure() -> None:
    """§7.7 fixture: bytes/row of the two daily files, production-shaped.

    The values are deterministic. The 前复权 prices carry varying low digits
    because that is what a 6dp exact adjustment produces; a run of small
    integers would compress unrealistically well and hide the fixed-width
    floor (4 × int64 prices + int64 volume + 9-byte amount).
    """
    import decimal

    rows = 100_000
    entities = 5_228
    base = datetime.date(2000, 1, 3)
    entity = [f"sh.{600000 + index % entities}" for index in range(rows)]
    days = [base + datetime.timedelta(days=index // entities) for index in range(rows)]
    scale = pa.array([2] * rows, type=pa.int8())
    volume = [(index * 7_919) % 10_000_000 + 1_000 for index in range(rows)]
    amount = [(index * 104_729) % 10_000_000_000 + 1_000_000 for index in range(rows)]
    r7 = [(index * 2_654_435_761) % 2**32 for index in range(rows)]

    def write(name: str, prices: list[int]) -> None:
        table = pa.table(
            {
                "entity": pa.array(entity, pa.string()),
                "trade_day": pa.array(days, pa.date32()),
                "open": pa.array([p - 10 for p in prices], pa.int64()),
                "high": pa.array([p + 10 for p in prices], pa.int64()),
                "low": pa.array([p - 20 for p in prices], pa.int64()),
                "close": pa.array(prices, pa.int64()),
                "price_scale": scale,
                "volume": pa.array(volume, pa.int64()),
                "amount": pa.array(
                    [decimal.Decimal(value).scaleb(-2) for value in amount],
                    DECIMAL_20_2,
                ),
                "r7_raw": pa.array(r7, pa.uint32()),
                "volume_quantum": pa.array([None] * rows, pa.int32()),
            }
        )
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / name
            # The Rust writer uses format 1.0, dictionary encoding and snappy
            # (§7.6 / §7.8); match those so the numbers are comparable.
            pq.write_table(
                table,
                path,
                compression="snappy",
                row_group_size=65_536,
                use_dictionary=True,
                data_page_version="1.0",
            )
            size = path.stat().st_size
        per_row = size / rows
        full = per_row * 16_749_060
        print(
            f"{name}: {rows} rows, {size} bytes, {per_row:.1f} B/row, "
            f"full history ~{full / 1e6:.0f} MB"
        )

    # 2dp raw values widened to the 6dp price column: exact ×10,000.
    raw = [((index * 37) % 4_500 + 500) * 10_000 for index in range(rows)]
    # 6dp qfq values with varying low digits (1 to 900 yuan).
    qfq = [1_000_000 + ((index * 2_654_435_761) % 900_000_000) for index in range(rows)]
    write("daily_raw.parquet", raw)
    write("daily_qfq.parquet", qfq)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--snapshot-dir", type=pathlib.Path)
    parser.add_argument(
        "--manifest-sha256",
        help="expected sha256 of manifest.json (compare it across machines)",
    )
    parser.add_argument("--self-check", action="store_true")
    parser.add_argument("--measure", action="store_true")
    args = parser.parse_args()
    if args.self_check:
        self_check()
    if args.measure:
        measure()
    if args.snapshot_dir:
        check_snapshot(args.snapshot_dir, args.manifest_sha256)
    if args.manifest_sha256 and not args.snapshot_dir:
        parser.error("--manifest-sha256 needs --snapshot-dir")
    if not args.self_check and not args.measure and not args.snapshot_dir:
        parser.error("one of --self-check / --measure / --snapshot-dir is required")
    print(f"pyarrow {pa.__version__}, duckdb {getattr(duckdb, '__version__', 'missing')}")
    if failures:
        print(f"{len(failures)} failure(s)")
        return 1
    print("all checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
