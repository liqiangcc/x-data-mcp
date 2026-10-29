//! Historical-industry interval tests (M3 PR-6, `#446` §5.1–§5.4):
//! 7 snapshot files with a repeated content → 6 boundaries, member
//! add/remove/reclassify, same-day collisions, the dictionary statuses,
//! `before_coverage`/`missing_in_snapshot`, and the guarantee that the daily
//! append path and a full rebuild agree.
use super::*;

fn day(year: i32, month: u32, day: u32) -> i64 {
    epoch_day(NaiveDate::from_ymd_opt(year, month, day).unwrap())
}

/// One `tdxhy.cfg` member: `market|code|t_code|||x_code`.
fn cfg(entries: &[(u8, &str, &str, &str)]) -> Vec<u8> {
    let mut out = String::new();
    for (market, code, t_code, x_code) in entries {
        out.push_str(&format!("{market}|{code}|{t_code}|||{x_code}\r\n"));
    }
    out.into_bytes()
}

fn dictionary(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(code, name)| ((*code).to_string(), (*name).to_string()))
        .collect()
}

fn snapshot(
    bytes: &[u8],
    publisher_last_modified: Option<&str>,
    observed: &str,
    grade: &str,
    dictionary_entries: Option<&[(&str, &str)]>,
) -> SnapshotFacts {
    let (row_count, members) = parse_members(bytes).unwrap();
    SnapshotRecord {
        member_sha256: crate::hash::sha256(bytes),
        store_sha256: crate::hash::sha256(bytes),
        base_zip_sha256: Some("b".repeat(64)),
        generation_manifest_sha256: Some("c".repeat(64)),
        source_kind: SourceKind::Generation,
        publisher_last_modified: publisher_last_modified.map(str::to_string),
        observed_at: Some(observed.to_string()),
        ingested_at: observed.to_string(),
        imported_at: None,
        evidence_grade: grade.to_string(),
        parser_version: "tdxhy-v1".to_string(),
        row_count,
        scope_member_count: i64::try_from(members.len()).unwrap(),
        dictionary_sha256: dictionary_entries.map(|_| "d".repeat(64)),
        notes: None,
    }
    .into_facts(members, dictionary_entries.map(dictionary))
}

impl SnapshotRecord {
    fn into_facts(
        self,
        members: Vec<IndustryMember>,
        dictionary: Option<BTreeMap<String, String>>,
    ) -> SnapshotFacts {
        SnapshotFacts {
            record: self,
            members,
            dictionary_present: dictionary.is_some(),
            dictionary: dictionary.unwrap_or_default(),
        }
    }
}

fn assignment<'a>(rows: &'a [AssignmentRow], exchange: &str, code: &str) -> Vec<&'a AssignmentRow> {
    rows.iter()
        .filter(|row| row.exchange == exchange && row.code == code)
        .collect()
}

const DICT: &[(&str, &str)] = &[("X5001", "软件服务"), ("X5002", "硬件")];

/// One `tdxhy.cfg` file: the scoped rows plus a Beijing (market 2) filler row.
/// The filler is out of scope, but it makes two files with the same members
/// two distinct contents — exactly like real `tdxhy.cfg` snapshots, whose bytes
/// always differ even when the scoped projection does not.
fn cfg_file(entries: &[(u8, &str, &str, &str)], filler: &str) -> Vec<u8> {
    let mut bytes = cfg(entries);
    bytes.extend_from_slice(format!("2|{filler}|T1001|||X500102\r\n").as_bytes());
    bytes
}

