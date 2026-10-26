#!/usr/bin/env python3
"""Python `fractions.Fraction` reference for the `qfq-exact-v1` chain.

Issue-383 §4.3 / M1 PR-3: the forward-adjustment chain is composed over exact
rationals and rounded **once** at the target scale, so a 6dp value depends only
on (bar, events, scale, algorithm version) — never on intermediate precision.
This script is the language-independent oracle for that claim:

* it recomputes every expected value in `expected.json` with `Fraction`
  (`--check`), or regenerates the file (`--write`);
* the Rust implementation (`src/gbbq.rs` exact chain + `src/qfq.rs`
  `forward_adjust_exact`) is asserted bit-for-bit against the same file.

Inputs are committed evidence only — the three `.day` members under
`evidence/issue-166-windows-official-behavior/raw/files/` and the 94-event
matrix in `evidence/issue-54-gbbq-full-events/result.json` — plus the
synthetic `cases` in `expected.json` itself (their inputs and outputs live in
one place, so the Rust test feeds exactly the same numbers).

Not an implementation candidate: pure-stdlib, offline, O(bars × events).
"""

import argparse
import hashlib
import json
import struct
import sys
from decimal import Decimal, ROUND_HALF_UP
from fractions import Fraction
from math import gcd
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
DAY_DIR = ROOT / "evidence" / "issue-166-windows-official-behavior" / "raw" / "files"
EVENTS_JSON = ROOT / "evidence" / "issue-54-gbbq-full-events" / "result.json"
EXPECTED = HERE / "expected.json"
SCHEMA = "x-data-qfq-exact-reference.v1"

# The frozen #54 corpus: every security with both a committed `.day` member and
# decoded events. `sz.000004` has events but no committed member.
CORPUS = ("sh.600004", "sh.600006", "sz.000002")
DAY_FILE = {
    "sh.600004": "sh600004.day",
    "sh.600006": "sh600006.day",
    "sz.000002": "sz000002.day",
}


def normalize_factor_2dp(raw_f32):
    """`gbbq::normalize_factor`: f32 → shortest f64 decimal → 2dp HALF_UP."""
    return Decimal(repr(float(raw_f32))).quantize(Decimal("0.01"), rounding=ROUND_HALF_UP)


def factor_units_2dp(raw_f32):
    """The 2dp factor as an integer number of 1/100 units (C1..C4 × 100)."""
    return int(normalize_factor_2dp(raw_f32) * 100)


def day_bars(path):
    """Parse a `.day` member: (yyyymmdd, open, high, low, close) per record."""
    data = path.read_bytes()
    if len(data) % 32:
        raise ValueError(f"{path}: {len(data)} bytes is not a whole number of 32-byte records")
    bars = []
    for offset in range(0, len(data), 32):
        date, o, h, lo, c, _amount, _volume, _r7 = struct.unpack(
            "<IIIIIfII", data[offset : offset + 32]
        )
        bars.append((date, o, h, lo, c))
    return bars


def corpus_events():
    """category==1 events per security from the committed 94-event matrix."""
    document = json.loads(EVENTS_JSON.read_text())
    out = {}
    for entity, matrix in document["four_security_matrices"].items():
        events = []
        for row in matrix["rows"]:
            clear = bytes.fromhex(row["clear_hex"])
            date = struct.unpack("<I", clear[8:12])[0]
            category = clear[12]
            c = factor_units_2dp_pair(clear[13:])
            events.append((date, category, c))
        out[entity] = events
    return out


def factor_units_2dp_pair(raw_four):
    values = struct.unpack("<ffff", raw_four)
    return tuple(factor_units_2dp(v) for v in values)


def reduce_triple(pa, pb, den):
    """Keep the exact chain small: divide the common denominator out."""
    common = gcd(gcd(abs(pa), abs(pb)), den)
    if common > 1:
        return pa // common, pb // common, den // common
    return pa, pb, den


