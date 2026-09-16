#!/usr/bin/env python3
"""issue-54-qfq-contract-regression scanner.

Regresses the accepted #167 official-client contract against the full frozen
#159 event domain (94 events, four securities) + retained gen-2 .day members.
All arithmetic in Decimal (prec 50). No tuning, no candidates.
"""
import argparse, hashlib, io, json, struct, sys, zipfile
from decimal import Decimal, ROUND_HALF_UP, getcontext
from pathlib import Path

getcontext().prec = 50
HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent / "evidence"
RETAINED = Path("/home/box/retained-inputs")

CENT = Decimal("0.01")
TEN = Decimal(10)
SEC_MEMBER = {
    "sh.600004": "sh\\lday\\sh600004.day",
    "sh.600006": "sh\\lday\\sh600006.day",
    "sz.000002": "sz\\lday\\sz000002.day",
    "sz.000004": "sz\\lday\\sz000004.day",
}
BS_PROBE_SEC = {
    "sh.600004": "sh.600004.fixed_history",
    "sh.600006": "sh.600006.fixed_history",
    "sz.000002": "sz.000002.fixed_history",
    "sz.000004": "sz.000004.fixed_history",
}


def sha256(b): return hashlib.sha256(b).hexdigest()


def pin_bytes(b, expected, label):
    if sha256(b) != expected:
        sys.exit(f"PIN MISMATCH {label}")
    return b


def pin_file(path, expected):
    return pin_bytes(Path(path).read_bytes(), expected, str(path))


def q2(x): return x.quantize(CENT, rounding=ROUND_HALF_UP)


def norm_c(s):
    """2dp business-decimal normalization of an exact-binary f32 string."""
    return Decimal(str(float(Decimal(s)))).quantize(CENT, rounding=ROUND_HALF_UP)


