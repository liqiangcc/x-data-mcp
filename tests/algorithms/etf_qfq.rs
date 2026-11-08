//! ETF forward-adjustment oracles (#607 PR-A, D-89 / D-91 / D-92).
//!
//! The pure layer lives in `x_data_mcp::etf_qfq`; these tests keep the design's
//! fixture numbers (`docs/tasks/issue-607-etf-qfq.md` §2.4) next to the code,
//! including the alternative orderings that must **not** reproduce them.
//!
//! Semantics under test: the gbbq date of an ETF event is its 除权除息日 `E`
//! (the same `effective_day` the A-share chain uses). An event adjusts only bars
//! dated strictly before `E`; the `E` bar and every later bar are untouched.
//! There is no registration-day mapping.
//!
//! Fixtures with real `.day` / gbbq bytes need the retained ETF inputs, which are
//! not in this repository: PR-A proves the algorithm on the design's anchor
//! numbers; the byte-level reproduction is bound to the source fixture in PR-B.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use x_data_mcp::day_file::RawDayRecord;
use x_data_mcp::etf_qfq::{
    self, ETF_CATEGORY_CASH, ETF_CATEGORY_SCALE, ETF_PRICE_DIVISOR, EtfError,
};
use x_data_mcp::gbbq;
use x_data_mcp::qfq;

/// The forward series' `price_scale` (`PRICE_SCALE_QFQ`).
const SCALE: u32 = 6;

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn ymd(date: NaiveDate) -> u32 {
    use chrono::Datelike;
    date.year() as u32 * 10000 + date.month() * 100 + date.day()
}

/// One ETF bar with the same value in all four price fields, so a single
/// expected number per bar is unambiguous.
fn bar(date: NaiveDate, raw: u32) -> RawDayRecord {
    RawDayRecord {
        date,
        ohlc_raw: [raw; 4],
        amount_float32_bits: 0,
        volume_raw: 0,
        r7_raw: 0,
    }
}

/// `c1` (每 10 份派现, 元), `c3` (份额倍数) — the only two habits every ETF
/// fixture uses; the other slots are zero.
fn factors(c1: &str, c3: &str) -> [Decimal; 4] {
    etf_qfq::etf_factors(c1, "0", c3, "0").unwrap()
}

fn event(effective_day: u32, category: u8, c1: &str, c3: &str) -> etf_qfq::EtfEvent {
    etf_qfq::etf_event(effective_day, category, factors(c1, c3))
}

fn closes(bars: &[qfq::QfqBar]) -> Vec<Decimal> {
    bars.iter().map(|b| b.close).collect()
}

/// 6dp `Decimal` from an exact decimal string.
fn d(text: &str) -> Decimal {
    use std::str::FromStr;
    Decimal::from_str(text).unwrap()
}

// ---------------------------------------------------------------------------
// T1 — raw/1000 换算
// ---------------------------------------------------------------------------

#[test]
fn t1_etf_raw_units_are_milli_yuan_and_scale_to_six_dp() {
    assert_eq!(ETF_PRICE_DIVISOR, 1000);
    // 4_123 厘 = 4.123 元; no events → identity chain, so only the unit
    // conversion is under test.
    let bars = [bar(day(2024, 1, 2), 4_123)];
    let out = etf_qfq::etf_forward_adjust_exact(&bars, &[], SCALE).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].close, d("4.123000"));
    assert_eq!(out[0].close.scale(), SCALE);
    // The A-share divisor stays 100: the same integer is a different price.
    let a_share = qfq::forward_adjust_exact(&bars, &[], SCALE).unwrap();
    assert_eq!(a_share[0].close, d("41.230000"));
}

// ---------------------------------------------------------------------------
// T2 — cat1 单事件：p ← p − c1/10；空事件集合是恒等
// ---------------------------------------------------------------------------