def compose_chain(events):
    """Exact affine chain T(x) = (pa*x + 100*pb) / (100*den) over events.

    Per-event `T_i(x) = (x - C1/10 + C4*C2/10) / ((10+C3+C4)/10)`, expressed
    with the 2dp integer factors `n_i = C_i * 100` as
    `a = 100000 / (100*(1000+n3+n4))`, `b = (n2*n4 - 100*n1) / (100*(1000+n3+n4))`.
    """
    pa, pb, den = 1, 0, 1
    for _date, _category, (n1, n2, n3, n4) in events:
        b_den = 1000 + n3 + n4
        if b_den == 0:
            raise ValueError("degenerate event denominator")
        event_den = 100 * b_den
        event_num_b = n2 * n4 - 100 * n1
        pa, pb, den = (
            100000 * pa,
            100000 * pb + event_num_b * den,
            event_den * den,
        )
        pa, pb, den = reduce_triple(pa, pb, den)
    return pa, pb, den


def apply_chain(chain, price_cents):
    """Exact adjusted value in yuan for one raw cent price."""
    pa, pb, den = chain
    return Fraction(pa * price_cents + 100 * pb, 100 * den)


def round_units(value, scale):
    """Round once, HALF_UP away from zero, to `scale` decimals (integer units)."""
    scaled = value * 10**scale
    numerator, denominator = scaled.numerator, scaled.denominator
    quotient, remainder = divmod(abs(numerator), denominator)
    if 2 * remainder >= denominator:
        quotient += 1
    return -quotient if numerator < 0 else quotient


def is_half_cent(value):
    """True when the exact value sits exactly on a 2dp half-cent boundary."""
    return (value * 200).denominator == 1 and (value * 200).numerator % 2 != 0


def chain_for(events_sorted, bar_date):
    """Suffix of events with `event_date > bar_date` (the frozen #54 rule)."""
    start = 0
    for index, (date, _category, _c) in enumerate(events_sorted):
        if date <= bar_date:
            start = index + 1
        else:
            break
    return start


def adjust(bars, events, scale):
    """Forward-adjust a bar list; returns [(date, units, chain_event_count)].

    Equal event dates keep their input order (`sort_by_key` is stable in Rust,
    `sorted(key=…)` is stable here), so a same-day event pair composes in the
    order the source lists it — the frozen #54 rule, no date-only reordering.
    """
    ordered = sorted((e for e in events if e[1] == 1), key=lambda e: e[0])
    chains = {}
    out = []
    for date, o, h, lo, c in bars:
        start = chain_for(ordered, date)
        if start not in chains:
            chains[start] = compose_chain(ordered[start:])
        chain = chains[start]
        units = [round_units(apply_chain(chain, cents), scale) for cents in (o, h, lo, c)]
        out.append((date, units, len(ordered) - start))
    return out


def corpus_digest(results_by_entity):
    """Canonical corpus digest — the same line format the Rust test builds."""
    digest = hashlib.sha256()
    for entity in sorted(results_by_entity):
        for date, units, count in results_by_entity[entity]:
            digest.update(
                f"{entity}\t{date}\t{units[0]}\t{units[1]}\t{units[2]}\t{units[3]}\t{count}\n".encode()
            )
    return digest.hexdigest()


