//! Frozen-evidence oracles for the #54 GBBQ semantic layer.
//!
//! Every expectation below is read from committed evidence artifacts — no
//! values are recomputed or fitted:
//!
//! * `evidence/issue-54-gbbq-full-events/result.json` (#159): 94 committed
//!   `clear_hex` records with `float32_raw_bits_hex` and `fixed_2dp` oracles.
//! * `evidence/issue-54-qfq-contract-regression/result.json` (#169): per-event
//!   `ref`/`ref_cent` and the six `client_oracle_bars` transcriptions.
//! * `evidence/issue-166-windows-official-behavior/raw/files/sz000002.day`
//!   (#167): committed official-client day bytes for the chain oracle.

use std::str::FromStr;

use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::Value;
use x_data_mcp::{day_file, gbbq};

const FULL_EVENTS: &str = include_str!("../evidence/issue-54-gbbq-full-events/result.json");
const REGRESSION: &str = include_str!("../evidence/issue-54-qfq-contract-regression/result.json");
const SZ000002_DAY: &[u8] =
    include_bytes!("../evidence/issue-166-windows-official-behavior/raw/files/sz000002.day");

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn cent(v: Decimal) -> Decimal {
    v.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
}

fn rows() -> Vec<(String, Value)> {
    let evidence: Value = serde_json::from_str(FULL_EVENTS).unwrap();
    let mut rows = Vec::new();
    for (security, matrix) in evidence["four_security_matrices"].as_object().unwrap() {
        for row in matrix["rows"].as_array().unwrap() {
            rows.push((security.clone(), row.clone()));
        }
    }
    rows
}

fn event_from_row(row: &Value) -> gbbq::Event {
    let clear: [u8; gbbq::RECORD_BYTES] =
        hex(row["clear_hex"].as_str().unwrap()).try_into().unwrap();
    gbbq::event_from_clear(row["record_index"].as_u64().unwrap() as usize, &clear).unwrap()
}

// Encrypted → clear pairs extracted from the retained gen-2 member at the
// committed `record_index` of each row (frozen bytes, not recomputed).
const CIPHER_VECTORS: &[(&str, &str)] = &[
    // sh.600006 2000-06-26 rec#127401
    (
        "28c4582b6e670f57e4a6936139dba0cc9f26e436da9482900000000000",
        "0136303030303600722f3101010000803f000000000000000000000000",
    ),
    // sh.600004 2004-07-13 rec#127329
    (
        "f41747da21359a9371fcf3bb96b9a3eb9f26e436da9482900000000000",
        "013630303030340009cc3101010000803f000000000000000000000000",
    ),
    // sz.000002 1991-06-01 rec#81
    (
        "3319534938f4d6c6c9eb7361cfb4255dc3ca168651206553000000a040",
        "0030303030303200c9cf2f010100000000cdcc8c40000000000000a040",
    ),
    // sz.000004 1991-06-28 rec#208
    (
        "9a0780d5c8b0cd2e89034f986b74ccb58d296e3fe8a2ffca3f00000000",
        "0030303030303400e4cf2f010100000000000000000000c03f00000000",
    ),
    // sh.600006 2025-07-17 rec#127431
    (
        "28c4582b6e670f57d0de8c025d371fae55cab4d73a3f042c0000000000",
        "01363030303036005d00350101cdcc4c3d000000000000000000000000",
    ),
    // sh.600004 2026-07-24 rec#127368
    (
        "f41747da21359a93674dcede1e4ec4001904d01f68e853950000000000",
        "0136303030303400742735010166663640000000000000000000000000",
    ),
    // sz.000002 2000-01-10 rec#103
    (
        "3319534938f4d6c695486b53b7d4dc5cfe96dedc565181e7002b872e40",
        "00303030303032006e2d310101000000000000f040000000002b872e40",
    ),
];

#[test]
fn cipher_vectors_match_committed_clear_bytes() {
    for (encrypted_hex, clear_hex) in CIPHER_VECTORS {
        let encrypted: [u8; gbbq::RECORD_BYTES] = hex(encrypted_hex).try_into().unwrap();
        let clear = gbbq::decrypt_record(&encrypted);
        assert_eq!(
            clear.as_slice(),
            hex(clear_hex).as_slice(),
            "cipher vector {encrypted_hex}"
        );
        // The semantic layer on the same record agrees with the clear bytes.
        let event = gbbq::decode_record(0, &encrypted).unwrap();
        let expected = gbbq::event_from_clear(0, &clear).unwrap();
        assert_eq!(event, expected);
    }
}

