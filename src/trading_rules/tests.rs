use chrono::NaiveDate;

use super::*;

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

fn rule(id: &str, status: StatusClass, phase: ListingPhase, band_bp: i64) -> PriceLimitRule {
    PriceLimitRule {
        rule_id: id.into(),
        market: RuleMarket::Sz,
        board: RuleBoard::Main,
        status_class: status,
        listing_phase: phase,
        band_bp: Some(band_bp),
        band_side: BandSide::Both,
        rounding: Rounding::HalfUp,
        effective_from: day(2020, 1, 1),
        effective_to: None,
        basis_ref: "fixture".into(),
        basis_status: BasisStatus::Verified,
    }
}

fn tick() -> MinTickRule {
    MinTickRule {
        rule_id: "tick".into(),
        market: RuleMarket::Sz,
        board: RuleBoard::Main,
        effective_from: day(2020, 1, 1),
        effective_to: None,
        tick_units: 1,
        price_scale: 2,
        basis_ref: "fixture".into(),
        basis_status: BasisStatus::Verified,
    }
}

fn book(rules: Vec<PriceLimitRule>) -> RuleBook {
    RuleBook {
        price_limits: rules,
        min_ticks: vec![tick()],
    }
}

#[test]
fn an_empty_book_validates_but_finds_nothing() {
    let empty = RuleBook::default();
    empty.validate().unwrap();
    assert!(matches!(
        empty.price_rule(
            RuleMarket::Sz,
            RuleBoard::Main,
            StatusClass::Normal,
            ListingPhase::Seasoned,
            day(2024, 1, 2),
        ),
        RuleLookup::Missing
    ));
}

#[test]
fn a_verified_row_is_found_and_the_interval_is_half_open() {
    let mut found = rule("r", StatusClass::Normal, ListingPhase::Seasoned, 1_000);
    found.effective_to = Some(day(2024, 1, 2));
    let book = book(vec![found]);
    book.validate().unwrap();
    assert!(matches!(
        book.price_rule(
            RuleMarket::Sz,
            RuleBoard::Main,
            StatusClass::Normal,
            ListingPhase::Seasoned,
            day(2024, 1, 1),
        ),
        RuleLookup::Found(_)
    ));
    assert!(matches!(
        book.price_rule(
            RuleMarket::Sz,
            RuleBoard::Main,
            StatusClass::Normal,
            ListingPhase::Seasoned,
            day(2024, 1, 2),
        ),
        RuleLookup::Missing
    ));
}

#[test]
fn a_pending_row_is_pending_not_found_and_not_missing() {
    let mut pending = rule("p", StatusClass::Normal, ListingPhase::Seasoned, 1_000);
    pending.basis_status = BasisStatus::Pending;
    let book = book(vec![pending]);
    book.validate().unwrap();
    assert!(matches!(
        book.price_rule(
            RuleMarket::Sz,
            RuleBoard::Main,
            StatusClass::Normal,
            ListingPhase::Seasoned,
            day(2024, 1, 2),
        ),
        RuleLookup::Pending
    ));
}

#[test]
fn a_specific_row_and_a_wildcard_row_are_ambiguous() {
    let specific = rule(
        "specific",
        StatusClass::Normal,
        ListingPhase::Seasoned,
        1_000,
    );
    let mut wildcard = rule(
        "wildcard",
        StatusClass::Normal,
        ListingPhase::Seasoned,
        2_000,
    );
    wildcard.market = RuleMarket::All;
    wildcard.board = RuleBoard::All;
    let book = book(vec![specific, wildcard]);
    // Overlap validation only rejects verified rows of the *same* exact
    // scope; wildcard overlap is the read model's fail-closed case.
    book.validate().unwrap();
    assert!(matches!(
        book.price_rule(
            RuleMarket::Sz,
            RuleBoard::Main,
            StatusClass::Normal,
            ListingPhase::Seasoned,
            day(2024, 1, 2),
        ),
        RuleLookup::Ambiguous(2)
    ));
}

#[test]
fn a_verified_overlap_in_one_scope_is_rejected() {
    let first = rule("a", StatusClass::Normal, ListingPhase::Seasoned, 1_000);
    let second = rule("b", StatusClass::Normal, ListingPhase::Seasoned, 2_000);
    let error = book(vec![first, second]).validate().unwrap_err();
    assert!(matches!(error, RuleError::Invalid(_)));
}

#[test]
fn scope_mismatches_do_not_match() {
    let book = book(vec![rule(
        "r",
        StatusClass::Normal,
        ListingPhase::Seasoned,
        1_000,
    )]);
    assert!(matches!(
        book.price_rule(
            RuleMarket::Sh,
            RuleBoard::Main,
            StatusClass::Normal,
            ListingPhase::Seasoned,
            day(2024, 1, 2),
        ),
        RuleLookup::Missing
    ));
    assert!(matches!(
        book.price_rule(
            RuleMarket::Sz,
            RuleBoard::Main,
            StatusClass::St,
            ListingPhase::Seasoned,
            day(2024, 1, 2),
        ),
        RuleLookup::Missing
    ));
    assert!(matches!(
        book.price_rule(
            RuleMarket::Sz,
            RuleBoard::Main,
            StatusClass::Normal,
            ListingPhase::First5Days,
            day(2024, 1, 2),
        ),
        RuleLookup::Missing
    ));
}

#[test]
fn min_tick_uses_the_market_and_board_dimensions() {
    let book = book(Vec::new());
    assert!(matches!(
        book.min_tick(RuleMarket::Sz, RuleBoard::Main, day(2024, 1, 2)),
        RuleLookup::Found(_)
    ));
    assert!(matches!(
        book.min_tick(RuleMarket::Sh, RuleBoard::Main, day(2024, 1, 2)),
        RuleLookup::Missing
    ));
}

#[test]
fn a_bounded_rule_without_a_band_is_rejected() {
    let mut bad = rule("bad", StatusClass::Normal, ListingPhase::Seasoned, 1_000);
    bad.band_bp = None;
    assert!(book(vec![bad]).validate().is_err());
}

#[test]
fn an_unbounded_rule_must_not_carry_a_band() {
    let mut bad = rule("bad", StatusClass::Normal, ListingPhase::Seasoned, 1_000);
    bad.band_side = BandSide::None;
    assert!(book(vec![bad]).validate().is_err());

    let mut good = rule("good", StatusClass::Normal, ListingPhase::FirstDay, 1_000);
    good.band_side = BandSide::None;
    good.band_bp = None;
    book(vec![good]).validate().unwrap();
}