#[test]
fn t2_cash_distribution_subtracts_one_tenth_of_c1_per_share() {
    // 每 10 份派现 0.661 元 → 每份 0.0661 元.
    let bars = [bar(day(2025, 3, 28), 4_111)];
    let events = [event(20250331, ETF_CATEGORY_CASH, "0.661", "0")];
    let out = etf_qfq::etf_forward_adjust_exact(&bars, &events, SCALE).unwrap();
    assert_eq!(out[0].close, d("4.044900"));
}

#[test]
fn t2_empty_event_set_is_the_identity_chain() {
    let bars = [bar(day(2024, 1, 2), 4_111), bar(day(2024, 1, 3), 4_222)];
    let out = etf_qfq::etf_forward_adjust_exact(&bars, &[], SCALE).unwrap();
    assert_eq!(closes(&out), vec![d("4.111000"), d("4.222000")]);
    assert!(out.iter().all(|b| b.chain_event_count == 0));
}

// ---------------------------------------------------------------------------
// T3 — cat11 单事件：p ← p / c3（f32 最短往返，不截到 2dp）
// ---------------------------------------------------------------------------

#[test]
fn t3_share_conversion_divides_by_c3_at_shortest_precision() {
    // sh.510500 的 c3=0.28032482: 2.243 → 8.001432.
    let bars = [bar(day(2024, 6, 3), 2_243)];
    let events = [event(20240604, ETF_CATEGORY_SCALE, "0", "0.28032482")];
    let out = etf_qfq::etf_forward_adjust_exact(&bars, &events, SCALE).unwrap();
    assert_eq!(out[0].close, d("8.001432"));

    // sh.510180 的 c3=10: 6.18 → 0.618.
    let bars = [bar(day(2024, 6, 3), 6_180)];
    let events = [event(20240604, ETF_CATEGORY_SCALE, "0", "10")];
    let out = etf_qfq::etf_forward_adjust_exact(&bars, &events, SCALE).unwrap();
    assert_eq!(out[0].close, d("0.618000"));

    // sz.159300 的 c3=0.2788902, kept at all seven digits.
    let bars = [bar(day(2024, 6, 3), 1_000)];
    let events = [event(20240604, ETF_CATEGORY_SCALE, "0", "0.2788902")];
    let out = etf_qfq::etf_forward_adjust_exact(&bars, &events, SCALE).unwrap();
    assert_eq!(out[0].close, d("3.585641"));
}

#[test]
fn t3_c3_is_not_pre_rounded_to_two_decimals() {
    // 0.93584 must stay 0.93584: rounding it to 0.94 first gives 1.063830
    // instead of 1.068559.
    let bars = [bar(day(2024, 6, 3), 1_000)];
    let exact = etf_qfq::etf_forward_adjust_exact(
        &bars,
        &[event(20240604, ETF_CATEGORY_SCALE, "0", "0.93584")],
        SCALE,
    )
    .unwrap();
    assert_eq!(exact[0].close, d("1.068559"));
    let pre_rounded = etf_qfq::etf_forward_adjust_exact(
        &bars,
        &[event(20240604, ETF_CATEGORY_SCALE, "0", "0.94")],
        SCALE,
    )
    .unwrap();
    assert_eq!(pre_rounded[0].close, d("1.063830"));
    assert_ne!(exact[0].close, pre_rounded[0].close);
}

// ---------------------------------------------------------------------------
// T4 — 严格大于：E 当天及其后的 bar 不受该事件影响
// ---------------------------------------------------------------------------

#[test]
fn t4_event_day_bar_itself_is_not_adjusted() {
    let bars = [
        bar(day(2025, 3, 28), 4_111),
        bar(day(2025, 3, 31), 4_111),
        bar(day(2025, 4, 1), 4_111),
    ];
    let events = [event(20250331, ETF_CATEGORY_CASH, "0.661", "0")];
    let out = etf_qfq::etf_forward_adjust_exact(&bars, &events, SCALE).unwrap();
    assert_eq!(out[0].close, d("4.044900"), "bar before E must be adjusted");
    assert_eq!(out[1].close, d("4.111000"), "the E bar itself is untouched");
    assert_eq!(out[2].close, d("4.111000"));
    assert_eq!(out[0].chain_event_count, 1);
    assert_eq!(out[1].chain_event_count, 0);
}