#[test]
fn decode_container_preserves_record_order_and_category() {
    let mut bytes = (CIPHER_VECTORS.len() as u32).to_le_bytes().to_vec();
    for (encrypted_hex, _) in CIPHER_VECTORS {
        bytes.extend(hex(encrypted_hex));
    }
    let file = gbbq::parse(&bytes).unwrap();
    let events = gbbq::decode_events(&file).unwrap();
    assert_eq!(events.len(), CIPHER_VECTORS.len());
    assert_eq!(events[0].code, "600006");
    assert_eq!(events[0].market, 1);
    assert_eq!(events[0].date_raw, 20000626);
    assert_eq!(events[2].code, "000002");
    assert_eq!(events[2].market, 0);
    for event in &events {
        assert_eq!(event.category, 1);
    }
}

#[test]
fn all_94_committed_events_decode_and_normalize_against_oracle() {
    let rows = rows();
    assert_eq!(rows.len(), 94);
    for (security, row) in &rows {
        let event = event_from_row(row);
        let clear = hex(row["clear_hex"].as_str().unwrap());
        assert_eq!(event.market, clear[0]);
        assert_eq!(event.date_raw, row["date_raw_int"].as_u64().unwrap() as u32);
        assert_eq!(event.category, clear[12]);
        assert_eq!(
            security.split('.').nth(1).unwrap(),
            event.code,
            "{security} {}",
            row["date"].as_str().unwrap()
        );
        // Exact f32 bits preserved, no silent mutation of the raw value.
        for (index, raw) in event.c_raw.iter().enumerate() {
            let expected = u32::from_str_radix(
                row["float32_raw_bits_hex"][index]
                    .as_str()
                    .unwrap()
                    .trim_start_matches("0x"),
                16,
            )
            .unwrap();
            assert_eq!(
                raw.to_bits(),
                expected,
                "{security} {} C{}",
                row["date"].as_str().unwrap(),
                index + 1
            );
        }
        // Business-decimal 2dp HALF_UP factors.
        let factors = event.factors().unwrap();
        let oracle = row["candidates"]["fixed_2dp"]["C1_C2_C3_C4"]
            .as_array()
            .unwrap();
        for (index, factor) in factors.iter().enumerate() {
            assert_eq!(
                *factor,
                dec(oracle[index].as_str().unwrap()),
                "{security} {} normalized C{}",
                row["date"].as_str().unwrap(),
                index + 1
            );
        }
    }
}

#[test]
fn reference_price_matches_regression_vectors_for_all_events() {
    let regression: Value = serde_json::from_str(REGRESSION).unwrap();
    let events: std::collections::HashMap<(String, String), gbbq::Event> = rows()
        .into_iter()
        .map(|(security, row)| {
            (
                (security, row["date"].as_str().unwrap().to_string()),
                event_from_row(&row),
            )
        })
        .collect();
    let mut checked = 0usize;
    for row in regression["per_event"].as_array().unwrap() {
        let key = (
            row["security"].as_str().unwrap().to_string(),
            row["date"].as_str().unwrap().to_string(),
        );
        let event = events.get(&key).unwrap_or_else(|| panic!("{key:?}"));
        let factors = event.factors().unwrap();
        for (index, expected) in row["c_normalized"].as_array().unwrap().iter().enumerate() {
            assert_eq!(
                factors[index],
                dec(expected.as_str().unwrap()),
                "{key:?} C{index}"
            );
        }
        let prev_close = dec(row["prev_close"].as_str().unwrap());
        let reference = gbbq::reference_price(prev_close, &factors);
        let expected_ref = dec(row["ref"].as_str().unwrap());
        // Long decimal expansions may differ in the last digit between
        // rust_decimal and Python's 28-digit context; the business claim is
        // the cent, and the pre-cent value must agree to 1e-9.
        assert!(
            (reference - expected_ref).abs() < dec("0.000000001"),
            "{key:?} ref {reference} vs {expected_ref}"
        );
        assert_eq!(
            gbbq::displayed_previous_close(prev_close, &factors),
            dec(row["ref_cent"].as_str().unwrap()),
            "{key:?} ref_cent"
        );
        checked += 1;
    }
    assert_eq!(checked, 94);
}

