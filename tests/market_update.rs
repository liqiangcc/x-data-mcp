use serde_json::{Value, json};
use x_data_mcp::{
    market_data::{CoverageEvidence, EvidenceValue, MarketDataBatch, Period},
    market_storage::compat::{self, CheckedRecord},
    market_update::{self, DateRange, Decision, Evidence, Reason},
};

fn raw() -> Value {
    let fixtures: Value =
        serde_json::from_str(include_str!("fixtures/issue79/legacy-records.json")).unwrap();
    fixtures[0]["record"].clone()
}
fn checked(mut raw: Value) -> CheckedRecord {
    raw["integrity_sha256"] = json!(compat::legacy_digest(&raw).unwrap());
    compat::check(&raw).unwrap()
}
fn inputs() -> (CheckedRecord, MarketDataBatch, DateRange) {
    let base = checked(raw());
    let mut candidate: MarketDataBatch =
        serde_json::from_value(base.raw()["batch"].clone()).unwrap();
    candidate.bars = vec![candidate.bars[1].clone()];
    let date = candidate.bars[0].date;
    (
        base,
        candidate,
        DateRange {
            start: date,
            end: date,
        },
    )
}
fn proof(base: &CheckedRecord, candidate: &MarketDataBatch, range: &DateRange) -> Evidence {
    Evidence {
        base_version_id: base.version().version_id.clone(),
        base_integrity_sha256: base.raw()["integrity_sha256"].as_str().unwrap().into(),
        candidate_sha256: market_update::candidate_digest(candidate).unwrap(),
        range: range.clone(),
        same_qfq_basis_ref: "fixture:verified-compatible-basis".into(),
        revision_ref: Some("fixture:verified-change".into()),
    }
}
#[test]
fn update_preserves_both_sides_and_tracks_years_without_publishing() {
    let (base, mut batch, range) = inputs();
    batch.bars[0].close = "10.06".parse().unwrap();
    let result =
        market_update::prepare(&base, &batch, &range, Some(&proof(&base, &batch, &range))).unwrap();
    let Decision::Candidate(result) = result else {
        panic!("expected combined candidate")
    };
    assert_eq!(result.records.len(), 3);
    assert_eq!(
        result.candidate_sha256,
        market_update::candidate_digest(&batch).unwrap()
    );
    assert_eq!(result.requested_range, range);
    assert_eq!(
        result.same_qfq_basis_ref,
        "fixture:verified-compatible-basis"
    );
    assert_eq!(result.base_integrity_sha256, base.raw()["integrity_sha256"]);
    assert_eq!(result.records[0], base.version().records[0]);
    assert_eq!(result.records[2], base.version().records[2]);
    assert_eq!(result.changed_dates, vec![range.start]);
    assert_eq!(
        result.affected_years.into_iter().collect::<Vec<_>>(),
        vec![2025]
    );
    assert_eq!(base.version().records[1].close.to_string(), "10.05");
}
#[test]
fn update_refuses_whole_item_when_old_in_range_date_is_missing() {
    let (base, batch, mut range) = inputs();
    range.end = "2025-12-31".parse().unwrap();
    assert_eq!(
        market_update::prepare(&base, &batch, &range, Some(&proof(&base, &batch, &range))).unwrap(),
        Decision::NeedsVerification(Reason::MissingOldDates(vec![range.end]))
    );
}
#[test]
fn update_decimal_format_change_reuses_fixed_version() {
    let (base, mut batch, range) = inputs();
    batch.bars[0].close = "10.05000".parse().unwrap();
    let mut evidence = proof(&base, &batch, &range);
    evidence.revision_ref = None;
    assert_eq!(
        market_update::prepare(&base, &batch, &range, Some(&evidence)).unwrap(),
        Decision::NoChange {
            version_id: base.version().version_id.clone()
        }
    );
}
#[test]
fn update_requires_bound_basis_and_actual_revision_evidence() {
    let (base, mut batch, range) = inputs();
    assert_eq!(
        market_update::prepare(&base, &batch, &range, None).unwrap(),
        Decision::NeedsVerification(Reason::BasisUnknown)
    );
    batch.bars[0].close = "10.050000000000000001".parse().unwrap();
    let mut e = proof(&base, &batch, &range);
    e.revision_ref = None;
    assert_eq!(
        market_update::prepare(&base, &batch, &range, Some(&e)).unwrap(),
        Decision::NeedsVerification(Reason::RevisionUnverified(vec![range.start]))
    );
    for field in 0..5 {
        let mut e = proof(&base, &batch, &range);
        match field {
            0 => e.base_version_id = "other".into(),
            1 => e.base_integrity_sha256 = "wrong".into(),
            2 => e.candidate_sha256 = "wrong".into(),
            3 => e.range.start = "2024-01-01".parse().unwrap(),
            _ => e.same_qfq_basis_ref.clear(),
        }
        assert_eq!(
            market_update::prepare(&base, &batch, &range, Some(&e)).unwrap(),
            Decision::NeedsVerification(Reason::BasisUnknown)
        );
    }
}
#[test]
fn update_rejects_known_value_loss_even_with_revision_reference() {
    for field in ["volume", "amount"] {
        let mut r = raw();
        r["records"][1][field] = json!("100");
        r["batch"]["bars"][1][field] = json!("100");
        let units = if field == "volume" {
            vec![("volume_unit", "shares")]
        } else {
            vec![("amount_currency", "CNY"), ("amount_unit", "yuan")]
        };
        for (key, value) in units {
            let unit = serde_json::to_value(EvidenceValue::confirmed(value, "fixture")).unwrap();
            r["unit_currency"][key] = unit.clone();
            r["batch"]["unit_currency"][key] = unit;
        }
        let base = checked(r);
        let (_, batch, range) = inputs();
        assert_eq!(
            market_update::prepare(&base, &batch, &range, Some(&proof(&base, &batch, &range)))
                .unwrap(),
            Decision::NeedsVerification(Reason::KnownValueLost(vec![range.start]))
        );
    }
}
#[test]
fn update_rejects_unknown_coverage_and_conflicting_units() {
    let (base, mut batch, range) = inputs();
    batch.coverage = CoverageEvidence::unknown("not established");
    assert_eq!(
        market_update::prepare(&base, &batch, &range, None).unwrap(),
        Decision::NeedsVerification(Reason::CoverageUnknown)
    );
    batch.coverage = CoverageEvidence::complete("fixture");
    batch.unit_currency.price_unit = EvidenceValue::confirmed("fen", "fixture");
    assert_eq!(
        market_update::prepare(&base, &batch, &range, None).unwrap(),
        Decision::NeedsVerification(Reason::UnitsIncompatible)
    );
}
#[test]
fn update_rejects_invalid_identity_period_range_and_conflicting_duplicate() {
    let (base, batch, range) = inputs();
    for case in 0..7 {
        let mut b = batch.clone();
        let mut r = range.clone();
        match case {
            0 => b.symbol = "sz.000001".into(),
            1 => b.period = Period::Year,
            2 => r.end = "2024-01-01".parse().unwrap(),
            3 => b.bars[0].date = "2026-01-01".parse().unwrap(),
            4 => b.provenance.source_period = "year".into(),
            5 => {
                let mut other = b.bars[0].clone();
                other.close = "11".parse().unwrap();
                b.bars.push(other);
            }
            _ => b.provenance.source = "other".into(),
        }
        assert!(
            market_update::prepare(&base, &b, &r, None).is_err(),
            "case {case}"
        );
    }
}
#[test]
fn update_adds_explicit_new_date_without_deleting_old_rows() {
    let (base, mut batch, mut range) = inputs();
    range.start = "2026-01-02".parse().unwrap();
    range.end = range.start;
    batch.bars[0].date = range.start;
    let mut e = proof(&base, &batch, &range);
    e.revision_ref = None;
    let Decision::Candidate(result) =
        market_update::prepare(&base, &batch, &range, Some(&e)).unwrap()
    else {
        panic!("candidate")
    };
    assert_eq!(&result.records[..3], base.version().records.as_slice());
    assert_eq!(
        result.affected_years.into_iter().collect::<Vec<_>>(),
        vec![2026]
    );
}

#[test]
fn update_empty_response_with_old_dates_is_needs_verification() {
    let (base, mut batch, range) = inputs();
    batch.bars.clear();
    assert_eq!(
        market_update::prepare(&base, &batch, &range, None).unwrap(),
        Decision::NeedsVerification(Reason::MissingOldDates(vec![range.start]))
    );
}