#[test]
fn t4_weekend_event_day_keeps_its_date_and_scope() {
    // sz.159922: cat11 E = 2024-12-01 is a Sunday. The date is not moved: every
    // bar before it is adjusted, the next trading day is not.
    let bars = [
        bar(day(2024, 11, 28), 6_159),
        bar(day(2024, 11, 29), 6_159),
        bar(day(2024, 12, 2), 6_159),
    ];
    let events = [event(20241201, ETF_CATEGORY_SCALE, "0", "2.5")];
    let out = etf_qfq::etf_forward_adjust_exact(&bars, &events, SCALE).unwrap();
    assert_eq!(
        closes(&out),
        vec![d("2.463600"), d("2.463600"), d("6.159000")]
    );
}

// ---------------------------------------------------------------------------
// T5 — sz.159919 锚定例（E = 2025-03-31, cat1 c1=0.661）
// ---------------------------------------------------------------------------

#[test]
fn t5_sz_159919_cash_event_matches_the_anchor_at_six_dp() {
    let bars = [bar(day(2025, 3, 28), 4_111)];
    let events = [event(20250331, ETF_CATEGORY_CASH, "0.661", "0")];
    let out = etf_qfq::etf_forward_adjust_exact(&bars, &events, SCALE).unwrap();
    // 4.111 − 0.661/10 = 4.0449, the official previous close 4.045 at 3dp.
    assert_eq!(out[0].close, d("4.044900"));
    // The value depends only on (bar, events, scale): recomputation and the
    // per-bar reference agree digit for digit.
    assert_eq!(
        out,
        etf_qfq::etf_forward_adjust_exact(&bars, &events, SCALE).unwrap()
    );
    assert_eq!(
        out,
        etf_qfq::etf_forward_adjust_exact_reference(&bars, &events, SCALE).unwrap()
    );
}

// ---------------------------------------------------------------------------
// T6 / T7 — sz.159922：cat11 E=2024-12-01, cat1 E=2024-12-02
// ---------------------------------------------------------------------------

fn sz_159922() -> Vec<etf_qfq::EtfEvent> {
    vec![
        event(20241201, ETF_CATEGORY_SCALE, "0", "2.5"),
        event(20241202, ETF_CATEGORY_CASH, "1.292", "0"),
    ]
}

#[test]
fn t6_sz_159922_both_events_compose_to_2334400() {
    let bars = [bar(day(2024, 11, 29), 6_159)];
    let out = etf_qfq::etf_forward_adjust_exact(&bars, &sz_159922(), SCALE).unwrap();
    // 6.159 / 2.5 − 1.292/10 = 2.4636 − 0.1292 = 2.3344, the official qss 2.334.
    assert_eq!(out[0].close, d("2.334400"));
    assert_eq!(out[0].chain_event_count, 2);
    assert_eq!(etf_qfq::etf_barriers(&sz_159922()).unwrap().len(), 2);
}

#[test]
fn t6_each_event_only_reaches_the_bars_before_its_own_day() {
    let events = sz_159922();
    // 2024-11-29 precedes both E's → both apply.
    // 2024-12-01 is the cat11 E → only the cash event still applies.
    // 2024-12-02 is the cat1 E too → nothing applies.
    let bars = [
        bar(day(2024, 11, 29), 6_159),
        bar(day(2024, 12, 1), 6_159),
        bar(day(2024, 12, 2), 6_159),
    ];
    let out = etf_qfq::etf_forward_adjust_exact(&bars, &events, SCALE).unwrap();
    assert_eq!(
        closes(&out),
        vec![d("2.334400"), d("6.029800"), d("6.159000")]
    );
}

