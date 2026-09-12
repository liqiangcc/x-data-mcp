//! Issue #55 AC-01/02/04. Offline gates, not parser or live-source evidence.
use chrono::{NaiveDate, Utc};
use rust_decimal::Decimal;
use x_data_mcp::{
    collector::Collector,
    market_data::{
        Adjustment, AdjustmentEvidence, Bar, CoverageEvidence, MarketDataBatch, MarketDataPipeline,
        Period, Provenance, SourceError, UnitCurrencyEvidence, YearlyDerivation,
    },
    official_files::OfficialFilePipeline,
    publish::{EffectiveVersion, MarketDataPublisher, PublishOutcome},
    storage::JsonFileStore,
};

fn daily_fixture() -> MarketDataBatch {
    MarketDataBatch {
        symbol: "000001".into(),
        period: Period::Day,
        adjustment: Adjustment::Forward,
        bars: vec![Bar {
            date: NaiveDate::from_ymd_opt(2026, 9, 10).unwrap(),
            open: Decimal::ONE,
            high: Decimal::ONE,
            low: Decimal::ONE,
            close: Decimal::ONE,
            volume: None,
            amount: None,
        }],
        provenance: Provenance {
            source: "offline-fixture".into(),
            source_period: "day".into(),
            source_adjustment: "qfq".into(),
            symbol: "000001".into(),
            acquired_at: Utc::now(),
            adapter_version: "fixture".into(),
            yearly_derivation: None,
        },
        coverage: CoverageEvidence::complete("fixture only"),
        unit_currency: UnitCurrencyEvidence::confirmed(
            "CNY",
            "yuan",
            "share",
            "CNY",
            "yuan",
            "fixture only",
        ),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "fixture only"),
    }
}

fn seed_daily(store: &JsonFileStore) -> EffectiveVersion {
    match MarketDataPublisher::new(store.clone())
        .publish(&daily_fixture())
        .unwrap()
    {
        PublishOutcome::Published(version) => version,
        outcome => panic!("unexpected fixture outcome: {outcome:?}"),
    }
}

fn yearly_fixture(daily: &EffectiveVersion) -> MarketDataBatch {
    // One-row fixture tests linkage only, not the unimplemented aggregation algorithm.
    let mut batch = daily_fixture();
    batch.period = Period::Year;
    batch.provenance.yearly_derivation = Some(YearlyDerivation {
        base_daily_version_id: daily.version_id.clone(),
        algorithm_version: "fixture-only".into(),
        first_trading_date: daily.records[0].date,
        last_trading_date: daily.records[0].date,
    });
    batch
}

#[test]
fn official_pipeline_reports_every_unresolved_gate_for_both_operations() {
    let dir = tempfile::tempdir().unwrap();
    let daily = seed_daily(&JsonFileStore::new(dir.path()));
    for result in [
        OfficialFilePipeline.materialize_daily("000001", Adjustment::Forward),
        OfficialFilePipeline.derive_yearly(&daily),
    ] {
        assert!(matches!(result, Err(SourceError::NotVerified(_))));
        let message = result.unwrap_err().to_string();
        assert!(message.contains("NOT_PRODUCTION_READY"));
        // Independent requirements from Issue #55; do not infer them from the adapter list.
        for gate in [
            "CATALOG_INDUSTRY_NOT_VERIFIED",
            "#6/#46",
            "GBBQ_QFQ_NOT_VERIFIED",
            "#54",
            "FILE_ACCESS_USE_NOT_VERIFIED",
            "FILE_SET_CONSISTENCY_NOT_VERIFIED",
            "LOCAL_PIPELINE_NOT_IMPLEMENTED",
        ] {
            assert!(message.contains(gate), "missing gate {gate}: {message}");
        }
    }
}

