//! NM-002/NM-003 P0 oracle: emit observed outputs of existing public Rust
//! functions as golden JSONL vectors for the Node.js migration.
//! Deterministic: all inputs are source literals except one committed fixture
//! file (tests/fixtures/issue79/legacy-records.json); no env, clocks or randomness.
//! Each line: {"family","id","input","observed","semantics","source",
//! "oracle_sha","run_ref","provenance"}.
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::{collections::BTreeSet, str::FromStr};
use x_data_mcp::{
    compact_market, day_file, gbbq, market_data, market_storage, metrics, publish, qfq, yearly,
};

fn d(date: &str) -> NaiveDate {
    NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap()
}
fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}
const ORACLE_SHA: &str = "32c29e0c57832b74925316ec8b601c16d5696fae";
const RUN_REF: &str = "nm003-oracle-run-1";

fn emit(family: &str, id: &str, input: Value, observed: Value, semantics: &str, source: &str) {
    let provenance = if family == "amount_f32_projection" {
        "source_derived_expression"
    } else {
        "observed_public_api"
    };
    println!(
        "{}",
        serde_json::to_string(&json!({
            "family": family, "id": id, "input": input,
            "observed": observed, "semantics": semantics, "source": source,
            "oracle_sha": ORACLE_SHA, "run_ref": RUN_REF, "provenance": provenance,
        }))
        .unwrap()
    );
}
fn ok<T, E: std::fmt::Display>(r: &Result<T, E>, f: impl Fn(&T) -> Value) -> Value {
    match r {
        Ok(v) => json!({"status": "ok", "value": f(v)}),
        Err(e) => json!({"status": "error", "error": e.to_string()}),
    }
}
fn raw_rec(y: u32, ohlc: [u32; 4], bits: u32, vol: u32, r7: u32) -> day_file::RawDayRecord {
    day_file::RawDayRecord {
        date: NaiveDate::from_ymd_opt(
            (y / 10000) as i32,
            (y / 100 % 100) as u32,
            (y % 100) as u32,
        )
        .unwrap(),
        ohlc_raw: ohlc,
        amount_float32_bits: bits,
        volume_raw: vol,
        r7_raw: r7,
    }
}
fn day_bytes(words: &[[u32; 8]]) -> Vec<u8> {
    words.iter().flatten().flat_map(|w| w.to_le_bytes()).collect()
}
fn bar(date: &str, o: &str, h: &str, l: &str, c: &str, v: Option<&str>, a: Option<&str>) -> market_data::Bar {
    market_data::Bar {
        date: d(date),
        open: dec(o), high: dec(h), low: dec(l), close: dec(c),
        volume: v.map(dec), amount: a.map(dec), r7_raw: None,
    }
}
fn version(records: Vec<market_data::Bar>) -> publish::EffectiveVersion {
    publish::EffectiveVersion {
        version_id: "oracle-daily-v1".into(),
        symbol: "600006".into(),
        period: market_data::Period::Day,
        adjustment: market_data::Adjustment::Forward,
        source: "oracle:synthetic".into(),
        published_at: DateTime::<Utc>::from_timestamp(1_717_200_000, 0).unwrap(),
        records,
        validation: publish::ValidationSummary {
            record_count: 0, unique_record_count: 0, duplicate_count: 0,
        },
        unit_currency: market_data::UnitCurrencyEvidence::confirmed(
            "cny", "yuan", "share", "cny", "yuan", "oracle fixture",
        ),
        adjustment_evidence: market_data::AdjustmentEvidence::confirmed("qfq", "oracle fixture"),
        yearly_derivation: None,
    }
}
fn compact_view(b: &compact_market::CompactBar) -> Value {
    json!({"trade_day": b.trade_day, "open_cents": b.open_cents, "high_cents": b.high_cents,
           "low_cents": b.low_cents, "close_cents": b.close_cents, "volume": b.volume,
           "amount": b.amount, "r7_raw": b.r7_raw})
}
// Mirrors the private projection expression at src/official_market.rs:396-397
// verbatim; labelled source-derived in emitted semantics.
fn project_amount(bits: u32) -> Option<Decimal> {
    Decimal::from_str(&f64::from(f32::from_bits(bits)).to_string()).ok()
}

