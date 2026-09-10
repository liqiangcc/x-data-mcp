use chrono::Utc;
use rust_decimal::Decimal;
use tempfile::tempdir;
use x_data_mcp::{
    collector::Collector,
    market_data::{
        Adjustment, Bar, MarketDataBatch, MarketDataSource, Period, Provenance, SourceError,
    },
    publish::{MarketDataPublisher, PublishError, PublishOutcome},
    storage::JsonFileStore,
};

fn bar(date: (i32, u32, u32), close: i64, volume: Option<Decimal>, amount: Option<Decimal>) -> Bar {
    Bar {
        date: chrono::NaiveDate::from_ymd_opt(date.0, date.1, date.2).unwrap(),
        open: Decimal::new(10, 0),
        high: Decimal::new(12, 0),
        low: Decimal::new(9, 0),
        close: Decimal::new(close, 0),
        volume,
        amount,
    }
}

fn batch(period: Period, bars: Vec<Bar>) -> MarketDataBatch {
    MarketDataBatch {
        symbol: "000001".into(),
        period,
        adjustment: Adjustment::Forward,
        bars,
        provenance: Provenance {
            source: "fake".into(),
            source_period: match period {
                Period::Day => "day",
                Period::Year => "year",
            }
            .into(),
            source_adjustment: "qfq".into(),
            symbol: "000001".into(),
            acquired_at: Utc::now(),
            adapter_version: "test".into(),
        },
    }
}

fn valid_day() -> MarketDataBatch {
    batch(
        Period::Day,
        vec![bar(
            (2026, 9, 10),
            11,
            Some(Decimal::ONE),
            Some(Decimal::TWO),
        )],
    )
}

#[test]
fn valid_day_and_year_publish_independent_effective_versions() {
    let dir = tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    let publisher = MarketDataPublisher::new(store.clone());
    let day = valid_day();
    let year = batch(
        Period::Year,
        vec![bar(
            (2025, 1, 1),
            11,
            Some(Decimal::ONE),
            Some(Decimal::TWO),
        )],
    );

    assert!(matches!(
        publisher.publish(&day),
        Ok(PublishOutcome::Published(_))
    ));
    assert!(matches!(
        publisher.publish(&year),
        Ok(PublishOutcome::Published(_))
    ));
    assert_eq!(
        store
            .get_latest_effective("000001", Period::Day)
            .unwrap()
            .unwrap()
            .records,
        day.bars
    );
    assert_eq!(
        store
            .get_latest_effective("000001", Period::Year)
            .unwrap()
            .unwrap()
            .records,
        year.bars
    );
}