#[test]
fn blocked_refresh_preserves_existing_daily_and_yearly_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    let daily = seed_daily(&store);
    MarketDataPublisher::new(store.clone())
        .publish(&yearly_fixture(&daily))
        .unwrap();
    let paths = [
        "effective/000001.day.qfq.json",
        "effective/000001.year.qfq.json",
        "000001.day.qfq.json",
        "000001.year.qfq.json",
    ];
    let before = paths.map(|path| std::fs::read(dir.path().join(path)).unwrap());
    let results = Collector::new(OfficialFilePipeline, store).collect(
        &["000001"],
        &[Period::Day, Period::Year],
        Adjustment::Forward,
    );
    for result in results {
        assert!(result.result.unwrap_err().contains("SOURCE_NOT_VERIFIED"));
    }
    for (path, bytes) in paths.into_iter().zip(before) {
        assert_eq!(std::fs::read(dir.path().join(path)).unwrap(), bytes);
    }
}

struct NoInputPipeline;
impl MarketDataPipeline for NoInputPipeline {
    fn materialize_daily(&self, _: &str, _: Adjustment) -> Result<MarketDataBatch, SourceError> {
        panic!("year request must not silently add a daily collection");
    }
    fn derive_yearly(&self, _: &EffectiveVersion) -> Result<MarketDataBatch, SourceError> {
        panic!("must not call yearly without published daily");
    }
}

#[test]
fn year_without_published_daily_cannot_call_pipeline_or_publish() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    // A raw/legacy batch file is not a published daily version.
    store.put(&daily_fixture()).unwrap();
    let result = Collector::new(NoInputPipeline, store.clone()).collect(
        &["000001"],
        &[Period::Year],
        Adjustment::Forward,
    );
    assert!(
        result[0]
            .result
            .as_ref()
            .unwrap_err()
            .contains("PUBLISHED_QFQ_DAILY_REQUIRED")
    );
    assert!(
        store
            .get_latest_effective("000001", Period::Year)
            .unwrap()
            .is_none()
    );
}

#[test]
fn publisher_rejects_external_year_missing_or_forged_daily_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    let daily = seed_daily(&store);
    let valid = yearly_fixture(&daily);
    let publisher = MarketDataPublisher::new(store.clone());
    let mut cases = Vec::new();
    let mut external_year = valid.clone();
    external_year.provenance.source_period = "year".into();
    cases.push(external_year);
    let mut missing = valid.clone();
    missing.provenance.yearly_derivation = None;
    cases.push(missing);
    let mut wrong_base = valid.clone();
    wrong_base
        .provenance
        .yearly_derivation
        .as_mut()
        .unwrap()
        .base_daily_version_id = "not-published".into();
    cases.push(wrong_base);
    let mut missing_algorithm = valid.clone();
    missing_algorithm
        .provenance
        .yearly_derivation
        .as_mut()
        .unwrap()
        .algorithm_version
        .clear();
    cases.push(missing_algorithm);
    let mut wrong_date = valid;
    wrong_date
        .provenance
        .yearly_derivation
        .as_mut()
        .unwrap()
        .last_trading_date = NaiveDate::from_ymd_opt(2026, 9, 11).unwrap();
    cases.push(wrong_date);
    for candidate in cases {
        assert!(publisher.publish(&candidate).is_err());
        assert!(
            store
                .get_latest_effective("000001", Period::Year)
                .unwrap()
                .is_none()
        );
    }
}

struct WrongBasePipeline;
impl MarketDataPipeline for WrongBasePipeline {
    fn materialize_daily(&self, _: &str, _: Adjustment) -> Result<MarketDataBatch, SourceError> {
        panic!("year request must not collect daily");
    }
    fn derive_yearly(&self, daily: &EffectiveVersion) -> Result<MarketDataBatch, SourceError> {
        let mut batch = yearly_fixture(daily);
        batch
            .provenance
            .yearly_derivation
            .as_mut()
            .unwrap()
            .base_daily_version_id = "other".into();
        Ok(batch)
    }
}

