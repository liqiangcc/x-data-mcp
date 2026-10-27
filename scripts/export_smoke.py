#!/usr/bin/env python3
"""Read a research snapshot with the third-party readers a researcher uses.

Two modes, both offline:

``--self-check``
    Build a small Parquet file from the §7.4 column contract with ``pyarrow``
    and read it back with ``pyarrow`` and ``duckdb``. This proves the installed
    wheels and the *type choices* (DECIMAL(18,6) on int64, DECIMAL(20,2) and
    DECIMAL(28,18) on fixed-length byte arrays, DATE, INT8, UINT32) are
    readable by both consumers. It does not exercise the Rust writer.

``--snapshot-dir DIR``
    Read a real export directory: ``manifest.json`` first, then every file it
    lists, with ``pyarrow`` (row count, column order, arrow types) and
    ``duckdb`` (streaming aggregation). Every mismatch is a failure. This is
    the check that a Rust-only test cannot make: that a third-party reader
    accepts the bytes the exporter wrote.

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
        "band_side": pa.string(),
        "rounding": pa.string(),
        "effective_from": pa.date32(),
        "effective_to": pa.date32(),
        "basis_ref": pa.string(),
        "basis_status": pa.string(),
    },
    "universe_intervals.parquet": {
        "entity": pa.string(),
        "member_from": pa.date32(),
        "member_to": pa.date32(),
        "basis": pa.string(),
        "flags": pa.string(),
    },
}

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
}

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


def check_snapshot(directory: pathlib.Path) -> None:
    manifest_path = directory / "manifest.json"
    check(manifest_path.is_file(), f"{directory}: manifest.json is missing")
    if not manifest_path.is_file():
        return
    manifest = json.loads(manifest_path.read_text())
    check(
        manifest.get("schema") == "x-data-mcp.research-snapshot.v2",
        f"manifest schema {manifest.get('schema')!r}",
    )
    check(
        directory.name == manifest.get("snapshot_id"),
        f"directory {directory.name} != manifest snapshot_id {manifest.get('snapshot_id')}",
    )
    files = manifest.get("files") or []
    check(len(files) > 0, "manifest lists no file")
    for entry in files:
        name = entry["path"]
        path = directory / name
        check(path.is_file(), f"{name}: listed in the manifest but missing")
        if not path.is_file():
            continue
        expected = CONTRACT.get(name)
        if expected is None:
            check(False, f"{name}: no type contract in this script")
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


def self_check() -> None:
    """Exercise the readers and the §7.4 type contract without Rust."""
    import datetime
    import decimal

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
    import datetime
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
    parser.add_argument("--self-check", action="store_true")
    parser.add_argument("--measure", action="store_true")
    args = parser.parse_args()
    if args.self_check:
        self_check()
    if args.measure:
        measure()
    if args.snapshot_dir:
        check_snapshot(args.snapshot_dir)
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