#[test]
fn seven_snapshots_with_a_repeated_content_are_six_boundaries() {
    // 7 files; 10-24 and 10-28 are byte-identical, so they are one snapshot
    // (§5.3) and the remaining days define 6 boundaries.
    let first = [
        (0u8, "000001", "T1001", "X500102"),
        (1, "600006", "T1001", "X500102"),
    ];
    let files: [(&[(u8, &str, &str, &str)], &str, &str, &str); 7] = [
        (
            &first,
            "830001",
            "Wed, 30 Sep 2026 08:00:00 GMT",
            "2026-09-30T09:00:00Z",
        ),
        (
            &[
                (0, "000001", "T1001", "X500102"),
                (1, "600006", "T1001", "X500202"),
            ],
            "830002",
            "Mon, 12 Oct 2026 08:00:00 GMT",
            "2026-10-12T09:00:00Z",
        ),
        (
            &[
                (0, "000001", "T1001", "X500102"),
                (0, "000002", "T1001", "X500102"),
                (1, "600006", "T1001", "X500202"),
            ],
            "830003",
            "Tue, 13 Oct 2026 08:00:00 GMT",
            "2026-10-13T09:00:00Z",
        ),
        (
            &[
                (0, "000001", "T1001", "X500102"),
                (1, "600006", "T1001", "X500202"),
            ],
            "830004",
            "Fri, 16 Oct 2026 08:00:00 GMT",
            "2026-10-16T09:00:00Z",
        ),
        (
            &[
                (0, "000001", "T1001", "X500102"),
                (1, "600006", "T1001", "X500102"),
            ],
            "830005",
            "Mon, 19 Oct 2026 08:00:00 GMT",
            "2026-10-19T09:00:00Z",
        ),
        (
            &first,
            "830006",
            "Sat, 24 Oct 2026 08:00:00 GMT",
            "2026-10-24T09:00:00Z",
        ),
        // The 7th file repeats the 6th byte for byte: same `member_sha256`, so
        // it is the same snapshot and no second row is written.
        (
            &first,
            "830006",
            "Wed, 28 Oct 2026 08:00:00 GMT",
            "2026-10-28T09:00:00Z",
        ),
    ];
    let inputs: Vec<SnapshotFacts> = files
        .iter()
        .map(|(entries, filler, last_modified, observed)| {
            snapshot(
                &cfg_file(entries, filler),
                Some(*last_modified),
                observed,
                "A",
                Some(DICT),
            )
        })
        .collect();

    let rebuild = rebuild(&inputs).unwrap();
    assert_eq!(rebuild.snapshots.len(), 6, "7 files, 6 distinct contents");
    assert_eq!(rebuild.boundaries, 6);
    assert_eq!(rebuild.superseded, 0);

    // The reclassification produced three intervals for sh.600006.
    let rows = assignment(&rebuild.assignments, "sh", "600006");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].industry_code, "X5001");
    assert_eq!(rows[0].effective_from, day(2026, 9, 30));
    assert_eq!(rows[0].effective_to, Some(day(2026, 10, 12)));
    assert_eq!(rows[1].industry_code, "X5002");
    assert_eq!(rows[1].effective_from, day(2026, 10, 12));
    assert_eq!(rows[1].effective_to, Some(day(2026, 10, 19)));
    assert_eq!(rows[2].industry_code, "X5001");
    assert_eq!(rows[2].effective_from, day(2026, 10, 19));
    assert_eq!(rows[2].effective_to, None);
    // The level-2 ancestor and the name come from the same dictionary.
    assert_eq!(rows[2].level, 2);
    assert_eq!(rows[2].name.as_deref(), Some("软件服务"));
    assert_eq!(rows[2].name_status, "resolved");
    assert_eq!(rows[2].basis, "publisher_last_modified");
    assert_eq!(rows[2].evidence_grade, "A");

    // 000002 appeared on 10-13 and disappeared on 10-16: the interval closes
    // at the snapshot it vanished from, not at a guessed date.
    let added = assignment(&rebuild.assignments, "sz", "000002");
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].effective_from, day(2026, 10, 13));
    assert_eq!(added[0].effective_to, Some(day(2026, 10, 16)));
    assert_eq!(
        added[0].known_at, "2026-10-13T09:00:00Z",
        "the forward run knows the snapshot when it acquired it"
    );
    assert_eq!(coverage_start(&rebuild.assignments), Some(day(2026, 9, 30)));
}