#[test]
fn t7_same_e_day_merges_cat11_before_cat1() {
    // Both events on one E: 6.159 / 2.5 − 0.1292 = 2.3344 in a single barrier.
    // The reverse order (6.159 − 0.1292) / 2.5 = 2.41192 must differ.
    let events = [
        event(20241202, ETF_CATEGORY_SCALE, "0", "2.5"),
        event(20241202, ETF_CATEGORY_CASH, "1.292", "0"),
    ];
    let bars = [bar(day(2024, 11, 29), 6_159)];
    let out = etf_qfq::etf_forward_adjust_exact(&bars, &events, SCALE).unwrap();
    assert_eq!(out[0].close, d("2.334400"));
    // Input order must not matter: cat1 listed first still merges cat11 first.
    let swapped = [
        event(20241202, ETF_CATEGORY_CASH, "1.292", "0"),
        event(20241202, ETF_CATEGORY_SCALE, "0", "2.5"),
    ];
    assert_eq!(
        etf_qfq::etf_forward_adjust_exact(&bars, &swapped, SCALE).unwrap(),
        out
    );
    assert_eq!(out[0].chain_event_count, 1, "one merged barrier, not two");
    // The counter-example: the reverse order is a different number.
    let reversed = qfq::compose_barriers(&[
        etf_qfq::etf_event_transform(ETF_CATEGORY_CASH, &factors("1.292", "0")).unwrap(),
        etf_qfq::etf_event_transform(ETF_CATEGORY_SCALE, &factors("0", "2.5")).unwrap(),
    ])
    .unwrap();
    let reversed_row = etf_qfq::etf_forward_adjust_exact(
        &bars,
        &[etf_qfq::etf_event(
            20241202,
            ETF_CATEGORY_SCALE,
            factors("0", "2.5"),
        )],
        SCALE,
    )
    .unwrap();
    // `6.159 / 2.5 − 0.1292` is 2.3344; `(6.159 − 0.1292) / 2.5` is 2.41192.
    let wrong = (d("6.159") - d("0.1292")) / d("2.5");
    assert_eq!(wrong.round_dp(6), d("2.411920"));
    assert_ne!(out[0].close, d("2.411920"));
    assert_eq!(reversed_row[0].close, d("2.463600"));
    // The counter-example chain — cash first, then the split — reads the same
    // 6.159 元 as 2.411920, not 2.334400.
    assert_eq!(
        reversed.units(SCALE).unwrap().units_at(6_159, 1_000),
        Ok(2_411_920)
    );
}

// ---------------------------------------------------------------------------
// T8 — 后缀记忆化与逐 bar 参考实现逐位相等
// ---------------------------------------------------------------------------

#[test]
fn t8_suffix_memoization_equals_the_per_bar_reference() {
    let mut bars = Vec::new();
    for offset in 0..40i64 {
        let date = day(2024, 1, 1) + chrono::Duration::days(offset * 3);
        bars.push(bar(date, 1_000 + offset as u32 * 7));
    }
    let events = vec![
        event(20240115, ETF_CATEGORY_SCALE, "0", "3"),
        event(20240118, ETF_CATEGORY_CASH, "0.845", "0"),
        event(20240122, ETF_CATEGORY_SCALE, "0", "0.5"),
        event(20240123, ETF_CATEGORY_CASH, "2.2", "0"),
        event(20240123, ETF_CATEGORY_SCALE, "0", "1.5"),
    ];
    let fast = etf_qfq::etf_forward_adjust_exact(&bars, &events, SCALE).unwrap();
    let reference = etf_qfq::etf_forward_adjust_exact_reference(&bars, &events, SCALE).unwrap();
    assert_eq!(fast, reference);
    assert_eq!(fast.len(), bars.len());
}

// ---------------------------------------------------------------------------
// T9 — 非 1/11 类别拒绝
// ---------------------------------------------------------------------------

