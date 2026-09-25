//! Frozen-evidence oracles for the #54→#3 QFQ daily pure computation.
//!
//! Oracles are committed artifacts only:
//! * `evidence/issue-54-qfq-contract-regression/result.json` (#169):
//!   `client_oracle_bars` transcriptions (6 bars × OHLC) and per-event
//!   #166 `client_transcription_166` event-day 昨结 anchors.
//! * `evidence/issue-54-gbbq-full-events/result.json` (#159): per-security
//!   `clear_hex` event records.
//! * `evidence/issue-166-windows-official-behavior/raw/files/*.day` (#167):
//!   committed official-client day bytes.

use std::str::FromStr;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde_json::Value;
use x_data_mcp::{day_file, gbbq, qfq};

const FULL_EVENTS: &str = include_str!("../../evidence/issue-54-gbbq-full-events/result.json");
const REGRESSION: &str =
    include_str!("../../evidence/issue-54-qfq-contract-regression/result.json");

fn day_bytes(security: &str) -> &'static [u8] {
    match security {
        "sh.600006" => {
            include_bytes!(
                "../../evidence/issue-166-windows-official-behavior/raw/files/sh600006.day"
            )
        }
        "sh.600004" => {
            include_bytes!(
                "../../evidence/issue-166-windows-official-behavior/raw/files/sh600004.day"
            )
        }
        "sz.000002" => {
            include_bytes!(
                "../../evidence/issue-166-windows-official-behavior/raw/files/sz000002.day"
            )
        }
        other => panic!("no committed .day for {other}"),
    }
}

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn security_events(security: &str) -> Vec<gbbq::Event> {
    let evidence: Value = serde_json::from_str(FULL_EVENTS).unwrap();
    evidence["four_security_matrices"][security]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let clear: [u8; gbbq::RECORD_BYTES] = (0..gbbq::RECORD_BYTES)
                .map(|i| {
                    u8::from_str_radix(&row["clear_hex"].as_str().unwrap()[i * 2..i * 2 + 2], 16)
                        .unwrap()
                })
                .collect::<Vec<u8>>()
                .try_into()
                .unwrap();
            gbbq::event_from_clear(0, &clear).unwrap()
        })
        .collect()
}

fn bars(security: &str) -> Vec<day_file::RawDayRecord> {
    day_file::parse(day_bytes(security)).unwrap()
}

fn ymd(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
}

#[test]
fn all_six_client_oracle_bars_reproduce() {
    let regression: Value = serde_json::from_str(REGRESSION).unwrap();
    let oracle = regression["client_oracle_bars"].as_array().unwrap();
    assert_eq!(oracle.len(), 6);
    for bar in oracle {
        let security = bar["security"].as_str().unwrap();
        let adjusted = qfq::forward_adjust(&bars(security), &security_events(security)).unwrap();
        let date = ymd(bar["date"].as_str().unwrap());
        let qbar = adjusted.iter().find(|b| b.date == date).unwrap();
        assert_eq!(
            qbar.chain_event_count,
            bar["n_events_in_chain"].as_u64().unwrap() as usize,
            "{security} {date} chain length"
        );
        for (field, actual) in [
            ("O", &qbar.open),
            ("H", &qbar.high),
            ("L", &qbar.low),
            ("C", &qbar.close),
        ] {
            assert_eq!(
                *actual,
                dec(bar["fields"][field]["observed"].as_str().unwrap()),
                "{security} {date} {field}"
            );
        }
    }
}

#[test]
fn event_day_previous_close_marks_reproduce_client_transcriptions() {
    let regression: Value = serde_json::from_str(REGRESSION).unwrap();
    let mut anchored = 0usize;
    for row in regression["per_event"].as_array().unwrap() {
        let Some(transcription) = row["oracle"].get("client_transcription_166") else {
            continue;
        };
        let security = row["security"].as_str().unwrap();
        let events = security_events(security);
        let marks = qfq::event_day_marks(&bars(security), &events).unwrap();
        let event_ymd: u32 = row["date"]
            .as_str()
            .unwrap()
            .replace('-', "")
            .parse()
            .unwrap();
        let mark = marks.iter().find(|m| m.event_date == event_ymd).unwrap();
        assert_eq!(
            mark.displayed_previous_close,
            Some(dec(transcription.as_str().unwrap())),
            "{security} {event_ymd} displayed 昨结"
        );
        assert_eq!(
            mark.previous_bar_date,
            Some(ymd(row["prev_date"].as_str().unwrap()))
        );
        anchored += 1;
    }
    assert_eq!(anchored, 2);
}