def day_records(blob):
    """32B .day records -> {date_int: {O,H,L,C(Decimal),amount,vol}}."""
    out = {}
    for i in range(len(blob) // 32):
        r = blob[i * 32:(i + 1) * 32]
        dt = struct.unpack("<I", r[:4])[0]
        o, h, l, c = (Decimal(v) / 100 for v in struct.unpack("<IIII", r[4:20]))
        amt, vol = struct.unpack("<fI", r[20:28])
        out[dt] = {"O": o, "H": h, "L": l, "C": c, "amount": amt, "vol": vol}
    return out


def event_norm_fields(row):
    """Normalized C1..C4 (Decimal) from the frozen exact_binary strings."""
    return [norm_c(v) for v in row["candidates"]["exact_binary"]["C1_C2_C3_C4"]]


def event_coeffs(c):
    """(d_, a_, s) for T(x) = (x - d_ + a_) / s."""
    c1, c2, c3, c4 = c
    return c1 / TEN, c4 * c2 / TEN, (TEN + c3 + c4) / TEN


def ref_unrounded(prev_close, c):
    d_, a_, s = event_coeffs(c)
    return (prev_close - d_ + a_) / s


def chain(events_norm, bar_date_int):
    """Affine (A,B) over events with event_date > bar_date, oldest->newest."""
    A, B = Decimal(1), Decimal(0)
    for e in events_norm:
        if e["date_int"] <= bar_date_int:
            continue
        d_, a_, s = event_coeffs(e["c"])
        A, B = A / s, (B - d_ + a_) / s
    return A, B


def bs_rows(probe, label):
    for q in probe["queries"]:
        if q["label"] == label:
            return q["query"]["pages"][0]["rows"]
    sys.exit(f"missing bs query {label}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--plan", default=str(HERE / "plan.json"))
    ap.add_argument("--out", default=str(HERE / "result.json"))
    ap.add_argument("--check", action="store_true")
    args = ap.parse_args()

    plan = json.loads(Path(args.plan).read_text())
    pins = plan["inputs"]["committed_files"]

    m159 = json.loads(pin_file(
        ROOT / "issue-54-gbbq-full-events" / "result.json",
        pins["evidence/issue-54-gbbq-full-events/result.json"]["sha256"]))
    obs = json.loads(pin_file(
        ROOT / "issue-166-windows-official-behavior" / "raw" / "observations.json",
        pins["evidence/issue-166-windows-official-behavior/raw/observations.json"]["sha256"]))
    probe = json.loads(pin_file(
        ROOT / "issue-3-cross-stock-qfq" / "baostock-valid-probe.json",
        pins["evidence/issue-3-cross-stock-qfq/baostock-valid-probe.json"]["sha256"]))
    hsj_bytes = pin_file(
        RETAINED / "official-hsjday.zip",
        plan["inputs"]["retained_files"]
        ["/home/box/retained-inputs/official-hsjday.zip"]["sha256"])
    hsj = zipfile.ZipFile(io.BytesIO(hsj_bytes))

    days = {sec: day_records(hsj.read(m)) for sec, m in SEC_MEMBER.items()}

    # ---------- normalized event tables ----------
    events = {}
    for sec, mm in m159["four_security_matrices"].items():
        evs = []
        for row in mm["rows"]:
            evs.append({
                "date": row["date"],
                "date_int": int(row["date"].replace("-", "")),
                "c_raw": row["candidates"]["exact_binary"]["C1_C2_C3_C4"],
                "c": event_norm_fields(row),
                "row": row,
            })
        evs.sort(key=lambda e: e["date_int"])
        events[sec] = evs

    # ---------- consistency: matrix vs re-decoded .day ----------
    consistency = {"matrix_prev_close_mismatches": [],
                   "matrix_event_bar_mismatches": [],
                   "day_member_vs_166": {}}
    for sec, evs in events.items():
        d = days[sec]
        for e in evs:
            row = e["row"]
            pdate = row.get("previous_date")
            if pdate:
                pd = int(pdate.replace("-", ""))
                rec = d.get(pd)
                if rec is None or rec["C"] != Decimal(str(row["previous_close"])):
                    consistency["matrix_prev_close_mismatches"].append(
                        {"sec": sec, "date": e["date"], "prev_date": pdate,
                         "matrix": row.get("previous_close"),
                         "day": str(rec["C"]) if rec else None})
            eb = row.get("event_day_bar")
            if eb:
                rec = d.get(e["date_int"])
                if rec is None or rec["O"] != Decimal(eb["open"]) or \
                        rec["C"] != Decimal(eb["close"]):
                    consistency["matrix_event_bar_mismatches"].append(
                        {"sec": sec, "date": e["date"]})
    for sec, f166 in (("sh.600004", "sh600004.day"),
                      ("sh.600006", "sh600006.day"),
                      ("sz.000002", "sz000002.day")):
        p166 = ROOT / "issue-166-windows-official-behavior" / "raw" / "files" / f166
        b166 = pin_file(p166, pins[
            "evidence/issue-166-windows-official-behavior/raw/files/"
            + f166]["sha256"])
        bgen = hsj.read(SEC_MEMBER[sec])
        n = min(len(b166), len(bgen))
        first_diff = next((i for i in range(n) if b166[i] != bgen[i]), None)
        consistency["day_member_vs_166"][sec] = {
            "size_166": len(b166), "size_gen2": len(bgen),
            "common_prefix_identical": first_diff is None,
            "first_diff_offset": first_diff}

    # ---------- oracle: BaoStock rows ----------
    bs = {}
    for sec, label in BS_PROBE_SEC.items():
        rows = bs_rows(probe, label)
        by_date = {}
        for r in rows:
            by_date[r[0]] = {
                "O": Decimal(r[2]), "H": Decimal(r[3]), "L": Decimal(r[4]),
                "C": Decimal(r[5]), "preclose": Decimal(r[6]),
                "volume": r[7], "amount": r[8]}
        bs[sec] = by_date

    # ---------- BaoStock model extraction (observed-only) ----------
    # For each security, de-chain observed event-day preclose values
    # (newest event first) into per-event reference/factor in BaoStock's
    # multiplicative-cent model. All inputs are observed quantities:
    #   f_j = ref_j / prev_close_raw(j)
    #   ref_j = preclose_BS(d_j) / prod_{k>j} f_k
    bs_model = {}
    for sec, evs in events.items():
        in_ev = [e for e in evs
                 if "2025-01-01" <= e["date"] <= "2026-09-10"
                 and e["date"] in bs[sec] and e["row"]["event_date_in_day"]]
        prod = Decimal(1)
        refs = {}
        for e in reversed(in_ev):
            pc = bs[sec][e["date"]]["preclose"]
            ref = pc / prod
            pj = Decimal(str(e["row"]["previous_close"]))
            f = ref / pj
            refs[e["date"]] = {"ref": ref, "factor": f,
                               "preclose_observed": pc,
                               "prev_close_raw": pj}
            prod *= f
        bs_model[sec] = refs

    # ---------- per-event reference regression ----------
    per_event = []
    for sec, evs in events.items():
        for e in evs:
            row = e["row"]
            entry = {
                "security": sec, "date": e["date"],
                "c_raw": e["c_raw"],
                "c_normalized": [str(c) for c in e["c"]],
                "prev_date": row.get("previous_date"),
                "prev_close": str(row.get("previous_close")),
                "event_date_in_day": row["event_date_in_day"],
            }
            pc = Decimal(str(row["previous_close"]))
            ref = ref_unrounded(pc, e["c"])
            entry["ref"] = str(ref)
            entry["ref_cent"] = str(q2(ref))
            entry["ref_cent_equals_prev_close"] = q2(ref) == pc

            oracle = {}
            checks = []
            if e["date"] in bs_model[sec]:
                bm = bs_model[sec][e["date"]]
                observed = q2(bm["ref"])
                oracle["baostock_preclose_dechained"] = {
                    "observed": str(bm["ref"]),
                    "observed_cent": str(observed),
                    "preclose_observed": str(bm["preclose_observed"])}
                checks.append(observed == q2(ref))
            # #167 client-transcribed event-day references (hard oracles)
            obs166 = {"sh.600006": {"2025-07-17": Decimal("7.25")},
                      "sh.600004": {"2026-07-24": Decimal("7.68")}}
            if sec in obs166 and e["date"] in obs166[sec]:
                exp = obs166[sec][e["date"]]
                oracle["client_transcription_166"] = str(exp)
                checks.append(q2(ref) == exp)
            if checks:
                status = ("MATCH" if all(checks)
                          else "MISMATCH_UNEXPLAINED")
            elif not row["event_date_in_day"]:
                status = "NO_EVENT_DAY_BAR"
                oracle["reason"] = "no_event_day_bar"
            else:
                status = "NO_ORACLE"
                oracle["reason"] = "outside_frozen_window"
            entry["status"] = status
            entry["oracle"] = oracle
            per_event.append(entry)

    # ---------- chain reproduction vs #167 transcribed bars ----------
    client_bars = []
    obs_map = [
        ("sh.600006", "2025-07-16"), ("sh.600006", "2025-07-17"),
        ("sh.600004", "2026-07-23"), ("sh.600004", "2026-07-24"),
        ("sz.000002", "2000-01-10"), ("sz.000002", "1999-12-29"),
    ]
    for sec, ds in obs_map:
        target = None
        for r in obs["phase_a"][sec]["kline_readouts"]:
            if r["date"] == ds and "前复权" in r["mode"]:
                target = r
        if target is None:
            sys.exit(f"missing 166 readout {sec} {ds}")
        di = int(ds.replace("-", ""))
        raw = days[sec][di]
        A, B = chain(events[sec], di)
        fields = {}
        for f in "OHLC":
            pred = q2(A * raw[f] + B)
            fields[f] = {"pred": str(pred), "observed": str(Decimal(str(target[f]))),
                         "match": pred == Decimal(str(target[f]))}
        client_bars.append({
            "security": sec, "date": ds,
            "n_events_in_chain": sum(1 for e in events[sec] if e["date_int"] > di),
            "fields": fields,
            "all_match": all(v["match"] for v in fields.values())})

    # ---------- chain reproduction vs frozen BaoStock window ----------
    bs_window = {}
    for sec in SEC_MEMBER:
        d = days[sec]
        evs = events[sec]
        rows_out = {"dates": 0, "fields": 0, "exact_10dp": 0,
                    "display_match_2dp": 0, "model_divergence": 0,
                    "suspension_placeholders": 0,
                    "day_dates_without_bs_row": 0,
                    "divergence_samples": [], "unexplained_items": []}
        for date, brow in sorted(bs[sec].items()):
            di = int(date.replace("-", ""))
            if di not in d:
                if (brow["O"] == brow["H"] == brow["L"] == brow["C"]
                        == brow["preclose"] and brow["volume"] == ""
                        and brow["amount"] == ""):
                    rows_out["suspension_placeholders"] += 1
                else:
                    rows_out["unexplained_items"].append(
                        {"date": date, "reason": "baostock_row_without_day_bar"})
                continue
            rec = d[di]
            A, B = chain(evs, di)
            # BaoStock multiplicative-cent model self-consistency for this
            # date: expected factor = prod f_j over in-window events j>d
            # (BS quantizes per-event factors at ~6dp, so tolerance 1e-6).
            exp_f = Decimal(1)
            for j, bm in bs_model[sec].items():
                if int(j.replace("-", "")) > di:
                    exp_f *= bm["factor"]
            bs_self_consistent = all(
                rec[f] != 0 and
                abs(brow[f] / rec[f] - exp_f) < Decimal("1e-6")
                for f in "OHLC")
            for f in "OHLC":
                rows_out["fields"] += 1
                pred = A * rec[f] + B
                src = brow[f]
                if abs(pred - src) < Decimal("5e-11"):
                    rows_out["exact_10dp"] += 1
                elif q2(pred) == q2(src):
                    rows_out["display_match_2dp"] += 1
                elif bs_self_consistent:
                    rows_out["model_divergence"] += 1
                    if len(rows_out["divergence_samples"]) < 10:
                        rows_out["divergence_samples"].append({
                            "date": date, "field": f,
                            "pred": str(pred), "source": str(src)})
                else:
                    rows_out["unexplained_items"].append(
                        {"date": date, "field": f, "pred": str(pred),
                         "source": str(src)})
            rows_out["dates"] += 1
        rows_out["day_dates_without_bs_row"] = sum(
            1 for di in d
            if 20250102 <= di <= 20260910
            and f"{str(di)[:4]}-{str(di)[4:6]}-{str(di)[6:]}" not in bs[sec])
        bs_window[sec] = rows_out

    # ---------- C3 report ----------
    c3_events = []
    for sec, evs in events.items():
        for e in evs:
            if e["c"][2] == 0:
                continue
            # exercised iff some oracle bar (client transcribed or BS window
            # bar) has date < event date
            ex_client = any(di < e["date_int"] for s2, ds in obs_map
                            if s2 == sec
                            for di in [int(ds.replace("-", ""))])
            ex_bs = any(int(dt.replace("-", "")) < e["date_int"]
                      for dt in bs[sec])
            c3_events.append({"security": sec, "date": e["date"],
                              "c_normalized": [str(c) for c in e["c"]],
                              "exercised_by_client_oracle": ex_client,
                              "exercised_by_baostock_window": ex_bs})

    statuses = {}
    for e in per_event:
        statuses[e["status"]] = statuses.get(e["status"], 0) + 1

    result = {
        "plan_id": plan["plan_id"],
        "contract": plan["contract_under_test"],
        "events_total": len(per_event),
        "per_event_status_counts": statuses,
        "per_event": per_event,
        "client_oracle_bars": client_bars,
        "client_oracle_all_match": all(b["all_match"] for b in client_bars),
        "baostock_window": bs_window,
        "c3_nonzero_events": c3_events,
        "c3_nonzero_count": len(c3_events),
        "consistency": consistency,
        "unexplained_total": (
            statuses.get("MISMATCH_UNEXPLAINED", 0)
            + sum(len(w["unexplained_items"]) for w in bs_window.values())),
    }
    out = json.dumps(result, ensure_ascii=False, sort_keys=True, indent=1) + "\n"
    if args.check:
        if Path(args.out).read_text() != out:
            sys.exit("REPLAY MISMATCH")
        print("REPLAY OK", sha256(out.encode()))
        return
    Path(args.out).write_text(out)
    print("wrote", args.out, sha256(out.encode()))


if __name__ == "__main__":
    main()