#[test]
fn members_added_removed_and_reclassified_keep_intervals_contiguous() {
    let v1 = cfg(&[
        (0, "000001", "T1001", "X500102"),
        (1, "600006", "T1001", "X500102"),
    ]);
    let v2 = cfg(&[
        (0, "000001", "T1001", "X500102"),
        (1, "600006", "T1001", "X500202"),
        (0, "000003", "T1001", "X500202"),
    ]);
    let v3 = cfg(&[
        (1, "600006", "T1001", "X500202"),
        (0, "000003", "T1001", "X500202"),
    ]);
    let inputs = vec![
        snapshot(
            &v1,
            Some("Wed, 30 Sep 2026 00:00:00 GMT"),
            "2026-09-30T01:00:00Z",
            "A",
            Some(DICT),
        ),
        snapshot(
            &v2,
            Some("Mon, 12 Oct 2026 00:00:00 GMT"),
            "2026-10-12T01:00:00Z",
            "A",
            Some(DICT),
        ),
        snapshot(
            &v3,
            Some("Fri, 16 Oct 2026 00:00:00 GMT"),
            "2026-10-16T01:00:00Z",
            "A",
            Some(DICT),
        ),
    ];
    let rebuild = rebuild(&inputs).unwrap();
    // 000001 was removed from 10-16 onwards: the interval ends there.
    let removed = assignment(&rebuild.assignments, "sz", "000001");
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].effective_from, day(2026, 9, 30));
    assert_eq!(removed[0].effective_to, Some(day(2026, 10, 16)));
    // 000003 was added on 10-12 and keeps the 10-12 mapping from then on.
    let added = assignment(&rebuild.assignments, "sz", "000003");
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].effective_from, day(2026, 10, 12));
    assert!(added[0].effective_to.is_none());
    // 600006: X5001 → X5002 → unchanged, so exactly two rows.
    let reclassified = assignment(&rebuild.assignments, "sh", "600006");
    assert_eq!(reclassified.len(), 2);
    assert_eq!(reclassified[0].industry_code, "X5001");
    assert_eq!(reclassified[1].industry_code, "X5002");
    assert!(reclassified[1].effective_to.is_none());
}

#[test]
fn a_code_that_returns_with_the_same_mapping_opens_a_second_interval() {
    let v1 = cfg(&[(0, "000001", "T1001", "X500102")]);
    // The returning snapshot must be a *different content* (an extra row makes
    // it so): identical bytes would be the same snapshot and be deduped.
    let v2 = cfg(&[
        (1, "600006", "T1001", "X500102"),
        (0, "000004", "T1001", "X500102"),
    ]);
    let v3 = cfg(&[
        (0, "000001", "T1001", "X500102"),
        (1, "600004", "T1001", "X500102"),
    ]);
    let inputs = vec![
        snapshot(
            &v1,
            Some("Wed, 30 Sep 2026 00:00:00 GMT"),
            "2026-09-30T01:00:00Z",
            "A",
            Some(DICT),
        ),
        snapshot(
            &v2,
            Some("Mon, 12 Oct 2026 00:00:00 GMT"),
            "2026-10-12T01:00:00Z",
            "A",
            Some(DICT),
        ),
        snapshot(
            &v3,
            Some("Fri, 16 Oct 2026 00:00:00 GMT"),
            "2026-10-16T01:00:00Z",
            "A",
            Some(DICT),
        ),
    ];
    let rebuild = rebuild(&inputs).unwrap();
    let rows = assignment(&rebuild.assignments, "sz", "000001");
    assert_eq!(
        rows.len(),
        2,
        "absence closes the interval; return opens one"
    );
    assert_eq!(rows[0].effective_from, day(2026, 9, 30));
    assert_eq!(rows[0].effective_to, Some(day(2026, 10, 12)));
    assert_eq!(rows[1].effective_from, day(2026, 10, 16));
    assert!(rows[1].effective_to.is_none());
    // The three contents are distinct, so the fixture has three boundaries.
    assert_eq!(rebuild.boundaries, 3);
}

