use serde_json::json;
use x_data_mcp::metrics::{
    consecutive::{Completion, Observation, evaluate, ratio_change},
    wire::*,
};
fn parameters(direction: &str) -> Parameters {
    serde_json::from_value(json!({"direction":direction,"min_periods":3})).unwrap()
}
fn observations(prices: &[&str]) -> Vec<Observation> {
    prices
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let year = 2020 + i as i32;
            Observation {
                year,
                first: chrono::NaiveDate::from_ymd_opt(year, 1, 2).unwrap(),
                last: chrono::NaiveDate::from_ymd_opt(year, 12, 30).unwrap(),
                close: p.parse().unwrap(),
                unfinished: false,
                completion: Completion::Complete,
            }
        })
        .collect()
}
fn run(data: &[Observation], p: &Parameters) -> Outcome {
    evaluate(
        data,
        &Years {
            start_year: 2020,
            end_year: 2020 + data.len() as i32 - 1,
        },
        p,
    )
    .unwrap()
}
#[test]
fn independent_down_vectors_distinguish_exact_lower_bound_and_zero() {
    for (prices, n, kind, matches, change) in [
        (
            vec!["100", "90", "80", "70"],
            3,
            RunKind::LowerBound,
            Some(true),
            Some("-0.300000000000"),
        ),
        (
            vec!["100", "90", "80"],
            2,
            RunKind::LowerBound,
            None,
            Some("-0.200000000000"),
        ),
        (
            vec!["80", "100", "90", "80", "70"],
            3,
            RunKind::Exact,
            Some(true),
            Some("-0.300000000000"),
        ),
        (
            vec!["100", "90", "90", "80"],
            1,
            RunKind::Exact,
            Some(false),
            Some("-0.111111111111"),
        ),
        (
            vec!["100", "90", "95"],
            0,
            RunKind::Exact,
            Some(false),
            None,
        ),
    ] {
        match run(&observations(&prices), &parameters("down")) {
            Outcome::Computed {
                run_length,
                run_length_kind,
                matches: m,
                cumulative_change,
                end,
                ..
            } => {
                assert_eq!(run_length, n);
                assert_eq!(run_length_kind, kind);
                assert_eq!(m, matches);
                assert_eq!(cumulative_change.as_deref(), change);
                assert_eq!(end.last_trading_date.day(), 30);
            }
            other => panic!("{other:?}"),
        }
    }
}
use chrono::Datelike;
#[test]
fn completed_only_and_provisional_do_not_skip_unknown_middle() {
    let mut data = observations(&["100", "90", "80", "70"]);
    data[3].unfinished = true;
    data[3].completion = Completion::Incomplete;
    assert!(matches!(
        run(&data, &parameters("down")),
        Outcome::Computed {
            run_length: 2,
            matches: None,
            provisional: false,
            ..
        }
    ));
    let mut p = parameters("down");
    p.completed_only = false;
    assert!(matches!(
        run(&data, &p),
        Outcome::Computed {
            run_length: 3,
            matches: Some(true),
            provisional: true,
            ..
        }
    ));
    data[2].completion = Completion::Unknown;
    assert!(matches!(
        run(&data, &p),
        Outcome::NotComputable {
            reason: Reason::UnknownCompletion,
            ..
        }
    ));
}
#[test]
fn gaps_nonpositive_unknown_and_short_windows_are_not_false_metrics() {
    let p = parameters("down");
    for completion in [Completion::Unknown, Completion::Incomplete] {
        let mut data = observations(&["100", "90", "80"]);
        data[1].completion = completion;
        assert!(matches!(run(&data, &p), Outcome::NotComputable { .. }));
    }
    let mut data = observations(&["100", "90", "80"]);
    data.remove(1);
    assert!(matches!(
        evaluate(
            &data,
            &Years {
                start_year: 2020,
                end_year: 2022
            },
            &p
        )
        .unwrap(),
        Outcome::NotComputable {
            reason: Reason::MissingYear,
            ..
        }
    ));
    assert!(matches!(
        run(&observations(&["100", "0"]), &p),
        Outcome::NotComputable {
            reason: Reason::NonPositivePrice,
            ..
        }
    ));
    assert!(matches!(
        run(&observations(&["100"]), &p),
        Outcome::NotComputable {
            reason: Reason::WindowTooShort,
            ..
        }
    ));
}
#[test]
fn up_symmetry_exact_decimal_equality_and_irrelevant_prefix() {
    assert!(matches!(
        run(&observations(&["70", "80", "90", "100"]), &parameters("up")),
        Outcome::Computed {
            run_length: 3,
            matches: Some(true),
            ..
        }
    ));
    assert!(matches!(
        run(
            &observations(&["1.000000000000000001", "1.0000000000000000010"]),
            &parameters("down")
        ),
        Outcome::Computed {
            run_length: 0,
            matches: Some(false),
            observed_direction: ObservedDirection::Flat,
            ..
        }
    ));
    let mut data = observations(&["50", "80", "100", "90", "80", "70"]);
    data[0].completion = Completion::Unknown;
    assert!(matches!(
        run(&data, &parameters("down")),
        Outcome::Computed {
            run_length: 3,
            run_length_kind: RunKind::Exact,
            ..
        }
    ));
}
#[test]
fn exact_rational_rounding_has_one_half_even_step_in_both_directions() {
    for (end, expected) in [
        ("1.0000000000005", "0.000000000000"),
        ("1.0000000000015", "0.000000000002"),
        ("0.9999999999995", "0.000000000000"),
        ("0.9999999999985", "-0.000000000002"),
    ] {
        assert_eq!(
            ratio_change(end.parse().unwrap(), "1".parse().unwrap()).unwrap(),
            expected
        );
    }
    assert_eq!(
        ratio_change("2".parse().unwrap(), "3".parse().unwrap()).unwrap(),
        "-0.333333333333"
    );
    assert!(
        ratio_change(
            rust_decimal::Decimal::MAX,
            "0.0000000000000000000000000001".parse().unwrap()
        )
        .is_ok()
    );
}
#[test]
fn duplicated_unordered_and_excessive_windows_are_rejected() {
    let p = parameters("down");
    let mut data = observations(&["100", "90"]);
    data.reverse();
    assert!(
        evaluate(
            &data,
            &Years {
                start_year: 2020,
                end_year: 2021
            },
            &p
        )
        .is_err()
    );
    assert!(
        evaluate(
            &[],
            &Years {
                start_year: 1,
                end_year: 1001
            },
            &p
        )
        .is_err()
    );
    let d = observations(&["100"]);
    assert!(
        evaluate(
            &[d[0].clone(), d[0].clone()],
            &Years {
                start_year: 2020,
                end_year: 2020
            },
            &p
        )
        .is_err()
    );
}
#[test]
fn actual_input_includes_the_break_observation_but_not_irrelevant_prefix() {
    let data = observations(&["50", "80", "100", "90", "80", "70"]);
    match run(&data, &parameters("down")) {
        Outcome::Computed {
            actual_input_range,
            observations_used,
            anchor: Some(anchor),
            ..
        } => {
            assert_eq!(actual_input_range.start_year, 2021);
            assert_eq!(anchor.year, 2022);
            assert_eq!(observations_used, 5);
        }
        other => panic!("{other:?}"),
    }
}
