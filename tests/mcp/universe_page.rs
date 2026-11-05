//! #295: a page of a v2 `selection.kind=universe` price read reports only its
//! own entities, and the snapshot budget counts the answer the caller gets.
//!
//! The repository is a fabricated in-memory one (no storage): entity `i` of
//! `n` has a version (every 50th is absent), a per-entity coverage reason, and
//! one bar per trading day (every 97th cell is missing), so pages differ in
//! content and the manifest is as heavy as a real one.
use chrono::{DateTime, Datelike, NaiveDate, Utc, Weekday};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use x_data_mcp::{
    data_supply::{
        Access,
        columnar::{self, PageMeta},
        query::{MAX_BYTES, Queries, QueryPlan, QuerySnapshot, ReadRepository},
        wire::*,
    },
    market_storage::Error,
};

struct Fake {
    members: usize,
}

fn security(i: usize) -> Security {
    Security {
        exchange: if i.is_multiple_of(2) {
            Exchange::Sh
        } else {
            Exchange::Sz
        },
        code: format!("{:06}", 600_000 + i),
    }
}

fn version_id(entity: &Security, tag: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(format!("{tag}{}", entity.symbol()).as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn evidence(field: &str, value: &str) -> Evidence {
    Evidence {
        field: field.into(),
        status: "confirmed".into(),
        value: Some(value.into()),
        reason: "bound to the verified official generation inputs".into(),
    }
}

fn metadata(i: usize) -> Metadata {
    Metadata {
        source: "official_market".into(),
        published_at: DateTime::from_timestamp(1_760_000_000 + i as i64, 0).unwrap(),
        coverage_status: "complete".into(),
        // The measured production `versions` rows are ~860 B each (#295
        // report), because the coverage reason differs per entity.
        coverage_reason: format!(
            "full history verified through generation, member {i}; {}",
            "the .day members, the security-event set and the trading calendar were checked \
             against the frozen generation manifest before this version was published. "
                .repeat(5)
        ),
        units: vec![
            evidence("price_currency", "CNY"),
            evidence("price_unit", "yuan"),
            evidence("volume_unit", "share"),
            evidence("amount_currency", "CNY"),
            evidence("amount_unit", "yuan"),
        ],
        adjustment: Some(evidence("price_adjustment", "qfq")),
        lineage: None,
        index: None,
        verified_through: None,
        price_scale: None,
        last_trade_date: None,
        delisting_date: None,
    }
}

fn version_ref(entity: &Security) -> VersionRef {
    VersionRef {
        dataset: Dataset::Daily,
        contract_version: ContractVersion::V1,
        entity: Some(entity.clone()),
        version_id: version_id(entity, "v"),
        integrity_sha256: version_id(entity, "v"),
    }
}

fn is_absent(i: usize) -> bool {
    i % 50 == 7
}

fn has_bar(i: usize, day_index: usize) -> bool {
    !(i + day_index).is_multiple_of(97)
}

fn trading_days(start: NaiveDate, end: NaiveDate) -> Vec<NaiveDate> {
    start
        .iter_days()
        .take_while(|day| *day <= end)
        .filter(|day| !matches!(day.weekday(), Weekday::Sat | Weekday::Sun))
        .collect()
}

fn price(i: usize, day_index: usize, field: usize) -> String {
    format!(
        "{}.{:02}",
        10 + (i % 90),
        (i * 7 + day_index * 13 + field) % 100
    )
}

impl ReadRepository for Fake {
    fn query(&self, plan: &QueryPlan, _: &Access) -> Result<QuerySnapshot, Error> {
        let days = match &plan.args.filter {
            Filter::CrossSection { .. } => plan.trade_days.clone(),
            Filter::Daily {
                start_date,
                end_date,
                ..
            } => trading_days(*start_date, *end_date),
            _ => panic!("unexpected filter"),
        };
        let mut snapshot = QuerySnapshot {
            manifest: vec![],
            rows: vec![],
            temporal: None,
            limitations: vec![],
        };
        for entity in &plan.entities {
            let i = entity.code.parse::<usize>().unwrap() - 600_000;
            if is_absent(i) {
                snapshot.manifest.push(ManifestEntry {
                    entity: Some(entity.clone()),
                    version_ref: None,
                    status: ManifestStatus::Absent,
                    metadata: None,
                    stale: None,
                    as_of_day: None,
                    reason: None,
                });
                continue;
            }
            let reference = version_ref(entity);
            snapshot.manifest.push(ManifestEntry {
                entity: Some(entity.clone()),
                version_ref: Some(reference.clone()),
                status: ManifestStatus::Present,
                metadata: Some(metadata(i)),
                stale: None,
                as_of_day: None,
                reason: None,
            });
            for (day_index, day) in days.iter().enumerate() {
                if !has_bar(i, day_index) {
                    continue;
                }
                snapshot.rows.push(Row::Market {
                    entity: entity.clone(),
                    version_ref: reference.clone(),
                    date: *day,
                    first_trading_date: None,
                    last_trading_date: None,
                    year_unfinished: None,
                    open: price(i, day_index, 0),
                    high: price(i, day_index, 1),
                    low: price(i, day_index, 2),
                    close: price(i, day_index, 3),
                    volume: Some(format!("{}", 100 * (i + 1))),
                    amount: Some(format!("{}.5", 1000 * (i + 1))),
                    no_trade: None,
                });
            }
        }
        Ok(snapshot)
    }
    fn verify(&self, _: &[VersionRef]) -> Result<(), Error> {
        Ok(())
    }
    fn versions(&self, _: &VersionsQuery, _: &Access) -> Result<Vec<VersionInfo>, Error> {
        unreachable!()
    }
    fn universe_members(
        &self,
        _: Dataset,
        _: Option<&str>,
        _: Option<CatalogScope>,
        _: Option<NaiveDate>,
        _: Option<NaiveDate>,
    ) -> Result<(String, Vec<Security>), Error> {
        Ok((
            "catalog-fake".into(),
            (0..self.members).map(security).collect(),
        ))
    }
    fn calendar_days(&self, start: NaiveDate, end: NaiveDate) -> Result<Vec<NaiveDate>, Error> {
        Ok(trading_days(start, end))
    }
    fn last_trading_day_upto(&self, day: NaiveDate) -> Result<NaiveDate, Error> {
        Ok(day)
    }
}

fn day(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 1, d).unwrap()
}

fn request(selection: Value, filter: Value, page_size: usize, cursor: Option<&str>) -> Query {
    serde_json::from_value(json!({
        "dataset": "market.daily",
        "contract_version": "v2",
        "selection": selection,
        "filter": filter,
        "version": {"kind": "latest_per_entity"},
        "page_size": page_size,
        "cursor": cursor,
    }))
    .unwrap()
}

fn universe() -> Value {
    json!({"kind": "universe"})
}

fn cross_section(end: u32) -> Value {
    json!({"kind": "cross_section", "start_date": "2026-01-05", "end_date": format!("2026-01-{end:02}")})
}

fn bytes(page: &Value) -> usize {
    serde_json::to_vec(page).unwrap().len()
}

fn pages(queries: &Queries<Fake>, selection: Value, filter: Value, size: usize) -> Vec<Value> {
    let access = Access::local();
    let mut out: Vec<Value> = Vec::new();
    loop {
        let cursor = out
            .last()
            .and_then(|page| page["next_cursor"].as_str())
            .map(str::to_owned);
        if !out.is_empty() && cursor.is_none() {
            return out;
        }
        out.push(
            queries
                .query_v2(
                    request(selection.clone(), filter.clone(), size, cursor.as_deref()),
                    &access,
                )
                .unwrap(),
        );
    }
}

/// The snapshot the repository answers for the whole universe, and what the
/// caller would have received for it in one piece (the pre-fix page body).
fn whole(
    members: usize,
    filter: Filter,
    trade_days: Vec<NaiveDate>,
) -> (QuerySnapshot, Vec<Security>) {
    let entities: Vec<Security> = {
        let mut all: Vec<Security> = (0..members).map(security).collect();
        all.sort();
        all
    };
    let plan = QueryPlan {
        args: request(
            universe(),
            json!({"kind": "daily", "start_date": "2026-01-05", "end_date": "2026-01-05"}),
            1000,
            None,
        ),
        entities: entities.clone(),
        catalog_version: None,
        trade_days,
        temporal: None,
    };
    let mut plan = plan;
    plan.args.filter = filter;
    (
        Fake { members }.query(&plan, &Access::local()).unwrap(),
        entities,
    )
}

fn meta<'a>() -> PageMeta<'a> {
    PageMeta {
        snapshot_id: "x",
        snapshot_time: Utc::now(),
        next_cursor: None,
        page_complete: true,
        limitations: &[],
        temporal: None,
    }
}

/// `(entity, day) -> close` of a cross-section page (null cells skipped).
fn closes(page: &Value, into: &mut BTreeMap<(String, String), String>) {
    let entities = page["entities"].as_array().unwrap();
    let days = page["trade_days"].as_array().unwrap();
    for (d, column) in page["columns"]["close"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let cells = column.as_array().unwrap();
        assert_eq!(cells.len(), entities.len(), "column is aligned to entities");
        for (e, cell) in cells.iter().enumerate() {
            if let Some(value) = cell.as_str() {
                let key = (
                    entities[e].as_str().unwrap().to_owned(),
                    days[d].as_str().unwrap().to_owned(),
                );
                if let Some(old) = into.insert(key, value.to_owned()) {
                    assert_eq!(old, value, "a repeated entity must carry the same cell");
                }
            }
        }
    }
}

/// The reported fault: every page carried every entity, every version row and
/// a full-width `null`-padded column grid.
#[test]
fn a_universe_page_is_sized_by_its_own_rows() {
    let members = 2_200;
    let queries = Queries::new(Fake { members });
    let all = pages(&queries, universe(), cross_section(5), 500);
    assert_eq!(all.len(), 5);
    let first = bytes(&all[0]);
    let second = bytes(&all[1]);
    for page in &all {
        // 500 rows, plus the entities without a row (absent version or
        // missing bar) that sit between two pages' first rows.
        assert!(page["entities"].as_array().unwrap().len() <= 540);
        assert_eq!(
            page["versions"].as_array().unwrap().len(),
            page["entities"].as_array().unwrap().len()
        );
        assert_eq!(page["catalog_version"], "catalog-fake");
    }
    // Page 2 is the same order of size as page 1 ...
    assert!(
        second * 2 > first && first * 2 > second,
        "{first} vs {second}"
    );

    // ... and far below the answer the pre-fix code gave for each page.
    let (snapshot, entities) = whole(
        members,
        Filter::CrossSection {
            start_date: day(5),
            end_date: day(5),
        },
        vec![day(5)],
    );
    let fields: Vec<String> = ["open", "high", "low", "close", "volume", "amount"]
        .map(String::from)
        .to_vec();
    let before = columnar::cross_section(
        Dataset::Daily,
        &snapshot.manifest,
        &[day(5)],
        &entities,
        &snapshot.rows[..500],
        &fields,
        &meta(),
    );
    assert!(
        second * 3 < bytes(&before),
        "page {second} B vs pre-fix page {} B",
        bytes(&before)
    );
}

/// All pages together hold exactly the rows and versions of the whole answer.
#[test]
fn universe_pages_add_up_to_the_whole_answer() {
    let members = 2_200;
    let queries = Queries::new(Fake { members });
    for (selection_filter, end, size) in [
        (cross_section(5), 5, 1000),
        (cross_section(7), 7, 1000),
        (cross_section(9), 9, 777),
    ] {
        let all = pages(&queries, universe(), selection_filter, size);
        let mut got = BTreeMap::new();
        let mut versions: BTreeMap<String, Value> = BTreeMap::new();
        for page in &all {
            closes(page, &mut got);
            let reported = page["entities"].as_array().unwrap();
            let rows = page["versions"].as_array().unwrap();
            assert_eq!(reported.len(), rows.len());
            // `texts` indices and the cut of a long text belong to one
            // response, so rows are compared after the texts are joined back.
            let texts = page["texts"].as_array().cloned().unwrap_or_default();
            for (entity, row) in reported.iter().zip(rows) {
                assert_eq!(row["entity"], *entity);
                let row = super::versions_slim::expand(row, &texts);
                if let Some(old) = versions.insert(entity.as_str().unwrap().to_owned(), row.clone())
                {
                    assert_eq!(old, row, "a repeated entity carries the same version row");
                }
            }
        }
        let days = trading_days(day(5), day(end));
        let (snapshot, entities) = whole(
            members,
            Filter::CrossSection {
                start_date: day(5),
                end_date: day(end),
            },
            days.clone(),
        );
        let mut expected = BTreeMap::new();
        for row in &snapshot.rows {
            let Row::Market {
                entity,
                date,
                close,
                ..
            } = row
            else {
                unreachable!()
            };
            expected.insert((entity.symbol(), date.to_string()), close.clone());
        }
        assert_eq!(got, expected, "rows of window ending {end}");
        // Every manifest entry (absent ones included) is reported once.
        assert_eq!(versions.len(), entities.len());
        for (entity, entry) in entities.iter().zip(&snapshot.manifest) {
            let row = &versions[&entity.symbol()];
            let status = if entry.version_ref.is_some() {
                "present"
            } else {
                "absent"
            };
            assert_eq!(row["status"], status, "{}", entity.symbol());
            assert_eq!(
                row["version_id"].as_str(),
                entry.version_ref.as_ref().map(|r| r.version_id.as_str())
            );
        }
    }
}

/// The daily (panel) layout over a universe is cut the same way.
#[test]
fn universe_panel_pages_report_only_their_entities() {
    let members = 2_200;
    let queries = Queries::new(Fake { members });
    let filter = json!({"kind": "daily", "start_date": "2026-01-05", "end_date": "2026-01-09"});
    let all = pages(&queries, universe(), filter, 1000);
    let (snapshot, entities) = whole(
        members,
        Filter::Daily {
            start_date: day(5),
            end_date: day(9),
            as_of: None,
        },
        vec![],
    );
    let mut got = BTreeMap::new();
    for page in &all {
        let reported = page["entities"].as_array().unwrap();
        assert_eq!(page["versions"].as_array().unwrap().len(), reported.len());
        assert_eq!(page["dates"].as_array().unwrap().len(), reported.len());
        for (e, entity) in reported.iter().enumerate() {
            let dates = page["dates"][e].as_array().unwrap();
            let closes = page["columns"]["close"][e].as_array().unwrap();
            assert_eq!(dates.len(), closes.len());
            for (date, close) in dates.iter().zip(closes) {
                let key = (
                    entity.as_str().unwrap().to_owned(),
                    date.as_str().unwrap().to_owned(),
                );
                if let Some(old) = got.insert(key, close.as_str().unwrap().to_owned()) {
                    // an entity cut by a page edge is reported by both pages
                    // with the rows each page holds, never the same row twice
                    panic!("row repeated across pages: {old}");
                }
            }
        }
    }
    let mut expected = BTreeMap::new();
    for row in &snapshot.rows {
        let Row::Market {
            entity,
            date,
            close,
            ..
        } = row
        else {
            unreachable!()
        };
        expected.insert((entity.symbol(), date.to_string()), close.clone());
    }
    assert_eq!(got, expected);
    let mut seen: Vec<&str> = all
        .iter()
        .flat_map(|page| page["versions"].as_array().unwrap())
        .map(|row| row["entity"].as_str().unwrap())
        .collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), entities.len());
}