#[test]
fn two_snapshots_on_one_day_resolve_to_the_later_ingest_and_drop_the_zero_length_row() {
    let v1 = cfg(&[(0, "000001", "T1001", "X500102")]);
    let v2 = cfg(&[(0, "000001", "T1001", "X500202")]);
    let inputs = vec![
        snapshot(
            &v1,
            Some("Mon, 12 Oct 2026 00:00:00 GMT"),
            "2026-10-12T01:00:00Z",
            "A",
            Some(DICT),
        ),
        snapshot(
            &v2,
            Some("Wed, 14 Oct 2026 00:00:00 GMT"),
            "2026-10-14T01:00:00Z",
            "A",
            Some(DICT),
        ),
    ];
    let rebuild = rebuild(&inputs).unwrap();
    // Both snapshots start on 10-12 (their own publisher dates differ only in
    // metadata; the same-day case is two snapshots with the same start day,
    // which this fixture makes explicit by using one start date).
    assert_eq!(rebuild.superseded, 0, "different days are not a collision");
    assert_eq!(assignment(&rebuild.assignments, "sz", "000001").len(), 2);

    // Same day: the second snapshot starts that same day, so the first
    // same-day segment is zero-length and must not be written.
    let same_day = vec![
        snapshot(
            &v1,
            Some("Mon, 12 Oct 2026 00:00:00 GMT"),
            "2026-10-12T01:00:00Z",
            "A",
            Some(DICT),
        ),
        snapshot(
            &v2,
            Some("Mon, 12 Oct 2026 12:00:00 GMT"),
            "2026-10-12T13:00:00Z",
            "A",
            Some(DICT),
        ),
    ];
    let rebuild = rebuild(&same_day).unwrap();
    assert_eq!(rebuild.superseded, 1);
    let rows = assignment(&rebuild.assignments, "sz", "000001");
    assert_eq!(rows.len(), 1, "zero-length segments are not written");
    assert_eq!(rows[0].industry_code, "X5002");
    assert_eq!(rows[0].effective_from, day(2026, 10, 12));

    // A code that exists only in the superseded same-day snapshot has no row
    // at all: it was never observed on a day it was absent from every snapshot.
    let with_extra = cfg(&[
        (0, "000001", "T1001", "X500202"),
        (0, "000009", "T1001", "X500202"),
    ]);
    let vanished_same_day = vec![
        snapshot(
            &with_extra,
            Some("Mon, 12 Oct 2026 00:00:00 GMT"),
            "2026-10-12T01:00:00Z",
            "A",
            Some(DICT),
        ),
        snapshot(
            &v2,
            Some("Mon, 12 Oct 2026 12:00:00 GMT"),
            "2026-10-12T13:00:00Z",
            "A",
            Some(DICT),
        ),
    ];
    let rebuild = rebuild(&vanished_same_day).unwrap();
    assert!(assignment(&rebuild.assignments, "sz", "000009").is_empty());
    // One collision: the code that only existed in the superseded same-day
    // snapshot had its zero-length segment dropped.
    assert_eq!(rebuild.superseded, 1);
}

#[test]
fn dictionary_presence_decides_unknown_dictionary_versus_not_in_dictionary() {
    let bytes = cfg(&[
        (0, "000001", "T1001", "X500102"),
        (0, "000004", "T1001", "X999902"),
    ]);
    // No dictionary at all: every name is `unknown_dictionary` (§5.6), and the
    // code/interval stays usable.
    let without = rebuild(&[snapshot(
        &bytes,
        Some("Wed, 30 Sep 2026 00:00:00 GMT"),
        "2026-09-30T01:00:00Z",
        "B",
        None,
    )])
    .unwrap();
    assert!(without.assignments.iter().all(|row| row.name.is_none()));
    assert!(
        without
            .assignments
            .iter()
            .all(|row| row.name_status == "unknown_dictionary")
    );

    // A dictionary that lacks the code is `not_in_dictionary`, not "unknown":
    // we did read a dictionary and the code is absent from it.
    let with = rebuild(&[snapshot(
        &bytes,
        Some("Wed, 30 Sep 2026 00:00:00 GMT"),
        "2026-09-30T01:00:00Z",
        "B",
        Some(DICT),
    )])
    .unwrap();
    let known = assignment(&with.assignments, "sz", "000001");
    assert_eq!(known[0].name.as_deref(), Some("软件服务"));
    assert_eq!(known[0].name_status, "resolved");
    let missing = assignment(&with.assignments, "sz", "000004");
    assert!(missing[0].name.is_none());
    assert_eq!(missing[0].name_status, "not_in_dictionary");
}