#[test]
fn empty_event_set_is_identity() {
    let raw = bars("sz.000002");
    let adjusted = qfq::forward_adjust(&raw, &[]).unwrap();
    assert_eq!(adjusted.len(), raw.len());
    for (bar, qbar) in raw.iter().zip(adjusted.iter()) {
        assert_eq!(qbar.chain_event_count, 0);
        for (position, actual) in [
            (0usize, &qbar.open),
            (1, &qbar.high),
            (2, &qbar.low),
            (3, &qbar.close),
        ] {
            assert_eq!(
                *actual,
                Decimal::from(bar.ohlc_raw[position]) / Decimal::from(100)
            );
        }
        assert_eq!(qbar.amount_float32_bits, bar.amount_float32_bits);
        assert_eq!(qbar.volume_raw, bar.volume_raw);
    }
}

#[test]
fn event_on_bar_date_does_not_adjust_that_bar() {
    // sh.600006 2025-07-17 is the event day itself (chain length 0): its
    // adjusted bar must equal the raw bar.
    let raw = bars("sh.600006");
    let adjusted = qfq::forward_adjust(&raw, &security_events("sh.600006")).unwrap();
    let date = ymd("2025-07-17");
    let qbar = adjusted.iter().find(|b| b.date == date).unwrap();
    let raw_bar = raw.iter().find(|b| b.date == date).unwrap();
    assert_eq!(qbar.chain_event_count, 0);
    assert_eq!(
        qbar.close,
        Decimal::from(raw_bar.ohlc_raw[3]) / Decimal::from(100)
    );
}

#[test]
fn event_without_matching_bar_still_adjusts_earlier_bars() {
    // An event on a non-trading date must still enter the chain of earlier
    // bars.  Fabricate a synthetic event dated between two 000002 bars
    // (1999-12-30 has no bar for this check — use a date strictly after the
    // last 1999 bar and before 2000-01-10).
    let raw = bars("sz.000002");
    let synthetic = gbbq::Event {
        market: 0,
        code: "000002".into(),
        date_raw: 20000101,
        category: 1,
        c_raw: [1.0f32, 0.0, 0.0, 0.0],
    };
    let adjusted = qfq::forward_adjust(&raw, std::slice::from_ref(&synthetic)).unwrap();
    let before = adjusted
        .iter()
        .find(|b| b.date == ymd("1999-12-29"))
        .unwrap();
    let after = adjusted
        .iter()
        .find(|b| b.date == ymd("2000-01-10"))
        .unwrap();
    assert_eq!(before.chain_event_count, 1);
    assert_eq!(after.chain_event_count, 0);
    // T(x) = x - 1.00/10 = x - 0.10 for C1=1.00.
    let raw_before = raw.iter().find(|b| b.date == ymd("1999-12-29")).unwrap();
    assert_eq!(
        before.close,
        Decimal::from(raw_before.ohlc_raw[3]) / Decimal::from(100) - dec("0.10")
    );
    // No earlier bar exists before the first record: mark is None.
    let first = gbbq::Event {
        date_raw: 19900101,
        ..synthetic
    };
    let marks = qfq::event_day_marks(&raw, &[first]).unwrap();
    assert_eq!(marks[0].displayed_previous_close, None);
    assert_eq!(marks[0].previous_bar_date, None);
}

#[test]
fn non_strict_event_inclusion_cannot_reproduce_client_fields() {
    // Anti-tautology: including the event ON the bar date (>= instead of the
    // frozen strict >) must fail to reproduce the observed displays.
    let events = security_events("sz.000002");
    let raw = bars("sz.000002");
    let regression: Value = serde_json::from_str(REGRESSION).unwrap();
    let mut mismatches = 0usize;
    for bar in regression["client_oracle_bars"].as_array().unwrap() {
        if bar["security"].as_str().unwrap() != "sz.000002" {
            continue;
        }
        let ymd_int: u32 = bar["date"]
            .as_str()
            .unwrap()
            .replace('-', "")
            .parse()
            .unwrap();
        let mut dated: Vec<(u32, [Decimal; 4])> = events
            .iter()
            .map(|e| (e.date_raw, e.factors().unwrap()))
            .collect();
        dated.sort_by_key(|(d, _)| *d);
        let chain =
            gbbq::compose_chain(dated.iter().filter(|(d, _)| *d >= ymd_int).map(|(_, f)| *f));
        let raw_bar = raw
            .iter()
            .find(|b| b.date == ymd(bar["date"].as_str().unwrap()))
            .unwrap();
        for (position, field) in ["O", "H", "L", "C"].iter().enumerate() {
            let raw_price = Decimal::from(raw_bar.ohlc_raw[position]) / Decimal::from(100);
            let predicted = chain
                .apply(raw_price)
                .round_dp_with_strategy(2, rust_decimal::RoundingStrategy::MidpointAwayFromZero);
            if predicted != dec(bar["fields"][field]["observed"].as_str().unwrap()) {
                mismatches += 1;
            }
        }
    }
    assert!(
        mismatches > 0,
        "inclusive event filter must not fully match"
    );
}
