//! Decision-table tests for the delisting check (M3 PR-5, `#446` §4.5) and the
//! evidence/item identity it freezes. The end-to-end path (real series rows,
//! real events, real task outcome) lives in `tests/mcp/plan_run.rs`.
use super::*;

fn registration() -> Registration {
    Registration {
        entity: "sh.600001".into(),
        record_count: 100,
        first_day: 1000,
        last_day: 1100,
        content_digest: "a".repeat(64),
        last_event_seq: 7,
        delisting_day: 1100,
    }
}

fn observed(registration: &Registration) -> Observed {
    Observed {
        record_count: registration.record_count,
        first_day: registration.first_day,
        last_day: registration.last_day,
        content_digest: registration.content_digest.clone(),
        last_event_seq: registration.last_event_seq,
    }
}

fn kinds(kinds: &[&str]) -> impl FnOnce() -> Result<Vec<String>, Error> {
    let kinds: Vec<String> = kinds.iter().map(|kind| (*kind).to_string()).collect();
    move || Ok(kinds)
}

fn verdict(observed: &Observed, events: &[&str]) -> Verdict {
    classify(&registration(), observed, kinds(events)).unwrap()
}

/// A registration whose delisting day still leaves room for a tail: the
/// fixture entry above is delisted on its last stored day, so nothing may
/// follow it.
fn open_registration() -> Registration {
    let mut registration = registration();
    registration.delisting_day = registration.last_day + 30;
    registration
}

fn tail_verdict(observed: &Observed, events: &[&str]) -> Verdict {
    classify(&open_registration(), observed, kinds(events)).unwrap()
}

#[test]
fn identical_facts_are_unchanged_without_reading_events() {
    let registration = registration();
    let seen = observed(&registration);
    assert_eq!(
        classify(&registration, &seen, || Err(Error::Integrity(
            "events must not be read".into()
        )))
        .unwrap(),
        Verdict::Unchanged
    );
}

#[test]
fn a_vanished_or_rewritten_series_fails_with_a_distinct_reason() {
    // History rewritten at the start: the first stored bar moved.
    let mut seen = observed(&registration());
    seen.first_day -= 1;
    assert_eq!(verdict(&seen, &[]), Verdict::Failed(FIRST_DAY_CHANGED));
    // History lost bars: fewer rows, or a last day that went backwards.
    let mut seen = observed(&registration());
    seen.record_count -= 1;
    assert_eq!(verdict(&seen, &[]), Verdict::Failed(HISTORY_SHRUNK));
    let mut seen = observed(&registration());
    seen.last_day -= 1;
    seen.record_count -= 1;
    assert_eq!(verdict(&seen, &[]), Verdict::Failed(HISTORY_SHRUNK));
    // Same span, different content: an in-place rewrite (the "摘要变化" case).
    let mut seen = observed(&registration());
    seen.content_digest = "b".repeat(64);
    assert_eq!(verdict(&seen, &[]), Verdict::Failed(CONTENT_DIGEST_CHANGED));
    // A delisted code cannot trade again: a bar past the delisting day is a
    // failure even with a perfectly recorded append.
    let mut seen = observed(&registration());
    seen.last_day = registration().delisting_day + 1;
    seen.record_count += 1;
    assert_eq!(
        verdict(&seen, &["append"]),
        Verdict::Failed(LAST_DAY_BEYOND_DELISTING)
    );
}

#[test]
fn a_registration_already_past_the_delisting_day_fails_even_when_unchanged() {
    // The registered state is impossible on its own: a delisted code cannot
    // own a bar after its delisting day, so "nothing changed since" must not
    // turn it into a clean `unchanged`.
    let mut registration = registration();
    registration.delisting_day = registration.last_day - 1;
    let seen = observed(&registration);
    assert_eq!(
        classify(&registration, &seen, || Err(Error::Integrity(
            "events must not be read".into()
        )))
        .unwrap(),
        Verdict::Failed(LAST_DAY_BEYOND_DELISTING)
    );
}

#[test]
fn a_recorded_tail_append_is_the_one_allowed_change() {
    let mut seen = observed(&registration());
    seen.last_day += 1;
    seen.record_count += 1;
    assert_eq!(tail_verdict(&seen, &["append"]), Verdict::TailExtended);
    // Two appended bars in one run are still one tail.
    let mut seen = observed(&registration());
    seen.last_day += 3;
    seen.record_count += 3;
    assert_eq!(
        tail_verdict(&seen, &["append", "append"]),
        Verdict::TailExtended
    );
    // A replace event (or an append after a rewrite) is not a tail.
    assert_eq!(
        tail_verdict(&seen, &["replace"]),
        Verdict::Failed(HISTORY_REWRITTEN)
    );
    assert_eq!(
        tail_verdict(&seen, &["append", "replace"]),
        Verdict::Failed(HISTORY_REWRITTEN)
    );
    // Facts that changed with no recorded transition are corruption, not a
    // tail: the commit path always writes an event.
    assert_eq!(
        tail_verdict(&seen, &[]),
        Verdict::Failed(TRANSITION_UNRECORDED)
    );
}

