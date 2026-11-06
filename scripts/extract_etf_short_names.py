#!/usr/bin/env python3
"""Offline extractor for the SSE delisted-ETF on-exchange short name.

D-72 freezes the termination-notice PDFs plus a CSV of extracted dates. PR-4
adds the ETF's 场内简称 to that CSV: the SSE announcement index has no short
name field (取证 §4), while the PDF body carries one (取证 §6). This script is
an **operator tool**, run once on the machine that holds the frozen PDF bytes
(vm2). Production Rust code never parses a PDF; it only reads `short_name`
from the imported CSV.

The script is offline: it reads local files, shells out to `pdftotext`, and
writes a CSV. It never opens a network connection, never edits the source
store in place and never guesses a name. For every row it emits the evidence
columns the Rust importer and the item evidence require:

    doc_sha256             sha256 of the PDF the value came from (D-72 column)
    short_name_page_no     page of the PDF the snippet was found on
    short_name_snippet     verbatim text the value was extracted from
    short_name_extractor   this script's version, e.g. "etf-short-name-v1"

When the PDF is missing, `pdftotext` fails, or no 场内简称 is present, the
three `short_name*` fields are left empty. The name is **not** taken from the
announcement title's fund full name and is never reconstructed from the code
or the fund company: the read surface answers `name: null`,
`name_basis: unavailable` for those ETFs (D-76 Q4).

Usage
-----
    python3 scripts/extract_etf_short_names.py \
        --csv  /path/to/etf_delist_extract_v1.csv \
        --pdfs /path/to/pdf_blobs \
        --out  /path/to/etf_delist_extract_v1_with_names.csv

`--pdfs` is the directory of blobs named `<doc_sha256>.pdf` (the source
store's blob layout) or the PDF's URL basename. See
`docs/tasks/etf-name-search.md` §9 for the operator steps and the import
command that follows.

Exit status is 0 only when the output CSV was written; a missing input CSV or
a CSV without the D-72 columns is an error (2). Missing PDFs and unextractable
names are reported on stderr and counted in the final summary, not fatal:
the point is to record the residual gap honestly.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import re
import subprocess
import sys
from pathlib import Path

EXTRACTOR_VERSION = "etf-short-name-v1"

# Columns the D-72 importer requires. `short_name` and friends are added by
# this script; an input that already has them is re-derived, not appended
# twice.
D72_REQUIRED = (
    "entity",
    "delisting_date",
    "announcement_date",
    "source_id",
    "doc_url",
    "doc_sha256",
    "page_no",
    "snippet",
    "extractor",
    "extracted_at",
    "confidence",
)
ADDED = (
    "short_name",
    "short_name_page_no",
    "short_name_snippet",
    "short_name_extractor",
)
SSE_SOURCE_ID = "sse_fund_delist_ann"

# The PDF samples in 取证 §6: `场内简称：民企 ETF`, `场内简称 永赢中证
# 500ETF`, with an optional colon (full- or half-width) and arbitrary spaces.
# The capture is deliberately bounded and stops at the line end. 基金简称 is a
# different, longer name and is never used as a fallback.
SHORT_NAME_RE = re.compile(r"场内简称\s*[:：]?\s*([^\r\n]{1,40})")

# A line that is only a fund's full name (a substitution we must not make) or
# a page header. Requiring the label above is the primary guard; this second
# check rejects captures that are clearly not an on-exchange short name.
FULL_NAME_MARKERS = ("证券投资基金", "交易型开放式指数", "基金简称")


def clean_short_name(raw: str) -> str | None:
    """Whitespace-normalise one captured 场内简称, or refuse it.

    ETF short names often contain spaces between Chinese and ASCII
    (`民企 ETF`, `永赢中证 500ETF`). They are removed for the stored name,
    matching the search-side normalisation (D-76 Q8). A capture that is a fund
    full name, is empty, or has no ASCII/CJK content is refused.
    """
    text = raw.strip()
    if not text:
        return None
    if any(marker in text for marker in FULL_NAME_MARKERS):
        return None
    # Cut a trailing date/annotation that `pdftotext -layout` sometimes puts
    # on the same line, e.g. `场内简称：民企 ETF    2020-11-13`.
    text = re.split(r"\s{2,}", text, maxsplit=1)[0].strip()
    compact = re.sub(r"\s+", "", text)
    if not compact or len(compact) > 20:
        return None
    if not re.search(r"[\w\u4e00-\u9fff]", compact):
        return None
    return compact


def find_pdf(pdfs: Path, url: str, sha: str) -> Path | None:
    """Locate a PDF by blob sha, then by URL basename (D-72 importer rule)."""
    for name in (f"{sha}.pdf", f"{sha}.PDF", Path(url).name):
        if not name:
            continue
        candidate = pdfs / name
        if candidate.is_file():
            return candidate
    return None


def page_text(pdf: Path, page: int) -> str:
    """`pdftotext -layout -f <page> -l <page>` for one page, UTF-8."""
    result = subprocess.run(
        [
            "pdftotext",
            "-layout",
            "-f",
            str(page),
            "-l",
            str(page),
            str(pdf),
            "-",
        ],
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"pdftotext exited {result.returncode}: "
            f"{result.stderr.decode('utf-8', 'replace').strip()[:200]}"
        )
    return result.stdout.decode("utf-8", "replace")


def extract_from_text(text: str) -> tuple[str, str] | None:
    """Return `(short_name, verbatim snippet)` from one page of text."""
    for match in SHORT_NAME_RE.finditer(text):
        line = match.group(0).strip()
        name = clean_short_name(match.group(1))
        if name is not None:
            return name, line
    return None


def extract_pdf(pdf: Path, sha: str, max_pages: int = 5) -> dict[str, str]:
    """Extract one PDF's short name. Never raises for a missing value."""
    empty = {key: "" for key in ADDED}
    # Verify the bytes still match the D-72 doc_sha256: a swapped PDF would
    # silently attach the wrong fund's name to this row.
    digest = hashlib.sha256(pdf.read_bytes()).hexdigest()
    if digest.lower() != sha.lower():
        raise RuntimeError(f"PDF sha256 is {digest}, CSV names {sha}")
    for page in range(1, max_pages + 1):
        text = page_text(pdf, page)
        if not text.strip():
            continue
        found = extract_from_text(text)
        if found is not None:
            name, snippet = found
            return {
                "short_name": name,
                "short_name_page_no": str(page),
                "short_name_snippet": snippet,
                "short_name_extractor": EXTRACTOR_VERSION,
            }
    return empty