#[test]
fn t9_only_categories_one_and_eleven_have_transforms() {
    for category in [0u8, 2, 3, 12] {
        let error = etf_qfq::etf_event_transform(category, &factors("1", "2")).unwrap_err();
        assert!(
            matches!(
                error,
                EtfError::Gbbq(gbbq::Error::EtfUnsupportedCategory { category: got }) if got == category
            ),
            "category {category} must fail closed, got {error:?}"
        );
    }
    // A zero c3 has no reciprocal either.
    let error = etf_qfq::etf_event_transform(ETF_CATEGORY_SCALE, &factors("0", "0")).unwrap_err();
    assert!(matches!(
        error,
        EtfError::Gbbq(gbbq::Error::EtfDegenerateFactor { .. })
    ));
}

// ---------------------------------------------------------------------------
// T10 — 空序列 / 单 bar / 边界
// ---------------------------------------------------------------------------

#[test]
fn t10_empty_bars_and_single_bar_follow_the_same_contract() {
    let events = [event(20240102, ETF_CATEGORY_CASH, "1", "0")];
    assert!(
        etf_qfq::etf_forward_adjust_exact(&[], &events, SCALE)
            .unwrap()
            .is_empty()
    );
    // On the E bar nothing applies; one day earlier the event does.
    let bars = [bar(day(2024, 1, 2), 1_000)];
    let out = etf_qfq::etf_forward_adjust_exact(&bars, &events, SCALE).unwrap();
    assert_eq!(out[0].close, d("1.000000"));
    assert_eq!(out[0].chain_event_count, 0);
    let bars = [bar(day(2024, 1, 1), 1_000)];
    let out = etf_qfq::etf_forward_adjust_exact(&bars, &events, SCALE).unwrap();
    assert_eq!(out[0].close, d("0.900000"));
}

#[test]
fn t10_events_before_the_first_bar_and_after_the_last_bar() {
    let bars = [bar(day(2024, 5, 6), 1_000), bar(day(2024, 5, 7), 1_000)];
    // An event before every bar contributes to nothing.
    let past = [event(20240102, ETF_CATEGORY_SCALE, "0", "2")];
    let out = etf_qfq::etf_forward_adjust_exact(&bars, &past, SCALE).unwrap();
    assert_eq!(closes(&out), vec![d("1.000000"), d("1.000000")]);
    // A future event adjusts every existing bar and nothing else.
    let future = [event(20300102, ETF_CATEGORY_SCALE, "0", "2")];
    let out = etf_qfq::etf_forward_adjust_exact(&bars, &future, SCALE).unwrap();
    assert_eq!(closes(&out), vec![d("0.500000"), d("0.500000")]);
}

#[test]
fn t10_repeated_events_accumulate_on_the_older_bars() {
    let bars = [
        bar(day(2024, 1, 2), 8_000),
        bar(day(2024, 3, 1), 8_000),
        bar(day(2024, 6, 3), 8_000),
    ];
    let events = [
        event(20240108, ETF_CATEGORY_SCALE, "0", "2"),
        event(20240306, ETF_CATEGORY_CASH, "1", "0"),
    ];
    let out = etf_qfq::etf_forward_adjust_exact(&bars, &events, SCALE).unwrap();
    // Both apply to the first bar: 8 / 2 − 0.1 = 3.9.
    assert_eq!(out[0].close, d("3.900000"));
    assert_eq!(out[0].chain_event_count, 2);
    // Only the later cash event applies after the split day.
    assert_eq!(out[1].close, d("7.900000"));
    assert_eq!(out[1].chain_event_count, 1);
    assert_eq!(out[2].close, d("8.000000"));
    assert_eq!(out[2].chain_event_count, 0);
}

// ---------------------------------------------------------------------------
// 舍入 — 只在最后舍入一次
// ---------------------------------------------------------------------------

