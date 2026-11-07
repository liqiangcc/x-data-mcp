#!/usr/bin/env python3
"""Offline extractor for the SSE delisted-ETF on-exchange short name.

D-72 freezes the termination-notice PDFs plus a CSV of extracted dates. PR-4
adds the ETF's 场内简称 to that CSV: the SSE announcement index has no short
name field (取证 §4), while the PDF body carries one (取证 §6). This script is
an **operator tool**, run once on the machine that holds the frozen PDF bytes
(vm2). Production Rust code never parses a PDF; it only reads `short_name`
from the imported CSV.

The script is offline: it reads local files, shells out to `pdfinfo` and
`pdftotext`, and writes a CSV. It never opens a network connection, never
edits the source store in place and never guesses a name. For every row it
emits the evidence columns the Rust importer and the item evidence require:

    doc_sha256             sha256 of the PDF the value came from (D-72 column)
    short_name_page_no     page of the PDF the snippet was found on
    short_name_snippet     verbatim line the value was extracted from
    short_name_extractor   this script's version, e.g. "etf-short-name-v2"

When the PDF is missing, `pdftotext` fails, or no 场内简称 is present, the
three `short_name*` fields are left empty. The name is **not** taken from the
announcement title's fund full name and is never reconstructed from the code
or the fund company: the read surface answers `name: null`,
`name_basis: unavailable` for those ETFs (D-76 Q4).

The v2 rule (XDM-203) is derived from the 82 SSE termination PDFs audited in
XDM-201 §3:

- A name is read only from an explicit `场内简称：` / `场内简称:` label (also
  written `基金场内简称：` in that corpus). `扩位简称`, `场内扩位简称`,
  `基金场内扩位简称`, `基金简称`, `证券简称` and `扩位证券简称` are
  **different fields** and are never used, not even as a fallback.
- The value stops at the first line end, punctuation (full- or half-width
  comma, period, semicolon, parenthesis, bracket, quote, colon, ...) or a
  layout column gap (two or more spaces). A single space inside a name
  (`民企 ETF`, `500 医药`) is kept while matching and removed afterwards,
  matching the search-side normalisation (D-76 Q8). Full-width characters are
  preserved.
- A standalone field line (the label begins its line) wins over a label
  embedded in prose, so a value wrapped across a line (`场内简称：50 等` /
  `权，扩位简称：…`) loses to the field line `场内简称：50 等权`
  (XDM-201 §3.3, sh.510430).
- A capture that still contains a label word (`以下简称`, `本基金`, `扩位`,
  `简称`, `代码`, `证券`, `基金`, ...) or a fund full name is refused. The
  length guard is `MAX_NAME_CHARS` (12), derived from the same corpus: the
  longest real value is 8 characters (`180EWETF`, `180高ETF`) and the
  exchange `fundAbbr` shape is 2-8 characters. It is a sanity bound, not a
  display cut: nothing is truncated, an over-long capture is refused and
  reported.

Pagination (also XDM-203): v1 always probed five pages, so a two-page PDF
produced `pdftotext exited 99: first page (3) can not be after the last
page (2)` for every row whose name was not on page 1. v2 asks `pdfinfo` for
the page count first and never requests a page past it; when `pdfinfo` is
unavailable it probes page by page and stops at the first failure after
page 1 (a failure on page 1 is still a real error).

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

EXTRACTOR_VERSION = "etf-short-name-v2"

# Evidence-derived sanity bound, not a display cut (see the module docstring):
# the 82-PDF audit's longest value is 8 characters and the exchange `fundAbbr`
# shape is 2-8. An over-long capture is refused, never truncated.
MAX_NAME_CHARS = 12

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

# `场内简称：民企 ETF`, `场内简称 永赢中证 500ETF`, `基金场内简称：能源行业`.
# The colon is optional (full- or half-width). The label must not be part of
# `扩位场内简称` / `基金扩位场内简称`: those are the expanded (扩位) name, a
# different field, and matching them would fabricate a name from it.
SHORT_NAME_LABEL_RE = re.compile(
    r"(?<!扩位)(?<!基金扩位)(?:基金)?场内简称\s*[:：]?\s*"
)

# Where a value ends. Full- and half-width punctuation, plus the layout column
# gap handled separately in `short_name_candidates`. A single space is *not* a
# terminator: `民企 ETF` and `500 医药` are one name.
VALUE_TERMINATORS = "，,。.；;、（）()【】[]{}<>《》「」『』“”\"'‘’：:！!？?…"

# A capture that still contains one of these is not a short name: it is prose
# or another field's label. Requiring the explicit `场内简称` label is the
# primary guard; this second check rejects a capture the terminator set did
# not cut (XDM-201 §3.3).
REJECT_MARKERS = (
    "以下简称",
    "本基金",
    "扩位",
    "简称",
    "代码",
    "证券",
    "基金",
    "交易型开放式",
    "投资基金",
)


def clean_short_name(raw: str) -> str | None:
    """Whitespace-normalise one captured 场内简称, or refuse it.

    ETF short names often contain spaces between Chinese and ASCII
    (`民企 ETF`, `永赢中证 500ETF`). They are removed for the stored name,
    matching the search-side normalisation (D-76 Q8). A capture that still
    contains prose or another field's label, is empty, exceeds
    `MAX_NAME_CHARS`, or has no ASCII/CJK content is refused. Valid full-width
    characters are kept as they are.
    """
    text = raw.strip()
    if not text:
        return None
    # Cut a trailing date/annotation that `pdftotext -layout` sometimes puts
    # on the same line, e.g. `场内简称：民企 ETF    2020-11-13`.
    text = re.split(r"\s{2,}", text, maxsplit=1)[0].strip()
    compact = re.sub(r"\s+", "", text)
    if not compact:
        return None
    if any(marker in compact for marker in REJECT_MARKERS):
        return None
    if len(compact) > MAX_NAME_CHARS:
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


def short_name_candidates(text: str) -> list[tuple[str, str, int]]:
    """All usable `(name, line, tier)` candidates on one page of text.

    `tier` orders the evidence shapes seen in XDM-201 §3, lowest wins:

        0  standalone field line, value ended by punctuation
        1  standalone field line, value ended by the line end (may be wrapped)
        2  label inside prose, value ended by punctuation
        3  label inside prose, value ended by the line end

    A standalone field line beats prose because a prose `场内简称：50 等` is
    often wrapped and its continuation (`权，扩位简称：…`) is another field
    (sh.510430). Within one tier the longest value wins.
    """
    candidates: list[tuple[str, str, int]] = []
    for match in SHORT_NAME_LABEL_RE.finditer(text):
        line_start = text.rfind("\n", 0, match.start()) + 1
        standalone = not text[line_start : match.start()].strip()
        rest = text[match.end() :]
        stop = len(rest)
        ended_by_punctuation = False
        for offset, char in enumerate(rest):
            if char == "\n" or char in VALUE_TERMINATORS:
                stop = offset
                ended_by_punctuation = char != "\n"
                break
        raw = rest[:stop]
        # `pdftotext -layout` separates table columns with two or more spaces.
        raw = re.split(r"\s{2,}", raw, maxsplit=1)[0]
        name = clean_short_name(raw)
        if name is None:
            continue
        line_end = text.find("\n", match.end())
        if line_end < 0:
            line_end = len(text)
        line = text[line_start:line_end].strip()
        tier = (0 if standalone else 2) + (0 if ended_by_punctuation else 1)
        candidates.append((name, line, tier))
    return candidates


def extract_from_text(text: str) -> tuple[str, str] | None:
    """Return `(short_name, verbatim line)` from one page of text."""
    candidates = short_name_candidates(text)
    if not candidates:
        return None
    name, line, _tier = min(candidates, key=lambda item: (item[2], -len(item[0])))
    return name, line


def page_count(pdf: Path) -> int | None:
    """PDF page count from `pdfinfo`, or None when it cannot be read."""
    try:
        result = subprocess.run(
            ["pdfinfo", str(pdf)], capture_output=True, check=False
        )
    except OSError:
        return None
    if result.returncode != 0:
        return None
    for line in result.stdout.decode("utf-8", "replace").splitlines():
        key, _, value = line.partition(":")
        if key.strip() != "Pages":
            continue
        try:
            count = int(value.strip())
        except ValueError:
            return None
        return count if count > 0 else None
    return None


def extract_pdf(pdf: Path, sha: str, max_pages: int = 5) -> dict[str, str]:
    """Extract one PDF's short name. Never raises for a missing value.

    The scan is bounded by `pdfinfo`'s page count, so a two-page PDF gets two
    `pdftotext` calls and no `exited 99` error (XDM-201 §9). Without `pdfinfo`
    the probe stops at the first failure after page 1; a failure on page 1 is a
    real read error and is raised.
    """
    empty = {key: "" for key in ADDED}
    # Verify the bytes still match the D-72 doc_sha256: a swapped PDF would
    # silently attach the wrong fund's name to this row.
    digest = hashlib.sha256(pdf.read_bytes()).hexdigest()
    if digest.lower() != sha.lower():
        raise RuntimeError(f"PDF sha256 is {digest}, CSV names {sha}")
    count = page_count(pdf)
    last_page = min(max_pages, count) if count is not None else max_pages
    for page in range(1, last_page + 1):
        try:
            text = page_text(pdf, page)
        except RuntimeError:
            if count is not None or page == 1:
                raise
            # No page count and `pdftotext` failed past page 1: the only
            # honest reading is that page `page` is past the last page.
            break
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
