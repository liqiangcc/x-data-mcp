//! Pure exact-decimal comparisons and one final rational half-even rounding.
use super::wire::*;
use crate::market_storage::Error;
use chrono::{Datelike, NaiveDate};
use num_bigint::BigInt;
use num_traits::{Signed, Zero};
use rust_decimal::Decimal;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    Complete,
    Incomplete,
    Unknown,
}
#[derive(Debug, Clone)]
pub struct Observation {
    pub year: i32,
    pub first: NaiveDate,
    pub last: NaiveDate,
    pub close: Decimal,
    pub unfinished: bool,
    pub completion: Completion,
}
impl Observation {
    fn point(&self) -> Point {
        Point {
            year: self.year,
            first_trading_date: self.first,
            last_trading_date: self.last,
            close: self.close.to_string(),
        }
    }
}
fn invalid(s: &str) -> Error {
    Error::Invalid(s.into())
}
pub fn evaluate(input: &[Observation], range: &Years, p: &Parameters) -> Result<Outcome, Error> {
    if range.start_year < 1
        || range.end_year > 9999
        || range.start_year > range.end_year
        || range.end_year - range.start_year >= 1000
        || input.len() > 1000
        || !(1..=1000).contains(&p.min_periods)
    {
        return Err(invalid("bounded years/observations/min_periods required"));
    }
    if input.windows(2).any(|w| w[0].year >= w[1].year)
        || input.iter().any(|o| {
            o.year < range.start_year
                || o.year > range.end_year
                || o.first > o.last
                || o.first.year() != o.year
                || o.last.year() != o.year
        })
    {
        return Err(invalid("unordered/duplicate/out-of-range observations"));
    }
    let mut excluded = vec![];
    let no = |reason, excluded_tail: Vec<i32>| Outcome::NotComputable {
        reason,
        excluded_tail,
        limitations: vec![
            "no inferred coverage, warmup, gap filling or version substitution".into(),
        ],
    };
    if input.last().is_some_and(|o| o.year != range.end_year) {
        return Ok(no(Reason::MissingYear, excluded));
    }
    let mut len = input.len();
    if p.completed_only {
        while len > 0 && input[len - 1].unfinished {
            excluded.push(input[len - 1].year);
            len -= 1;
        }
    }
    if len < 2 {
        return Ok(no(Reason::WindowTooShort, excluded));
    }
    let data = &input[..len];
    let end = &data[len - 1];
    let provisional = !p.completed_only && end.unfinished;
    let check = |o: &Observation| -> Option<Reason> {
        if o.close <= Decimal::ZERO {
            Some(Reason::NonPositivePrice)
        } else if o.completion == Completion::Unknown {
            Some(Reason::UnknownCompletion)
        } else if o.completion == Completion::Incomplete && !(provisional && o.year == end.year) {
            Some(Reason::IncompletePeriod)
        } else if o.unfinished && o.year != end.year {
            Some(Reason::IncompletePeriod)
        } else {
            None
        }
    };
    let mut run = 0;
    let mut anchor = len - 1;
    let mut kind = RunKind::LowerBound;
    let mut observed = None;
    for i in (1..len).rev() {
        let a = &data[i - 1];
        let b = &data[i];
        if b.year - a.year != 1 {
            return Ok(no(Reason::MissingYear, excluded));
        }
        if let Some(reason) = check(b).or_else(|| check(a)) {
            return Ok(no(reason, excluded));
        }
        let direction = match b.close.cmp(&a.close) {
            std::cmp::Ordering::Greater => ObservedDirection::Up,
            std::cmp::Ordering::Less => ObservedDirection::Down,
            std::cmp::Ordering::Equal => ObservedDirection::Flat,
        };
        if observed.is_none() {
            observed = Some(direction);
        }
        let matches = matches!(
            (p.direction, direction),
            (Direction::Up, ObservedDirection::Up) | (Direction::Down, ObservedDirection::Down)
        );
        if !matches {
            kind = RunKind::Exact;
            break;
        }
        run += 1;
        anchor = i - 1;
    }
    if kind == RunKind::LowerBound && data[0].year != range.start_year {
        return Ok(no(Reason::MissingYear, excluded));
    }
    let matches = if run >= p.min_periods {
        Some(true)
    } else if kind == RunKind::Exact {
        Some(false)
    } else {
        None
    };
    let cumulative_change = if run > 0 {
        Some(ratio_change(end.close, data[anchor].close)?)
    } else {
        None
    };
    Ok(Outcome::Computed {
        observed_direction: observed.unwrap(),
        run_length: run,
        run_length_kind: kind.clone(),
        matches,
        anchor: (run > 0).then(|| data[anchor].point()),
        first_change: (run > 0).then(|| data[anchor + 1].point()),
        end: end.point(),
        cumulative_change,
        precision: Precision {
            scale: 12,
            rounding: "half_even_once".into(),
            unit: "ratio".into(),
        },
        actual_input_range: Years {
            start_year: data[anchor].year,
            end_year: end.year,
        },
        excluded_tail: excluded,
        provisional,
        limitations: if kind == RunKind::LowerBound {
            vec![
                "at least the observed run; no observations outside the requested left boundary"
                    .into(),
            ]
        } else {
            vec![]
        },
    })
}
pub fn ratio_change(end: Decimal, anchor: Decimal) -> Result<String, Error> {
    if anchor <= Decimal::ZERO || end <= Decimal::ZERO {
        return Err(invalid("positive ratio inputs required"));
    }
    let ten = BigInt::from(10);
    let end_coeff = BigInt::from(end.mantissa()) * ten.pow(anchor.scale());
    let anchor_coeff = BigInt::from(anchor.mantissa()) * ten.pow(end.scale());
    let numerator = (end_coeff - &anchor_coeff) * ten.pow(12);
    let denominator = anchor_coeff;
    if numerator.bits() > 512 || denominator.bits() > 512 {
        return Err(invalid("rational integer budget exceeded"));
    }
    let negative = numerator.is_negative();
    let magnitude = numerator.abs();
    let mut quotient = &magnitude / &denominator;
    let remainder = &magnitude % &denominator;
    let double = &remainder * 2;
    let odd = (&quotient % 2u32) != BigInt::zero();
    if double > denominator || (double == denominator && odd) {
        quotient += 1;
    }
    let digits = quotient.to_string();
    let padded = format!("{:0>13}", digits);
    let split = padded.len() - 12;
    Ok(format!(
        "{}{}.{}",
        if negative && !quotient.is_zero() {
            "-"
        } else {
            ""
        },
        &padded[..split],
        &padded[split..]
    ))
}