def compute():
    """Recompute everything `expected.json` asserts."""
    events_by_entity = corpus_events()
    results = {}
    summary = []
    tie_2dp = 0
    tie_6dp = 0
    fields = 0
    for entity in CORPUS:
        bars = day_bars(DAY_DIR / DAY_FILE[entity])
        events = [e for e in events_by_entity[entity] if e[1] == 1]
        results[entity] = adjust(bars, events, 6)
        ordered = sorted(events, key=lambda e: e[0])
        chains = {}
        for date, o, h, lo, c in bars:
            start = chain_for(ordered, date)
            if start not in chains:
                chains[start] = compose_chain(ordered[start:])
            chain = chains[start]
            for cents in (o, h, lo, c):
                value = apply_chain(chain, cents)
                fields += 1
                if is_half_cent(value):
                    tie_2dp += 1
                if (value * 10**6).denominator == 1 and (value * 10**6).numerator % 2 != 0:
                    tie_6dp += 1
        summary.append(
            {
                "entity": entity,
                "day_file": str((DAY_DIR / DAY_FILE[entity]).relative_to(ROOT)),
                "bars": len(bars),
                "events": len(events),
            }
        )
    return {
        "corpus": {
            "digest_format": (
                "sha256 over '<entity>\\t<yyyymmdd>\\t<open6>\\t<high6>\\t<low6>"
                "\\t<close6>\\t<chain_events>\\n' lines, entities ascending, bars in "
                "member order; <*6> are 6dp integer units"
            ),
            "digest": corpus_digest(results),
            "securities": summary,
            "totals": {
                "bars": sum(s["bars"] for s in summary),
                "price_fields": fields,
                "events": sum(s["events"] for s in summary),
            },
        },
        "tie_counts": {"scale_2": tie_2dp, "scale_6": tie_6dp},
    }


def case_results(case):
    """Recompute one synthetic case from its own inputs."""
    events = [(e["date"], 1, tuple(e["c_2dp"])) for e in case["events"]]
    bars = [(b["date"], *b["ohlc_cents"]) for b in case["bars"]]
    return adjust(bars, events, case["scale"])


def check(document):
    """Verify every expected value; return a list of failure strings."""
    problems = []
    computed = compute()
    if document["corpus"]["digest"] != computed["corpus"]["digest"]:
        problems.append(
            f"corpus digest {document['corpus']['digest']} != {computed['corpus']['digest']}"
        )
    for key in ("securities", "totals"):
        if document["corpus"][key] != computed["corpus"][key]:
            problems.append(f"corpus {key} {document['corpus'][key]} != {computed['corpus'][key]}")
    if document["tie_counts"] != computed["tie_counts"]:
        problems.append(f"tie_counts {document['tie_counts']} != {computed['tie_counts']}")
    for case in document["cases"]:
        got = case_results(case)
        want = [(b["date"], b["units"], b["chain_event_count"]) for b in case["expected"]]
        if got != want:
            for index, (a, b) in enumerate(zip(got, want)):
                if a != b:
                    problems.append(f"case {case['name']} bar {index}: {a} != {b}")
                    break
            if len(got) != len(want):
                problems.append(f"case {case['name']}: {len(got)} bars != {len(want)}")
    return problems


def write(document):
    computed = compute()
    document["corpus"] = computed["corpus"]
    document["tie_counts"] = computed["tie_counts"]
    for case in document["cases"]:
        case["expected"] = [
            {"date": date, "units": units, "chain_event_count": count}
            for date, units, count in case_results(case)
        ]
    EXPECTED.write_text(json.dumps(document, indent=2, ensure_ascii=False) + "\n")
    return document


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="verify expected.json")
    parser.add_argument("--write", action="store_true", help="regenerate expected.json")
    args = parser.parse_args(argv)
    document = json.loads(EXPECTED.read_text())
    if document["schema"] != SCHEMA:
        print(f"unexpected schema {document['schema']}", file=sys.stderr)
        return 2
    if args.write:
        write(document)
        print(f"wrote {EXPECTED.relative_to(ROOT)}")
        return 0
    problems = check(document)
    for problem in problems:
        print(problem, file=sys.stderr)
    if problems:
        return 1
    totals = document["corpus"]["totals"]
    print(
        "qfq-exact reference OK: "
        f"{len(document['corpus']['securities'])} securities, {totals['bars']} bars, "
        f"{totals['price_fields']} price fields, {totals['events']} events, "
        f"{len(document['cases'])} cases, "
        f"2dp ties {document['tie_counts']['scale_2']}, "
        f"6dp ties {document['tie_counts']['scale_6']}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