#[test]
fn lookup_is_unknown_before_coverage_and_when_the_code_is_missing() {
    let bytes = cfg(&[(0, "000001", "T1001", "X500102")]);
    let rebuild = rebuild(&[snapshot(
        &bytes,
        None,
        "2026-10-24T02:00:00Z",
        "B",
        Some(DICT),
    )])
    .unwrap();
    // Basis falls back to the acquisition time when the publisher exposes no
    // `last_modified` (§5.2).
    assert_eq!(rebuild.assignments[0].basis, "acquisition_time");
    assert_eq!(rebuild.assignments[0].effective_from, day(2026, 10, 24));

    assert_eq!(
        lookup(&rebuild.assignments, "sz", "000001", day(2026, 10, 23)),
        Lookup::Unknown {
            reason: UnknownReason::BeforeCoverage
        }
    );
    assert_eq!(
        lookup(&rebuild.assignments, "sz", "000001", day(2026, 10, 24)),
        Lookup::Assigned {
            level: 2,
            industry_code: "X5001".to_string(),
            name: Some("软件服务".to_string()),
            name_status: "resolved".to_string(),
            effective_from: day(2026, 10, 24),
            effective_to: None,
            evidence_grade: "B".to_string(),
            snapshot_member_sha256: crate::hash::sha256(&bytes),
        }
    );
    // Inside coverage but absent from the snapshot: never interpolated.
    assert_eq!(
        lookup(&rebuild.assignments, "sz", "000002", day(2026, 10, 24)),
        Lookup::Unknown {
            reason: UnknownReason::MissingInSnapshot
        }
    );
    // An empty table is "before coverage", not "no industry".
    assert_eq!(
        lookup(&[], "sz", "000001", day(2026, 10, 24)),
        Lookup::Unknown {
            reason: UnknownReason::BeforeCoverage
        }
    );
}

#[test]
fn grade_c_snapshots_are_stored_but_never_define_intervals() {
    let bytes = cfg(&[(0, "000001", "T1001", "X500102")]);
    let mut record = snapshot(
        &bytes,
        Some("Wed, 16 Sep 2026 00:00:00 GMT"),
        "2026-09-16T00:00:00Z",
        "C",
        None,
    );
    record.record.source_kind = SourceKind::RepoEvidence;
    let rebuild = rebuild(&[record]).unwrap();
    assert_eq!(
        rebuild.snapshots.len(),
        1,
        "the snapshot row is kept for audit"
    );
    assert!(rebuild.assignments.is_empty());
    assert_eq!(rebuild.boundaries, 0);
    assert_eq!(coverage_start(&rebuild.assignments), None);
}