#[test]
fn ohlc_and_negative_values_reject_without_publishing() {
    let cases = [
        {
            let mut value = valid_day();
            value.bars[0].high = Decimal::new(8, 0);
            value
        },
        {
            let mut value = valid_day();
            value.bars[0].open = Decimal::new(13, 0);
            value
        },
        {
            let mut value = valid_day();
            value.bars[0].close = Decimal::new(8, 0);
            value
        },
        {
            let mut value = valid_day();
            value.bars[0].volume = Some(Decimal::NEGATIVE_ONE);
            value
        },
        {
            let mut value = valid_day();
            value.bars[0].amount = Some(Decimal::NEGATIVE_ONE);
            value
        },
    ];
    for candidate in cases {
        let dir = tempdir().unwrap();
        let store = JsonFileStore::new(dir.path());
        assert!(matches!(
            MarketDataPublisher::new(store.clone()).publish(&candidate),
            Err(PublishError::InvalidRecord(_))
        ));
        assert!(
            store
                .get_latest_effective("000001", Period::Day)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn missing_volume_and_amount_remain_unknown() {
    let dir = tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    let candidate = batch(Period::Day, vec![bar((2026, 9, 10), 11, None, None)]);
    MarketDataPublisher::new(store.clone())
        .publish(&candidate)
        .unwrap();
    let latest = store
        .get_latest_effective("000001", Period::Day)
        .unwrap()
        .unwrap();
    assert_eq!(latest.records[0].volume, None);
    assert_eq!(latest.records[0].amount, None);
}

#[test]
fn identical_duplicate_keys_are_deduplicated_and_conflicting_keys_reject_scope() {
    let dir = tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    let duplicate = batch(
        Period::Day,
        vec![
            bar((2026, 9, 10), 11, Some(Decimal::ONE), Some(Decimal::TWO)),
            bar((2026, 9, 10), 11, Some(Decimal::ONE), Some(Decimal::TWO)),
        ],
    );
    let outcome = MarketDataPublisher::new(store.clone())
        .publish(&duplicate)
        .unwrap();
    let version = match outcome {
        PublishOutcome::Published(version) => version,
        other => panic!("unexpected outcome: {other:?}"),
    };
    assert_eq!(version.validation.duplicate_count, 1);
    assert_eq!(version.records.len(), 1);

    let mut conflict = valid_day();
    conflict.bars.push(bar(
        (2026, 9, 10),
        10,
        Some(Decimal::ONE),
        Some(Decimal::TWO),
    ));
    assert!(matches!(
        MarketDataPublisher::new(store.clone()).publish(&conflict),
        Err(PublishError::DuplicateConflict(_))
    ));
    assert_eq!(
        store
            .get_latest_effective("000001", Period::Day)
            .unwrap()
            .unwrap()
            .version_id,
        version.version_id
    );
}

#[test]
fn old_version_survives_rejection_no_change_and_unverified_revision() {
    let dir = tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    let publisher = MarketDataPublisher::new(store.clone());
    let original = valid_day();
    let first = match publisher.publish(&original).unwrap() {
        PublishOutcome::Published(version) => version,
        other => panic!("unexpected outcome: {other:?}"),
    };
    let no_change = publisher.publish(&original).unwrap();
    assert_eq!(
        no_change,
        PublishOutcome::NoChange {
            version_id: first.version_id.clone()
        }
    );
    let mut changed = original.clone();
    changed.bars[0].close = Decimal::new(12, 0);
    let revision = publisher.publish(&changed).unwrap();
    assert!(matches!(revision, PublishOutcome::NeedsVerification { .. }));
    let latest = store
        .get_latest_effective("000001", Period::Day)
        .unwrap()
        .unwrap();
    assert_eq!(latest.version_id, first.version_id);
    assert_eq!(latest.records, first.records);

    let rebuilt = JsonFileStore::new(dir.path());
    assert_eq!(
        rebuilt
            .get_latest_effective("000001", Period::Day)
            .unwrap()
            .unwrap()
            .version_id,
        first.version_id
    );
}

#[derive(Clone)]
struct TwoScopeSource;

impl MarketDataSource for TwoScopeSource {
    fn fetch(
        &self,
        symbol: &str,
        period: Period,
        adjustment: Adjustment,
    ) -> Result<MarketDataBatch, SourceError> {
        if period == Period::Day {
            let mut invalid = valid_day();
            invalid.symbol = symbol.into();
            invalid.provenance.symbol = symbol.into();
            invalid.bars[0].open = Decimal::new(20, 0);
            invalid.adjustment = adjustment;
            return Ok(invalid);
        }
        let mut valid = batch(
            period,
            vec![bar(
                (2025, 1, 1),
                11,
                Some(Decimal::ONE),
                Some(Decimal::TWO),
            )],
        );
        valid.symbol = symbol.into();
        valid.provenance.symbol = symbol.into();
        valid.adjustment = adjustment;
        Ok(valid)
    }
}

#[test]
fn one_scope_publish_rejection_does_not_block_other_scope() {
    let dir = tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    let results = Collector::new(TwoScopeSource, store.clone()).collect(
        &["000001"],
        &[Period::Day, Period::Year],
        Adjustment::Forward,
    );
    assert!(results[0].result.is_err());
    assert!(results[1].result.is_ok());
    assert!(
        store
            .get_latest_effective("000001", Period::Day)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_latest_effective("000001", Period::Year)
            .unwrap()
            .is_some()
    );
}