/// `a_current` scale, all fields: 3 and 5 trading days are served. The
/// fixture is only meaningful if the snapshot, serialized the way the budget
/// used to count it, really is over the limit.
#[test]
fn three_and_five_day_universe_windows_fit_the_budget() {
    let members = 5_225;
    for end in [7, 9] {
        let days = trading_days(day(5), day(end));
        let (snapshot, _) = whole(
            members,
            Filter::CrossSection {
                start_date: day(5),
                end_date: day(end),
            },
            days,
        );
        assert!(
            serde_json::to_vec(&snapshot).unwrap().len() > MAX_BYTES,
            "the fixture no longer reproduces the pre-fix rejection"
        );
        let queries = Queries::new(Fake { members });
        let all = pages(&queries, universe(), cross_section(end), 1000);
        // Pages count rows (1,000 per page), not entities.
        assert_eq!(all.len(), snapshot.rows.len().div_ceil(1000));
        let widest = all.iter().map(bytes).max().unwrap();
        eprintln!(
            "window ending {end}: {} rows, snapshot {} B, widest page {widest} B, first {} B",
            snapshot.rows.len(),
            serde_json::to_vec(&snapshot).unwrap().len(),
            bytes(&all[0]),
        );
        assert!(widest < 2 * 1024 * 1024, "page of {widest} B");
    }
}