#[test]
fn collector_rejects_yearly_that_does_not_use_the_fixed_input() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    seed_daily(&store);
    let results = Collector::new(WrongBasePipeline, store.clone()).collect(
        &["000001"],
        &[Period::Year],
        Adjustment::Forward,
    );
    assert!(
        results[0]
            .result
            .as_ref()
            .unwrap_err()
            .contains("supplied published daily version")
    );
    assert!(
        store
            .get_latest_effective("000001", Period::Year)
            .unwrap()
            .is_none()
    );
}

fn multi_year_daily_fixture(store: &JsonFileStore) -> EffectiveVersion {
    let mut batch = daily_fixture();
    batch.bars = [
        (2024, 1, 2),
        (2025, 1, 2),
        (2025, 7, 1),
        (2025, 12, 31),
        (2026, 9, 10),
    ]
    .into_iter()
    .map(|(year, month, day)| {
        let mut bar = batch.bars[0].clone();
        bar.date = NaiveDate::from_ymd_opt(year, month, day).unwrap();
        bar
    })
    .collect();
    match MarketDataPublisher::new(store.clone())
        .publish(&batch)
        .unwrap()
    {
        PublishOutcome::Published(version) => version,
        outcome => panic!("unexpected fixture outcome: {outcome:?}"),
    }
}

fn only_2025_fixture(daily: &EffectiveVersion) -> MarketDataBatch {
    let mut batch = yearly_fixture(daily);
    batch.bars[0].date = NaiveDate::from_ymd_opt(2025, 12, 31).unwrap();
    let lineage = batch.provenance.yearly_derivation.as_mut().unwrap();
    lineage.first_trading_date = NaiveDate::from_ymd_opt(2025, 1, 2).unwrap();
    lineage.last_trading_date = NaiveDate::from_ymd_opt(2025, 12, 31).unwrap();
    batch
}

/// Issue #3: selected historical years use their actual participating dates,
/// not the outer boundaries of the fixed multi-year daily version.
#[test]
fn yearly_lineage_allows_one_year_within_a_multi_year_daily_version() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    let daily = multi_year_daily_fixture(&store);
    let yearly = only_2025_fixture(&daily);
    let outcome = MarketDataPublisher::new(store.clone())
        .publish(&yearly)
        .unwrap();
    let PublishOutcome::Published(version) = outcome else {
        panic!("expected subset yearly publication");
    };
    assert_eq!(
        version.yearly_derivation,
        yearly.provenance.yearly_derivation
    );
    assert_eq!(version.records, yearly.bars);
    assert_eq!(
        store
            .get_latest_effective("000001", Period::Day)
            .unwrap()
            .unwrap(),
        daily
    );
}

#[test]
fn yearly_lineage_rejects_outside_reversed_or_absent_participating_dates() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    let daily = multi_year_daily_fixture(&store);
    let publisher = MarketDataPublisher::new(store.clone());
    for (first, last) in [
        ((2023, 12, 29), (2025, 12, 31)), // before base coverage
        ((2025, 1, 2), (2027, 1, 4)),     // after base coverage
        ((2025, 12, 31), (2025, 1, 2)),   // reversed, both dates present
        ((2025, 1, 3), (2025, 12, 31)),   // inside bounds, first date absent
        ((2025, 1, 2), (2025, 12, 30)),   // inside bounds, last date absent
    ] {
        let mut yearly = only_2025_fixture(&daily);
        let lineage = yearly.provenance.yearly_derivation.as_mut().unwrap();
        lineage.first_trading_date = NaiveDate::from_ymd_opt(first.0, first.1, first.2).unwrap();
        lineage.last_trading_date = NaiveDate::from_ymd_opt(last.0, last.1, last.2).unwrap();
        assert!(
            publisher.publish(&yearly).is_err(),
            "unexpected acceptance: {first:?}..{last:?}"
        );
        assert!(
            store
                .get_latest_effective("000001", Period::Year)
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(
        store
            .get_latest_effective("000001", Period::Day)
            .unwrap()
            .unwrap(),
        daily
    );
}
