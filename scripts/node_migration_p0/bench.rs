//! NM-005 P0 Rust baseline: fixed synthetic workload timing probe.
//! Deterministic inputs (literal-derived), N repetitions, raw ns samples
//! printed as JSONL: {"probe","reps","samples_ns":[...],"input":{...}}.
//! Reads VmRSS of self before/after for RSS evidence.
use chrono::NaiveDate;
use rust_decimal::Decimal;
use std::time::Instant;
use x_data_mcp::{compact_market, day_file, gbbq, market_data, publish, qfq, yearly};

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}
fn rss_kb() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status").ok().and_then(|s| {
        s.lines()
            .find(|l| l.starts_with("VmRSS"))
            .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
    })
}
fn emit(probe: &str, input: serde_json::Value, samples: &[u128]) {
    let mut sorted = samples.to_vec();
    sorted.sort();
    let p50 = sorted[sorted.len() / 2];
    let p95 = sorted[sorted.len() * 95 / 100];
    println!(
        "{}",
        serde_json::json!({
            "probe": probe, "reps": samples.len(), "input": input,
            "samples_ns": samples, "p50_ns": p50, "p95_ns": p95,
        })
    );
}
fn day_records(n: usize) -> (Vec<u8>, Vec<day_file::RawDayRecord>) {
    let mut recs = Vec::with_capacity(n);
    let mut bytes = Vec::with_capacity(n * 32);
    let mut date = d(2020, 1, 1);
    for i in 0..n {
        while date.weekday().num_days_from_monday() > 4 {
            date += chrono::Duration::days(1);
        }
        let r = day_file::RawDayRecord {
            date,
            ohlc_raw: [1495 + (i % 25) as u32, 1520, 1495, 1495 + (i % 20) as u32],
            amount_float32_bits: f32::to_bits(150225000.0 + i as f32),
            volume_raw: 1234567 + i as u32,
            r7_raw: 0x10000,
        };
        let y = date.year() as u32 * 10000 + date.month() * 100 + date.day();
        for w in [y, r.ohlc_raw[0], r.ohlc_raw[1], r.ohlc_raw[2], r.ohlc_raw[3],
                  r.amount_float32_bits, r.volume_raw, r.r7_raw] {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        recs.push(r);
        date += chrono::Duration::days(1);
    }
    (bytes, recs)
}
use chrono::Datelike;
fn main() {
    const REPS: usize = 20;
    let rss0 = rss_kb();

    // P1: .day structural parse, 2500 records (~80KB)
    let (bytes, recs) = day_records(2500);
    let bytes_sha = x_data_mcp::hash::sha256(&bytes);
    let mut s = Vec::with_capacity(REPS);
    for _ in 0..REPS {
        let t = Instant::now();
        let out = day_file::parse(&bytes).unwrap();
        s.push(t.elapsed().as_nanos());
        std::hint::black_box(&out);
    }
    emit("day_parse_2500rec", serde_json::json!({"records":2500,"bytes":bytes.len(),"input_sha256":bytes_sha}), &s);

    // P2: QFQ forward_adjust, 250 bars x 20 events
    let bars250: Vec<_> = recs[..250].to_vec();
    let events: Vec<gbbq::Event> = (0..20)
        .map(|i| gbbq::Event {
            market: 1, code: "600006".into(),
            date_raw: 20200201 + i * 30, category: 1,
            c_raw: [2.0 + i as f32 * 0.1, 5.0, 0.0, 1.0],
        })
        .collect();
    let mut s = Vec::with_capacity(REPS);
    for _ in 0..REPS {
        let t = Instant::now();
        let out = qfq::forward_adjust(&bars250, &events).unwrap();
        s.push(t.elapsed().as_nanos());
        std::hint::black_box(&out);
    }
    emit("qfq_adjust_250bars_20events", serde_json::json!({"bars":250,"events":20}), &s);

    // P3: yearly aggregate over 2500-bar daily version
    let version = publish::EffectiveVersion {
        version_id: "bench-daily".into(), symbol: "600006".into(),
        period: market_data::Period::Day, adjustment: market_data::Adjustment::Forward,
        source: "bench:synthetic".into(),
        published_at: chrono::DateTime::<chrono::Utc>::from_timestamp(1_717_200_000, 0).unwrap(),
        records: recs.iter().map(|r| market_data::Bar {
            date: r.date,
            open: Decimal::new(r.ohlc_raw[0] as i64, 2),
            high: Decimal::new(r.ohlc_raw[1] as i64, 2),
            low: Decimal::new(r.ohlc_raw[2] as i64, 2),
            close: Decimal::new(r.ohlc_raw[3] as i64, 2),
            volume: Some(Decimal::from(r.volume_raw)),
            amount: Some(Decimal::new(150225000, 0)),
            r7_raw: Some(r.r7_raw),
        }).collect(),
        validation: publish::ValidationSummary { record_count: 0, unique_record_count: 0, duplicate_count: 0 },
        unit_currency: market_data::UnitCurrencyEvidence::confirmed("cny","yuan","share","cny","yuan","bench"),
        adjustment_evidence: market_data::AdjustmentEvidence::confirmed("qfq","bench"),
        yearly_derivation: None,
    };
    let mut s = Vec::with_capacity(REPS);
    for _ in 0..REPS {
        let t = Instant::now();
        let out = yearly::aggregate(&version, None, d(2035, 1, 1)).unwrap();
        s.push(t.elapsed().as_nanos());
        std::hint::black_box(&out);
    }
    emit("yearly_aggregate_2500bars", serde_json::json!({"bars":2500,"years":"all"}), &s);

    // P4: compact segment_digest, 250 bars
    let seg: Vec<_> = version.records[..250].iter()
        .map(|b| compact_market::CompactBar::from_bar(b, market_data::Period::Day).unwrap())
        .collect();
    let mut s = Vec::with_capacity(REPS);
    for _ in 0..REPS {
        let t = Instant::now();
        let out = compact_market::segment_digest("sh.600006", market_data::Period::Day, &seg).unwrap();
        s.push(t.elapsed().as_nanos());
        std::hint::black_box(compact_market::digest_hex(&out));
    }
    emit("segment_digest_250bars", serde_json::json!({"bars":250,"entity":"sh.600006"}), &s);

    // P5: normalize_factor x10000 (f32->Decimal HALF_UP loop)
    let mut s = Vec::with_capacity(REPS);
    for _ in 0..REPS {
        let t = Instant::now();
        for i in 0..10000u32 {
            std::hint::black_box(gbbq::normalize_factor(0.1f32 + i as f32 * 0.0001));
        }
        s.push(t.elapsed().as_nanos());
    }
    emit("normalize_factor_10k", serde_json::json!({"iterations":10000}), &s);

    println!("{}", serde_json::json!({"probe":"rss","start_kb":rss0,"end_kb":rss_kb()}));
}