#[test]
fn rebuild_is_deterministic_and_the_daily_append_matches_it() {
    let v1 = cfg(&[
        (0, "000001", "T1001", "X500102"),
        (1, "600006", "T1001", "X500102"),
    ]);
    let v2 = cfg(&[
        (0, "000001", "T1001", "X500202"),
        (1, "600006", "T1001", "X500102"),
        (0, "000005", "T1001", "X500102"),
    ]);
    let v3 = cfg(&[
        (0, "000005", "T1001", "X500102"),
        (1, "600006", "T1001", "X500102"),
    ]);
    let inputs = vec![
        snapshot(
            &v1,
            Some("Wed, 30 Sep 2026 00:00:00 GMT"),
            "2026-09-30T01:00:00Z",
            "A",
            Some(DICT),
        ),
        snapshot(
            &v2,
            Some("Mon, 12 Oct 2026 00:00:00 GMT"),
            "2026-10-12T01:00:00Z",
            "A",
            Some(DICT),
        ),
        snapshot(
            &v3,
            Some("Fri, 16 Oct 2026 00:00:00 GMT"),
            "2026-10-16T01:00:00Z",
            "A",
            Some(DICT),
        ),
    ];
    let first = rebuild(&inputs).unwrap();
    let second = rebuild(&inputs).unwrap();
    assert_eq!(
        summary(&first.snapshots, &first.assignments),
        summary(&second.snapshots, &second.assignments),
        "two replays of one input set agree bit for bit"
    );
    assert!(
        drift(
            &first.snapshots,
            &first.assignments,
            &second.snapshots,
            &second.assignments,
            20
        )
        .is_empty()
    );

    // The daily path applies one snapshot at a time onto the stored rows; the
    // result must equal the full rebuild, otherwise "clear the DB and replay"
    // would silently drift from the incrementally maintained table.
    let mut rows: Vec<AssignmentRow> = Vec::new();
    for facts in &inputs {
        let (next, _) = apply_on(&rows, facts).unwrap();
        rows = next;
    }
    assert_eq!(rows, first.assignments);

    // Applying the same snapshot again is a no-op at this layer too (the DB
    // writer short-circuits on `member_sha256`, this proves the computation is
    // stable when it is reached).
    let (again, superseded) = apply_on(&rows, &inputs[2]).unwrap();
    assert_eq!(again, rows);
    assert_eq!(superseded, 0);
}

#[test]
fn a_historical_snapshot_is_refused_on_the_daily_path() {
    let v1 = cfg(&[(0, "000001", "T1001", "X500102")]);
    let v2 = cfg(&[(0, "000001", "T1001", "X500202")]);
    let newer = rebuild(&[snapshot(
        &v2,
        Some("Mon, 19 Oct 2026 00:00:00 GMT"),
        "2026-10-19T01:00:00Z",
        "A",
        Some(DICT),
    )])
    .unwrap();
    let older = snapshot(
        &v1,
        Some("Wed, 30 Sep 2026 00:00:00 GMT"),
        "2026-09-30T01:00:00Z",
        "A",
        Some(DICT),
    );
    let error = apply_on(&newer.assignments, &older).unwrap_err();
    assert!(
        error.to_string().contains("backfill"),
        "a historical snapshot must be replayed, not appended: {error}"
    );
}

#[test]
fn parse_rejects_a_conflicting_duplicate_and_skips_codes_without_a_level_two_ancestor() {
    let conflicting = b"0|000001|T1001|||X500102\r\n0|000001|T1001|||X500202\r\n";
    let error = parse_members(conflicting).unwrap_err();
    assert!(error.to_string().contains("conflicting mapping"), "{error}");

    // A level-1 X code has no level-2 assignment and is not guessed one.
    let level_one = b"0|000001|T1001|||X50\r\n0|000002|T1001|||X500102\r\n";
    let (rows, members) = parse_members(level_one).unwrap();
    assert_eq!(rows, 2);
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].code, "000002");
    assert_eq!(members[0].industry_code, "X5001");
}

#[test]
fn publisher_times_accept_http_dates_and_rfc_3339() {
    assert_eq!(
        parse_publisher_day("Mon, 12 Oct 2026 08:00:00 GMT").unwrap(),
        NaiveDate::from_ymd_opt(2026, 10, 12).unwrap()
    );
    assert_eq!(
        parse_publisher_day("2026-10-12T08:00:00Z").unwrap(),
        NaiveDate::from_ymd_opt(2026, 10, 12).unwrap()
    );
    assert_eq!(
        parse_publisher_day("2026-10-12").unwrap(),
        NaiveDate::from_ymd_opt(2026, 10, 12).unwrap()
    );
    assert!(parse_publisher_day("yesterday").is_err());
    assert_eq!(
        parse_day_prefix("2026-10-24T02:00:00Z").unwrap(),
        NaiveDate::from_ymd_opt(2026, 10, 24).unwrap()
    );
}