fn main() {
    // ---- F1: .day structural parse --------------------------------------
    let valid = day_bytes(&[
        [20240102, 1500, 1520, 1495, 1510, f32::to_bits(150225000.0), 1234567, 0x10000],
        [20240103, 1512, 1530, 1508, 1525, f32::to_bits(99750000.0), 987654, 0x10000],
    ]);
    emit("day_parse", "two_valid_records", json!({"hex": valid.iter().map(|b| format!("{b:02x}")).collect::<String>()}),
        ok(&day_file::parse(&valid), |recs| json!(recs.iter().map(|r| json!({
            "date": r.date.to_string(), "ohlc_raw": r.ohlc_raw,
            "amount_float32_bits": r.amount_float32_bits, "volume_raw": r.volume_raw, "r7_raw": r.r7_raw,
        })).collect::<Vec<_>>())),
        "8 u32 LE words/record; strictly increasing dates; raw fields preserved verbatim",
        "src/day_file.rs::parse");
    emit("day_parse", "truncated_31_bytes", json!({"bytes": 31}),
        ok(&day_file::parse(&valid[..31]), |_| Value::Null),
        "byte count not divisible by 32 fails closed", "src/day_file.rs::DayFileError::Truncated");
    emit("day_parse", "invalid_month_13", json!({"date_word": 20241301}),
        ok(&day_file::parse(&day_bytes(&[[20241301, 1, 1, 1, 1, 0, 0, 0]])), |_| Value::Null),
        "invalid calendar date fails closed", "src/day_file.rs::DayFileError::InvalidDate");
    emit("day_parse", "non_increasing_date", json!({"dates": [20240103, 20240102]}),
        ok(&day_file::parse(&day_bytes(&[[20240103,1,1,1,1,0,0,0],[20240102,1,1,1,1,0,0,0]])), |_| Value::Null),
        "non-increasing date fails closed", "src/day_file.rs::DayFileError::DateOrder");

    // ---- F2: f32 amount projection (expression-identical mirror) --------
    for (id, bits) in [
        ("int_yuan", f32::to_bits(150225000.0f32)),
        ("tenth", f32::to_bits(0.1f32)),
        ("third_like", f32::to_bits(3.3333333f32)),
        ("negative", f32::to_bits(-5.5f32)),
        ("nan", f32::NAN.to_bits()),
        ("pos_inf", f32::INFINITY.to_bits()),
    ] {
        emit("amount_f32_projection", id, json!({"f32_bits": format!("0x{bits:08x}")}),
            json!({"status": "ok", "value": project_amount(bits).map(|v| v.to_string())}),
            "Decimal::from_str(f64::from(f32::from_bits(bits)).to_string()).ok(); shortest round-trip; NaN/inf -> None",
            "src/official_market.rs:396-397 (private expr, mirrored verbatim)");
    }

    // ---- F3: GBBQ factor normalization ----------------------------------
    for (id, raw) in [
        ("tenth", 0.1f32), ("round_up_1_005", 1.005f32), ("exact_1_25", 1.25f32),
        ("neg_small", -0.001f32), ("nan", f32::NAN), ("pos_inf", f32::INFINITY),
    ] {
        emit("gbbq_factor_normalize", id, json!({"f32": f64::from(raw)}),
            json!({"status": "ok", "value": gbbq::normalize_factor(raw).map(|v| v.to_string())}),
            "f32->f64->shortest-decimal->2dp HALF_UP; non-finite -> None (fail closed)",
            "src/gbbq.rs::normalize_factor");
    }

    // ---- F4: QFQ affine chain -------------------------------------------
    let c = [dec("2.00"), dec("5.00"), dec("0"), dec("1.00")];
    let t = gbbq::event_transform(&c);
    emit("qfq_affine", "event_transform", json!({"c": ["2.00","5.00","0","1.00"]}),
        json!({"status": "ok", "value": {"a": t.a.to_string(), "b": t.b.to_string()}}),
        "T(x)=a*x+b, a=10/(10+C3+C4), b=(C4*C2-C1)/(10+C3+C4); unrounded Decimal",
        "src/gbbq.rs::event_transform");
    let chain = gbbq::compose_chain([c, [dec("1.00"), dec("0"), dec("0"), dec("2.00")]]);
    emit("qfq_affine", "compose_chain_two_events", json!({"events": [["2.00","5.00","0","1.00"],["1.00","0","0","2.00"]]}),
        json!({"status": "ok", "value": {"a": chain.a.to_string(), "b": chain.b.to_string()}}),
        "oldest->newest fold, no intermediate rounding", "src/gbbq.rs::compose_chain");
    emit("qfq_affine", "displayed_previous_close", json!({"prev_close": "10.00", "c": ["2.00","5.00","0","1.00"]}),
        json!({"status": "ok", "value": gbbq::displayed_previous_close(dec("10.00"), &c).to_string()}),
        "reference price then 2dp HALF_UP", "src/gbbq.rs::displayed_previous_close");

    let bars = vec![
        raw_rec(20240102, [1500, 1520, 1495, 1510], 0, 1000, 0x10000),
        raw_rec(20240103, [1512, 1530, 1508, 1525], 0, 2000, 0x10000),
        raw_rec(20240104, [1000, 1010, 990, 1005], 0, 3000, 0x10000),
    ];
    let event = gbbq::Event {
        market: 1, code: "600006".into(), date_raw: 20240104, category: 1,
        c_raw: [2.0, 5.0, 0.0, 1.0],
    };
    emit("qfq_forward_adjust", "one_dividend_event", json!({"bars": 3, "event_date": 20240104, "c_raw": [2.0,5.0,0.0,1.0]}),
        ok(&qfq::forward_adjust(&bars, &[event.clone()]), |out| json!(out.iter().map(|b| json!({
            "date": b.date.to_string(), "open": b.open.to_string(), "high": b.high.to_string(),
            "low": b.low.to_string(), "close": b.close.to_string(),
            "chain_event_count": b.chain_event_count,
        })).collect::<Vec<_>>())),
        "chain applies to bars strictly before event date; output 2dp HALF_UP; amount/volume/r7 verbatim",
        "src/qfq.rs::forward_adjust");
    emit("qfq_forward_adjust", "empty_events_identity", json!({"events": []}),
        ok(&qfq::forward_adjust(&bars, &[]), |out| json!(out.iter().map(|b| b.close.to_string()).collect::<Vec<_>>())),
        "empty chain = identity, prices equal raw", "src/qfq.rs::forward_adjust");
    let bad_event = gbbq::Event { c_raw: [f32::NAN, 0.0, 0.0, 0.0], ..event };
    emit("qfq_forward_adjust", "nan_factor_reject", json!({"c_raw": ["NaN",0.0,0.0,0.0]}),
        ok(&qfq::forward_adjust(&bars, &[bad_event]), |_| Value::Null),
        "non-finite factor fails closed", "src/gbbq.rs::Error::NonFiniteFactor");

    // ---- F5: yearly aggregate Decimal semantics --------------------------
    let daily = version(vec![
        bar("2023-01-03", "10", "11", "9.5", "10.5", Some("100"), Some("1050")),
        bar("2023-06-30", "10.5", "12", "10.4", "11.8", Some("200"), Some("2360")),
        bar("2024-01-02", "11.8", "12.5", "11.6", "12.3", Some("150"), Some("1845")),
        bar("2024-03-28", "12.3", "13", "12.2", "12.9", Some("180"), Some("2322")),
    ]);
    emit("yearly_aggregate", "two_years_valid", json!({"bars": 4}),
        ok(&yearly::aggregate(&daily, None, d("2024-06-01")), |s| json!(s.records.iter().map(|r| json!({
            "year": r.bar.date.year(), "open": r.bar.open.to_string(), "high": r.bar.high.to_string(),
            "low": r.bar.low.to_string(), "close": r.bar.close.to_string(),
            "volume": r.bar.volume.map(|v| v.to_string()), "amount": r.bar.amount.map(|v| v.to_string()),
            "year_unfinished": r.year_unfinished,
        })).collect::<Vec<_>>())),
        "first-open/max-high/min-low/last-close; exact Decimal sums; unfinished flag by as_of",
        "src/yearly.rs::aggregate");
    let missing = BTreeSet::from([2025]);
    emit("yearly_aggregate", "missing_year_reject", json!({"years": [2025]}),
        ok(&yearly::aggregate(&daily, Some(&missing), d("2024-06-01")), |_| Value::Null),
        "requested absent natural year fails closed", "src/yearly.rs::AggregationError::MissingYear");
    let mut none_amount = daily.clone();
    none_amount.records[1].amount = None;
    emit("yearly_aggregate", "none_amount_propagates", json!({"note": "bar[1].amount=None"}),
        ok(&yearly::aggregate(&none_amount, None, d("2024-06-01")), |s| json!(s.records.iter().map(|r|
            json!({"year": r.bar.date.year(), "amount": r.bar.amount.map(|v| v.to_string())})
        ).collect::<Vec<_>>())),
        "any-None field yields unknown yearly field", "src/yearly.rs::aggregate");
    let mut overflow = daily.clone();
    for b in &mut overflow.records {
        b.amount = Some(Decimal::MAX);
    }
    emit("yearly_aggregate", "inexact_sum_overflow", json!({"amount": "Decimal::MAX x4"}),
        ok(&yearly::aggregate(&overflow, None, d("2024-06-01")), |_| Value::Null),
        "checked_add overflow -> InexactSum reject, no wraparound", "src/yearly.rs::AggregationError::InexactSum");
    let mut unsorted = daily.clone();
    unsorted.records.swap(0, 1);
    emit("yearly_aggregate", "unsorted_reject", json!({"note": "bar order swapped"}),
        ok(&yearly::aggregate(&unsorted, None, d("2024-06-01")), |_| Value::Null),
        "non-increasing dates fail closed", "src/yearly.rs::AggregationError::InvalidInput");

    // ---- F6: BigInt half-even ratio_change ------------------------------
    for (id, end, anchor) in [
        ("five_pct", "105", "100"), ("third", "1", "3"), ("sixth", "1", "6"),
        ("tie_even_quotient", "8193", "8192"), ("tie_odd_quotient", "8195", "8192"),
    ] {
        emit("metric_ratio_change", id, json!({"end": end, "anchor": anchor}),
            ok(&metrics::consecutive::ratio_change(dec(end), dec(anchor)), |v| json!(v)),
            "exact rational (end-anchor)/anchor, single half-even rounding at 12dp",
            "src/metrics/consecutive.rs::ratio_change");
    }
    emit("metric_ratio_change", "zero_anchor_reject", json!({"end": "1", "anchor": "0"}),
        ok(&metrics::consecutive::ratio_change(dec("1"), dec("0")), |v| json!(v)),
        "non-positive inputs fail closed", "src/metrics/consecutive.rs::ratio_change");

    // ---- F7: digest styles ----------------------------------------------
    let rec: Value = serde_json::from_str(
        &std::fs::read_to_string("tests/fixtures/issue79/legacy-records.json").unwrap(),
    )
    .unwrap();
    let first = &rec[0];
    emit("digest_legacy_published", "fixture_record0", json!({"fixture": "tests/fixtures/issue79/legacy-records.json[0]"}),
        ok(&market_storage::compat::legacy_digest(first), |v| json!(v)),
        "sha256 over serde_json bytes minus integrity_sha256 key", "src/market_storage/compat.rs::legacy_digest");
    emit("digest_legacy_published", "synthetic_object", json!({"input": {"a": 1, "integrity_sha256": "ignored"}}),
        ok(&market_storage::compat::legacy_digest(&json!({"a":1,"integrity_sha256":"ignored"})), |v| json!(v)),
        "same digest regardless of integrity_sha256 value", "src/market_storage/compat.rs::legacy_digest");
    emit("digest_legacy_published", "non_object_reject", json!({"input": "x"}),
        ok(&market_storage::compat::legacy_digest(&json!("x")), |_| Value::Null),
        "non-object fails closed", "src/market_storage/compat.rs::legacy_digest");

    let evidence_in = json!({
        "source": "oracle:synthetic",
        "coverage_reason": "synthetic complete coverage for oracle",
        "unit_currency": serde_json::to_value(market_data::UnitCurrencyEvidence::confirmed(
            "cny", "yuan", "share", "cny", "yuan", "oracle fixture")).unwrap(),
        "adjustment_evidence": serde_json::to_value(market_data::AdjustmentEvidence::confirmed(
            "qfq", "oracle fixture")).unwrap(),
    });
    emit("digest_canonical_evidence", "version_kind_valid", json!({"kind": "version"}),
        ok(&compact_market::canonical_evidence(compact_market::EvidenceKind::Version, &evidence_in),
            |b| json!({"canonical_json": b.canonical_json, "digest": b.digest})),
        "VersionEvidence shape + RFC 8785 canonical JSON + domain-separated sha256",
        "src/compact_market.rs::canonical_evidence");
    let mut bars_evidence = evidence_in.clone();
    bars_evidence["nested"] = json!({"records": [1, 2]});
    emit("digest_canonical_evidence", "embedded_records_reject", json!({"kind": "version", "note": "nested.records array"}),
        ok(&compact_market::canonical_evidence(compact_market::EvidenceKind::Version, &bars_evidence), |_| Value::Null),
        "bars/records arrays inside evidence fail closed", "src/compact_market.rs::reject_bar_arrays");
    emit("digest_canonical_evidence", "version_shape_reject", json!({"kind": "version", "value": {"b":[2,1],"a":{"z":true,"m":null}}}),
        ok(&compact_market::canonical_evidence(compact_market::EvidenceKind::Version,
            &json!({"b":[2,1],"a":{"z":true,"m":null}})), |_| Value::Null),
        "Version evidence must match VersionEvidence shape", "src/compact_market.rs::check_version_evidence");
    emit("digest_canonical_evidence", "non_object_reject", json!({"kind": "version", "value": [1,2]}),
        ok(&compact_market::canonical_evidence(compact_market::EvidenceKind::Version, &json!([1,2])), |_| Value::Null),
        "evidence must be a JSON object", "src/compact_market.rs::check_evidence_semantics");
    emit("digest_rfc8785", "nested_ordering", json!({"input": {"z":1,"a":{"y":[true,null]},"\u{00e9}":2}}),
        json!({"status": "ok", "value": serde_json_canonicalizer::to_string(
            &json!({"z":1,"a":{"y":[true,null]},"\u{00e9}":2})).unwrap()}),
        "RFC 8785 / JCS canonical form", "serde_json_canonicalizer =0.3.2");
    println!(
        "{}",
        serde_json::to_string(&json!({
            "family": "digest_rfc8785", "id": "reject_boundary", "input": Value::Null,
            "observed": {"status": "not_run",
                "reason": "serde_json_canonicalizer::to_string is a total serializer over serde_json::Value: every Value input serializes, so no reject case exists in this domain. Reject semantics live at the canonical_evidence layer (non-object / shape / embedded-bars vectors cover them)."},
            "semantics": "justified limitation: total function over Value domain; boundary coverage delegated to digest_canonical_evidence rejects",
            "source": "serde_json_canonicalizer =0.3.2; src/compact_market.rs::canonical_evidence",
            "oracle_sha": ORACLE_SHA, "run_ref": Value::Null, "provenance": "justified_not_run",
        }))
        .unwrap()
    );

    let day_bar = bar("2024-01-02", "15.00", "15.20", "14.95", "15.10", Some("1234567"), Some("150225000"));
    emit("compact_bar", "from_bar_valid", json!({"close": "15.10", "amount": "150225000"}),
        ok(&compact_market::CompactBar::from_bar(&day_bar, market_data::Period::Day), |b| compact_view(b)),
        "prices -> integer cents (exactly 2dp); amount -> canonical decimal text; date -> epoch day",
        "src/compact_market.rs::CompactBar::from_bar");
    emit("compact_bar", "price_not_2dp_reject", json!({"close": "15.105"}),
        ok(&compact_market::CompactBar::from_bar(
            &bar("2024-01-02","15","15.2","14.9","15.105",None,None), market_data::Period::Day), |_| Value::Null),
        "non-2dp price fails closed", "src/compact_market.rs::cents");
    emit("compact_bar", "r7_on_yearly_reject", json!({"period": "year", "r7_raw": 65536}),
        ok(&compact_market::CompactBar::from_bar(&market_data::Bar{
            date: d("2024-12-31"), open: dec("10"), high: dec("12"), low: dec("9"), close: dec("11"),
            volume: None, amount: None, r7_raw: Some(0x10000),
        }, market_data::Period::Year), |_| Value::Null),
        "yearly bar carrying r7_raw fails closed", "src/compact_market.rs::CompactBar::from_bar");
    for (id, text) in [
        ("amount_trailing_zero_reject", "1.230"),
        ("amount_not_decimal_reject", "abc"),
        ("amount_negative_reject", "-5"),
    ] {
        let cb = compact_market::CompactBar {
            trade_day: 19723, open_cents: 1500, high_cents: 1520, low_cents: 1495,
            close_cents: 1510, volume: None, amount: Some(text.into()), r7_raw: None,
        };
        emit("compact_bar", id, json!({"amount": text}),
            ok(&cb.to_bar(market_data::Period::Day).map(|_| ()), |_| Value::Null),
            "stored amount must be canonical non-negative decimal", "src/compact_market.rs::CompactBar::check");
    }
    let seg_bars = [
        compact_market::CompactBar::from_bar(&day_bar, market_data::Period::Day).unwrap(),
        compact_market::CompactBar::from_bar(
            &bar("2024-01-03","15.12","15.30","15.08","15.25",Some("987654"),Some("99750000")),
            market_data::Period::Day).unwrap(),
    ];
    emit("segment_digest", "two_bars", json!({"entity": "sh.600006", "period": "day", "bars": 2}),
        ok(&compact_market::segment_digest("sh.600006", market_data::Period::Day, &seg_bars),
            |dg| json!(compact_market::digest_hex(dg))),
        "\"x-data-segment-v1\\0\"+entity+period+count+bar encodings -> sha256",
        "src/compact_market.rs::segment_digest");
    emit("segment_digest", "empty_reject", json!({"bars": 0}),
        ok(&compact_market::segment_digest("sh.600006", market_data::Period::Day, &[]), |_| Value::Null),
        "empty segment fails closed", "src/compact_market.rs::segment_digest");
    emit("segment_digest", "bad_entity_reject", json!({"entity": "600006"}),
        ok(&compact_market::segment_digest("600006", market_data::Period::Day, &seg_bars), |_| Value::Null),
        "non-canonical entity fails closed", "src/compact_market.rs::canonical_entity");
}