def process(csv_in: Path, pdfs: Path, csv_out: Path) -> dict[str, int]:
    with csv_in.open("r", encoding="utf-8", newline="") as handle:
        reader = csv.DictReader(handle)
        header = list(reader.fieldnames or [])
        missing = [name for name in D72_REQUIRED if name not in header]
        if missing:
            raise ValueError(f"input CSV has no column(s): {', '.join(missing)}")
        rows = list(reader)
    out_header = [name for name in header if name not in ADDED] + list(ADDED)
    stats = {"rows": len(rows), "sse": 0, "named": 0, "missing_pdf": 0, "unavailable": 0}
    for row in rows:
        for key in ADDED:
            row[key] = ""
        if row.get("source_id", "").strip() != SSE_SOURCE_ID:
            continue
        stats["sse"] += 1
        sha = row.get("doc_sha256", "").strip()
        pdf = find_pdf(pdfs, row.get("doc_url", "").strip(), sha)
        if pdf is None:
            stats["missing_pdf"] += 1
            print(f"{row.get('entity', '?')}: no PDF for {sha}", file=sys.stderr)
            continue
        try:
            found = extract_pdf(pdf, sha)
        except (OSError, RuntimeError) as error:
            print(f"{row.get('entity', '?')}: {error}", file=sys.stderr)
            found = {key: "" for key in ADDED}
        row.update(found)
        if row["short_name"]:
            stats["named"] += 1
            print(f"{row.get('entity', '?')}: {row['short_name']}")
        else:
            stats["unavailable"] += 1
            print(
                f"{row.get('entity', '?')}: no 场内简称 in PDF; short_name left empty",
                file=sys.stderr,
            )
    csv_out.parent.mkdir(parents=True, exist_ok=True)
    with csv_out.open("w", encoding="utf-8", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=out_header, extrasaction="ignore")
        writer.writeheader()
        writer.writerows(rows)
    return stats


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--csv", required=True, type=Path, help="D-72 extraction CSV")
    parser.add_argument("--pdfs", required=True, type=Path, help="directory of frozen PDF blobs")
    parser.add_argument("--out", required=True, type=Path, help="output CSV with short_name")
    args = parser.parse_args(argv)
    if not args.csv.is_file():
        print(f"input CSV not found: {args.csv}", file=sys.stderr)
        return 2
    if not args.pdfs.is_dir():
        print(f"PDF directory not found: {args.pdfs}", file=sys.stderr)
        return 2
    try:
        stats = process(args.csv, args.pdfs, args.out)
    except (ValueError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    print(
        "rows={rows} sse={sse} named={named} unavailable={unavailable} "
        "missing_pdf={missing_pdf} -> {out}".format(out=args.out, **stats),
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