#[test]
fn client_anchor_references_reproduce() {
    // #167 official-client transcriptions carried in the #169 regression.
    let regression: Value = serde_json::from_str(REGRESSION).unwrap();
    let events: std::collections::HashMap<(String, String), gbbq::Event> = rows()
        .into_iter()
        .map(|(security, row)| {
            (
                (security, row["date"].as_str().unwrap().to_string()),
                event_from_row(&row),
            )
        })
        .collect();
    let mut anchored = 0usize;
    for row in regression["per_event"].as_array().unwrap() {
        let Some(transcription) = row["oracle"].get("client_transcription_166") else {
            continue;
        };
        let key = (
            row["security"].as_str().unwrap().to_string(),
            row["date"].as_str().unwrap().to_string(),
        );
        let factors = events[&key].factors().unwrap();
        let prev_close = dec(row["prev_close"].as_str().unwrap());
        assert_eq!(
            gbbq::reference_price(prev_close, &factors),
            dec(row["ref"].as_str().unwrap()),
            "{key:?} exact ref"
        );
        assert_eq!(
            gbbq::displayed_previous_close(prev_close, &factors),
            dec(transcription.as_str().unwrap()),
            "{key:?} client transcription"
        );
        anchored += 1;
    }
    // sh.600006 2025-07-17 (7.245 -> 7.25) and sh.600004 2026-07-24
    // (7.675 -> 7.68): the two boundary transcriptions from #166.
    assert_eq!(anchored, 2);
}

#[test]
fn forward_adjust_chain_reproduces_all_eight_client_fields() {
    let day = day_file::parse(SZ000002_DAY).unwrap();
    let mut events: Vec<(u32, [Decimal; 4])> = rows()
        .into_iter()
        .filter(|(security, _)| security == "sz.000002")
        .map(|(_, row)| {
            let event = event_from_row(&row);
            (event.date_raw, event.factors().unwrap())
        })
        .collect();
    events.sort_by_key(|(date, _)| *date);

    let regression: Value = serde_json::from_str(REGRESSION).unwrap();
    for bar in regression["client_oracle_bars"].as_array().unwrap() {
        if bar["security"].as_str().unwrap() != "sz.000002" {
            continue;
        }
        let ymd: u32 = bar["date"]
            .as_str()
            .unwrap()
            .replace('-', "")
            .parse()
            .unwrap();
        let raw_bar = day.iter().find(|r| r.date == ymd_date(ymd)).unwrap();
        // Events strictly after the bar date, oldest→newest, no rounding.
        let chain = gbbq::compose_chain(
            events
                .iter()
                .filter(|(date, _)| *date > ymd)
                .map(|(_, c)| *c),
        );
        let expected_events = bar["n_events_in_chain"].as_u64().unwrap() as usize;
        assert_eq!(
            events.iter().filter(|(date, _)| *date > ymd).count(),
            expected_events
        );
        for (position, field) in ["O", "H", "L", "C"].iter().enumerate() {
            let raw_price = Decimal::from(raw_bar.ohlc_raw[position]) / Decimal::from(100);
            let predicted = cent(chain.apply(raw_price));
            let observed = dec(bar["fields"][field]["observed"].as_str().unwrap());
            assert_eq!(predicted, observed, "sz.000002 {ymd} {field}");
        }
    }
}

#[test]
fn exact_f32_factors_do_not_reproduce_the_chain() {
    // Anti-tautology guard: the official-client evidence excludes the
    // exact-f32 interpretation (only 7/8 fields matched).  This asserts the
    // oracle discriminates — if the chain accidentally passed without
    // normalization the test suite must notice.
    let day = day_file::parse(SZ000002_DAY).unwrap();
    let mut events: Vec<(u32, [Decimal; 4])> = rows()
        .into_iter()
        .filter(|(security, _)| security == "sz.000002")
        .map(|(_, row)| {
            let event = event_from_row(&row);
            let unnormalized = event
                .c_raw
                .map(|raw| Decimal::from_str(&f64::from(raw).to_string()).unwrap());
            (event.date_raw, unnormalized)
        })
        .collect();
    events.sort_by_key(|(date, _)| *date);
    let regression: Value = serde_json::from_str(REGRESSION).unwrap();
    let mut mismatches = 0usize;
    for bar in regression["client_oracle_bars"].as_array().unwrap() {
        if bar["security"].as_str().unwrap() != "sz.000002" {
            continue;
        }
        let ymd: u32 = bar["date"]
            .as_str()
            .unwrap()
            .replace('-', "")
            .parse()
            .unwrap();
        let raw_bar = day.iter().find(|r| r.date == ymd_date(ymd)).unwrap();
        let chain = gbbq::compose_chain(
            events
                .iter()
                .filter(|(date, _)| *date > ymd)
                .map(|(_, c)| *c),
        );
        for (position, field) in ["O", "H", "L", "C"].iter().enumerate() {
            let raw_price = Decimal::from(raw_bar.ohlc_raw[position]) / Decimal::from(100);
            if cent(chain.apply(raw_price))
                != dec(bar["fields"][field]["observed"].as_str().unwrap())
            {
                mismatches += 1;
            }
        }
    }
    assert!(mismatches > 0, "exact-f32 chain must not fully match");
}

fn ymd_date(ymd: u32) -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt((ymd / 10000) as i32, ymd / 100 % 100, ymd % 100).unwrap()
}
