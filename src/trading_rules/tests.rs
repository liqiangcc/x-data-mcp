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

#[test]
fn the_frozen_d44_book_loads_and_covers_every_derivable_board() {
    let book = rule_book().unwrap();
    assert_eq!(book.price_limits.len(), 17);
    assert_eq!(book.min_ticks.len(), 2);
    // On an ordinary seasoned day every board the read model can classify
    // resolves to exactly one verified row.
    for (market, board, band) in [
        (RuleMarket::Sh, RuleBoard::Main, 1_000),
        (RuleMarket::Sz, RuleBoard::Main, 1_000),
        (RuleMarket::Sz, RuleBoard::Gem, 2_000),
        (RuleMarket::Sh, RuleBoard::Star, 2_000),
    ] {
        match book.price_rule(
            market,
            board,
            StatusClass::Normal,
            ListingPhase::Seasoned,
            day(2026, 7, 10),
        ) {
            RuleLookup::Found(rule) => assert_eq!(rule.band_bp, Some(band), "{}", rule.rule_id),
            other => panic!("{market:?}/{board:?} did not resolve: {other:?}"),
        }
    }
}

#[test]
fn r2_is_two_rows_around_the_2026_07_06_switch() {
    let book = rule_book().unwrap();
    let last_5pct = book.price_rule(
        RuleMarket::Sh,
        RuleBoard::Main,
        StatusClass::St,
        ListingPhase::Seasoned,
        day(2026, 7, 5),
    );
    let first_10pct = book.price_rule(
        RuleMarket::Sh,
        RuleBoard::Main,
        StatusClass::St,
        ListingPhase::Seasoned,
        day(2026, 7, 6),
    );
    let (RuleLookup::Found(before), RuleLookup::Found(after)) = (last_5pct, first_10pct) else {
        panic!("both R2 segments must resolve on their own day");
    };
    assert_eq!(before.band_bp, Some(500));
    assert_eq!(after.band_bp, Some(1_000));
    assert_eq!(before.effective_to, Some(day(2026, 7, 6)));
    assert_ne!(before.rule_id, after.rule_id);
    // From the switch the ST and the normal band coincide, but that is a
    // value coincidence between two rows, not a status-agnostic rule: an
    // unknown ST status is still not answered from the normal row.
    let normal = book.price_rule(
        RuleMarket::Sh,
        RuleBoard::Main,
        StatusClass::Normal,
        ListingPhase::Seasoned,
        day(2026, 7, 6),
    );
    let RuleLookup::Found(normal) = normal else {
        panic!("the normal row must resolve");
    };
    assert_eq!(normal.band_bp, after.band_bp);
}

#[test]
fn nothing_covers_a_day_before_the_r7_boundary() {
    let book = rule_book().unwrap();
    let first = day(1996, 12, 16);
    assert!(
        book.price_limits
            .iter()
            .all(|rule| rule.effective_from >= first),
        "R7 is NOT_AVAILABLE: no price-limit row may start before 1996-12-16"
    );
    assert!(
        book.min_ticks
            .iter()
            .all(|rule| rule.effective_from >= first),
        "R7 is NOT_AVAILABLE: no min-tick row may start before 1996-12-16"
    );
    for as_of in [day(1990, 12, 19), day(1996, 12, 13)] {
        assert!(matches!(
            book.price_rule(
                RuleMarket::Sh,
                RuleBoard::Main,
                StatusClass::Normal,
                ListingPhase::Seasoned,
                as_of,
            ),
            RuleLookup::Missing
        ));
        assert!(matches!(
            book.min_tick(RuleMarket::Sh, RuleBoard::All, as_of),
            RuleLookup::Missing
        ));
    }
    assert!(matches!(
        book.price_rule(
            RuleMarket::Sh,
            RuleBoard::Main,
            StatusClass::Normal,
            ListingPhase::Seasoned,
            first,
        ),
        RuleLookup::Found(_)
    ));
}

#[test]
fn every_row_names_its_tier_and_inferred_rows_say_so() {
    let book = rule_book().unwrap();
    // The two tables have different row types; check the same invariant on
    // both without trying to chain their iterators.
    let check = |rule_id: &str, basis: &str| {
        assert!(
            basis.contains("APPROVED_SOURCE") || basis.contains("OWNER_RISK_ACCEPTED"),
            "{rule_id} has no provenance tier"
        );
        if basis.contains("推断") || basis.contains("行情锚定") {
            assert!(
                basis.contains("OWNER_RISK_ACCEPTED"),
                "{rule_id} is inferred/anchor-based but not owner-risk accepted"
            );
        }
    };
    for rule in &book.price_limits {
        check(&rule.rule_id, &rule.basis_ref);
    }
    for rule in &book.min_ticks {
        check(&rule.rule_id, &rule.basis_ref);
    }
    let sz_st = book
        .price_limits
        .iter()
        .find(|rule| rule.rule_id == "r2-sz-main-st-5pct-until-2026-07-05")
        .expect("the inferred SZ ST segment is encoded");
    assert!(sz_st.basis_ref.contains("推断"));
    assert!(sz_st.basis_ref.contains("OWNER_RISK_ACCEPTED"));
    let r1 = book
        .price_limits
        .iter()
        .find(|rule| rule.rule_id == "r1-sh-main-normal-10pct")
        .expect("R1 is encoded");
    assert!(r1.basis_ref.contains("行情锚定"));
    assert!(r1.basis_ref.contains("OWNER_RISK_ACCEPTED"));
}

#[test]
fn the_admission_checks_reject_early_rows_missing_boards_and_untiered_basis() {
    let book = rule_book().unwrap();
    validate_admitted_book(&book).unwrap();

    let mut early = book.price_limits[0].clone();
    early.rule_id = "early".into();
    early.effective_from = day(1990, 12, 19);
    early.effective_to = Some(day(1996, 12, 16));
    let mut with_early = book.clone();
    with_early.price_limits.push(early);
    assert!(
        validate_admitted_book(&with_early).is_err(),
        "a rule row before 1996-12-16 must be rejected"
    );

    let mut missing_star = book.clone();
    missing_star
        .price_limits
        .retain(|rule| rule.board != RuleBoard::Star);
    assert!(
        validate_admitted_book(&missing_star).is_err(),
        "an uncovered board must be rejected"
    );

    let mut untiered = book.clone();
    untiered.price_limits[0].basis_ref = "fixture".into();
    assert!(
        validate_admitted_book(&untiered).is_err(),
        "a basis_ref without a tier must be rejected"
    );

    let mut inferred = book.clone();
    inferred
        .price_limits
        .iter_mut()
        .find(|rule| rule.rule_id == "r2-sz-main-st-5pct-until-2026-07-05")
        .unwrap()
        .basis_ref = "tier=APPROVED_SOURCE; 推断".into();
    assert!(
        validate_admitted_book(&inferred).is_err(),
        "an inferred row without OWNER_RISK_ACCEPTED must be rejected"
    );
}

#[test]
fn the_rule_source_is_admitted_not_pending() {
    match rule_source().unwrap() {
        RuleSource::Admitted(book) => {
            assert_eq!(book.price_limits.len(), 17);
            assert!(
                book.price_limits
                    .iter()
                    .all(|rule| rule.basis_status == BasisStatus::Verified),
                "D-44 admits every encoded row"
            );
        }
        RuleSource::Pending(pending) => panic!("A-7 is admitted but got {pending:?}"),
    }
}