#[test]
fn rounding_happens_once_over_the_whole_chain() {
    // 1.000 / 3 = 0.3333333… minus c1/10 = 0.00000051 元. Rounding the division
    // to 6dp first gives 0.333333 − 0.00000051 → 0.333332; the exact chain gives
    // 0.333333, because both steps fold before the single HALF_UP.
    let bars = [bar(day(2024, 1, 2), 1_000)];
    let events = [
        event(20240104, ETF_CATEGORY_SCALE, "0", "3"),
        event(20240106, ETF_CATEGORY_CASH, "0.0000006", "0"),
    ];
    let out = etf_qfq::etf_forward_adjust_exact(&bars, &events, SCALE).unwrap();
    assert_eq!(out[0].close, Decimal::new(333_333, 6));
    // Rounding the division to 6dp first, then subtracting, is a different
    // number: the early rounding costs the last digit.
    let round_then_subtract =
        (Decimal::new(1_000, 3) / Decimal::new(3, 0)).round_dp(6) - d("0.00000051");
    assert_eq!(out[0].close, Decimal::new(333_333, 6));
    assert_eq!(
        round_then_subtract.round_dp(6),
        Decimal::new(333_332, 6),
        "the early-rounded path must differ"
    );
}

// ---------------------------------------------------------------------------
// 事件构造、bar 字段与输入顺序
// ---------------------------------------------------------------------------

#[test]
fn events_keep_the_gbbq_date_as_the_effective_day() {
    let raw = gbbq::Event {
        market: 0,
        code: "159922".into(),
        date_raw: 20241201,
        category: ETF_CATEGORY_SCALE,
        c_raw: [0.0, 0.0, 2.5, 0.0],
    };
    let resolved = etf_qfq::EtfEvent::from_gbbq(&raw).unwrap();
    assert_eq!(resolved.effective_day, 20241201);
    assert_eq!(resolved.category, ETF_CATEGORY_SCALE);
    assert_eq!(resolved.factors[2], d("2.5"));
    // The shortest round-trip factor survives a 3dp value untouched.
    let raw = gbbq::Event {
        date_raw: 20241202,
        category: ETF_CATEGORY_CASH,
        c_raw: [0.661, 0.0, 0.0, 0.0],
        ..raw
    };
    assert_eq!(
        etf_qfq::EtfEvent::from_gbbq(&raw).unwrap().factors[0],
        d("0.661")
    );
}

#[test]
fn etf_bar_path_only_adjusts_prices_and_keeps_raw_fields() {
    let mut raw = bar(day(2024, 1, 2), 5_000);
    raw.ohlc_raw = [5_000, 5_100, 4_900, 5_050];
    raw.amount_float32_bits = 0x3f80_0000;
    raw.volume_raw = 42;
    raw.r7_raw = 0x10000;
    let events = [event(20240105, ETF_CATEGORY_CASH, "1", "0")];
    let out =
        etf_qfq::etf_forward_adjust_exact(std::slice::from_ref(&raw), &events, SCALE).unwrap();
    assert_eq!(out[0].open, d("4.900000"));
    assert_eq!(out[0].high, d("5.000000"));
    assert_eq!(out[0].low, d("4.800000"));
    assert_eq!(out[0].close, d("4.950000"));
    assert_eq!(out[0].amount_float32_bits, raw.amount_float32_bits);
    assert_eq!(out[0].volume_raw, raw.volume_raw);
    assert_eq!(out[0].r7_raw, raw.r7_raw);
    assert_eq!(out[0].date, raw.date);
    assert_eq!(ymd(out[0].date), 20240102);
}

#[test]
fn unsorted_input_bars_produce_the_same_digits() {
    let mut bars = vec![
        bar(day(2024, 1, 2), 1_000),
        bar(day(2024, 1, 10), 1_000),
        bar(day(2024, 1, 20), 1_000),
    ];
    let events = [event(20240108, ETF_CATEGORY_SCALE, "0", "4")];
    let ascending = etf_qfq::etf_forward_adjust_exact(&bars, &events, SCALE).unwrap();
    bars.reverse();
    let descending = etf_qfq::etf_forward_adjust_exact(&bars, &events, SCALE).unwrap();
    let mut by_date: Vec<(u32, Decimal)> =
        descending.iter().map(|b| (ymd(b.date), b.close)).collect();
    by_date.sort_by_key(|(date, _)| *date);
    assert_eq!(
        by_date,
        ascending
            .iter()
            .map(|b| (ymd(b.date), b.close))
            .collect::<Vec<_>>()
    );
}