/// The measured case: one day, 5,225 entities, 1,000 per page.
#[test]
fn one_day_universe_pages_stay_small() {
    let queries = Queries::new(Fake { members: 5_225 });
    let all = pages(&queries, universe(), cross_section(5), 1000);
    assert_eq!(all.len(), 6);
    let sizes: Vec<usize> = all.iter().map(bytes).collect();
    eprintln!("one-day page bytes: {sizes:?}");
    assert!(sizes.iter().all(|size| *size < 2 * 1024 * 1024));
}

/// `single` and `batch` are not cut: they report every selected entity, and a
/// universe that fits one page reads the same as the equivalent batch.
#[test]
fn single_and_batch_keep_every_entity() {
    let queries = Queries::new(Fake { members: 12 });
    let access = Access::local();
    let filter = cross_section(6);
    let batch_entities: Vec<Value> = (0..12).map(|i| json!(security(i))).collect();
    let batch = queries
        .query_v2(
            request(
                json!({"kind": "batch", "entities": batch_entities}),
                filter.clone(),
                1000,
                None,
            ),
            &access,
        )
        .unwrap();
    let universe = queries
        .query_v2(request(universe(), filter, 1000, None), &access)
        .unwrap();
    assert_eq!(batch["entities"].as_array().unwrap().len(), 12);
    assert_eq!(batch["versions"].as_array().unwrap().len(), 12);
    let normalized = |mut page: Value| {
        page["snapshot_id"] = json!("");
        page["snapshot_time"] = json!("");
        page.as_object_mut().unwrap().remove("catalog_version");
        page
    };
    assert_eq!(normalized(universe), normalized(batch));

    let single = queries
        .query_v2(
            request(
                json!({"kind": "single", "entity": security(3)}),
                json!({"kind": "daily", "start_date": "2026-01-05", "end_date": "2026-01-06"}),
                100,
                None,
            ),
            &access,
        )
        .unwrap();
    assert_eq!(single["entities"], json!([security(3).symbol()]));
    assert_eq!(single["versions"].as_array().unwrap().len(), 1);
}