#[test]
fn failures_name_the_entities_and_carry_both_states() {
    let registration = registration();
    let mut seen = observed(&registration);
    seen.content_digest = "b".repeat(64);
    let changed = failure(&registration, CONTENT_DIGEST_CHANGED, Some(seen.clone()));
    let report = Report {
        registered: 2,
        unchanged: 1,
        tail_extended: Vec::new(),
        failures: vec![changed],
    };
    assert!(report.failed());
    assert_eq!(
        report.failure_message(),
        "delisted series check failed for 1 entity: sh.600001(content_digest_changed)"
    );
    let value = serde_json::to_value(&report).unwrap();
    assert_eq!(value["failures"][0]["entity"], "sh.600001");
    assert_eq!(value["failures"][0]["reason"], "content_digest_changed");
    assert_eq!(value["failures"][0]["registered"]["record_count"], 100);
    assert_eq!(
        value["failures"][0]["observed"]["content_digest"],
        "b".repeat(64)
    );
    // A missing series keeps `observed: null` instead of inventing facts.
    let gone = failure(&registration, MISSING_SERIES, None);
    assert!(gone.observed.is_none());
}

#[test]
fn many_failures_are_named_up_to_a_cap() {
    let mut failures = Vec::new();
    for code in 0..25 {
        let mut registration = registration();
        registration.entity = format!("sh.6000{code:02}");
        failures.push(failure(&registration, MISSING_SERIES, None));
    }
    // `check` sorts by entity before reporting; the cap keeps the first 20 of
    // that order and counts the rest.
    failures.sort_by(|a, b| a.entity.cmp(&b.entity));
    let report = Report {
        registered: failures.len(),
        unchanged: 0,
        tail_extended: Vec::new(),
        failures,
    };
    let message = report.failure_message();
    assert!(message.starts_with("delisted series check failed for 25 entities: "));
    assert!(message.ends_with(" (+5 more)"));
    assert!(message.contains("sh.600019(missing_series)"));
    assert!(!message.contains("sh.600020"), "{message}");
}

#[test]
fn a_clean_report_says_nothing() {
    let report = Report {
        registered: 0,
        unchanged: 0,
        tail_extended: Vec::new(),
        failures: Vec::new(),
    };
    assert!(!report.failed());
}

#[test]
fn the_baseline_round_trips_through_frozen_inputs() {
    let baseline = Baseline {
        as_of: chrono::NaiveDate::from_ymd_opt(2026, 10, 28).unwrap(),
        registrations: vec![registration()],
    };
    let value = serde_json::to_value(&baseline).unwrap();
    assert_eq!(value["as_of"], "2026-10-28");
    assert_eq!(decode_baseline(&value).unwrap(), baseline);
    // Corruption fails closed instead of reading as "nothing to check".
    for broken in [
        serde_json::json!({}),
        serde_json::json!({"as_of":"2026-10-28","registrations":[],"extra":1}),
        serde_json::json!({"as_of":"2026-10-28"}),
    ] {
        assert!(decode_baseline(&broken).is_err(), "{broken}");
    }
}

#[test]
fn the_item_spec_freezes_the_baseline_and_owns_a_read_only_scope() {
    let generation = serde_json::json!({"manifest_sha256": "g"});
    let baseline = Baseline {
        as_of: chrono::NaiveDate::from_ymd_opt(2026, 10, 28).unwrap(),
        registrations: vec![registration()],
    };
    let skipped = vec![serde_json::json!({"entity":"sh.600002","reason":"no_series"})];
    let item = item_spec(&generation, &baseline, &skipped);
    assert_eq!(item.item_id, task_control::derived::DELISTED_SERIES_CHECK);
    assert_eq!(item.target, task_control::derived::target(&item.item_id));
    assert_eq!(item.progress_unit, PROGRESS_UNIT);
    assert_eq!(
        item.scope_keys,
        std::collections::BTreeSet::from([task_control::derived::delisted_scope()])
    );
    assert_eq!(
        item.frozen_inputs[crate::data_supply::generation::SOURCE_GENERATION_KEY],
        generation
    );
    assert_eq!(
        decode_baseline(&item.frozen_inputs[BASELINE_KEY]).unwrap(),
        baseline
    );
    assert_eq!(item.frozen_inputs[SKIPPED_KEY], serde_json::json!(skipped));
}
