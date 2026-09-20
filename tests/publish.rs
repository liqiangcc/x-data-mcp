use chrono::Utc;
use rust_decimal::Decimal;
use std::fs;
use tempfile::tempdir;
use x_data_mcp::{
    collector::Collector,
    market_data::{
        Adjustment, AdjustmentEvidence, Bar, EvidenceStatus, MarketDataBatch, MarketDataPipeline,
        Period, Provenance, SourceError, UnitCurrencyEvidence,
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
        r7_raw: None,
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
                Period::Year => "day",
            }
            .into(),
            source_adjustment: "qfq".into(),
            symbol: "000001".into(),
            acquired_at: Utc::now(),
            adapter_version: "test".into(),
            yearly_derivation: None,
            snapshot_inputs: None,
        },
        coverage: x_data_mcp::market_data::CoverageEvidence::complete("test fixture"),
        unit_currency: confirmed_units(),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "test fixture"),
    }
}

fn confirmed_units() -> UnitCurrencyEvidence {
    UnitCurrencyEvidence::confirmed("CNY", "share", "lot", "CNY", "yuan", "test fixture")
}

fn confirmed_adjustment() -> AdjustmentEvidence {
    AdjustmentEvidence::confirmed("qfq", "test fixture")
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
fn derived_yearly_retains_its_published_daily_version() {
    let dir = tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    let publisher = MarketDataPublisher::new(store.clone());
    let day = valid_day();
    let mut year = batch(
        Period::Year,
        vec![bar(
            (2026, 9, 10),
            11,
            Some(Decimal::ONE),
            Some(Decimal::TWO),
        )],
    );

    assert!(matches!(
        publisher.publish(&day),
        Ok(PublishOutcome::Published(_))
    ));
    let daily = store
        .get_latest_effective("000001", Period::Day)
        .unwrap()
        .unwrap();
    year.provenance.yearly_derivation = Some(x_data_mcp::market_data::YearlyDerivation {
        base_daily_version_id: daily.version_id.clone(),
        algorithm_version: "test-fixture".into(),
        first_trading_date: daily.records[0].date,
        last_trading_date: daily.records[0].date,
    });
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
    assert_eq!(
        store
            .get_latest_effective("000001", Period::Year)
            .unwrap()
            .unwrap()
            .yearly_derivation,
        year.provenance.yearly_derivation
    );
    assert!(!dir.path().join("000001.year.qfq.json").exists());
    assert_eq!(
        store
            .get("000001", Period::Year, Adjustment::Forward)
            .unwrap()
            .provenance
            .yearly_derivation,
        year.provenance.yearly_derivation
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
    let mut candidate = batch(Period::Day, vec![bar((2026, 9, 10), 11, None, None)]);
    candidate.unit_currency.volume_unit =
        x_data_mcp::market_data::EvidenceValue::unknown("volume was not supplied");
    candidate.unit_currency.amount_currency =
        x_data_mcp::market_data::EvidenceValue::unknown("amount was not supplied");
    candidate.unit_currency.amount_unit =
        x_data_mcp::market_data::EvidenceValue::unknown("amount was not supplied");
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
fn unknown_or_incompatible_adjustment_evidence_rejects_without_publish() {
    let cases = [
        AdjustmentEvidence::unknown("source qfq evidence unavailable"),
        AdjustmentEvidence::confirmed("hfq", "source used another adjustment"),
    ];
    for evidence in cases {
        let dir = tempdir().unwrap();
        let store = JsonFileStore::new(dir.path());
        let mut candidate = valid_day();
        candidate.adjustment_evidence = evidence;
        assert!(matches!(
            MarketDataPublisher::new(store.clone()).publish(&candidate),
            Err(PublishError::AdjustmentEvidence(_))
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
fn existing_version_and_invalid_adjustment_need_verification_without_change() {
    let cases = [
        AdjustmentEvidence::unknown("source qfq evidence unavailable"),
        AdjustmentEvidence::confirmed("hfq", "source used another adjustment"),
    ];
    for evidence in cases {
        let dir = tempdir().unwrap();
        let store = JsonFileStore::new(dir.path());
        let publisher = MarketDataPublisher::new(store.clone());
        let original = valid_day();
        let first = match publisher.publish(&original).unwrap() {
            PublishOutcome::Published(version) => version,
            other => panic!("unexpected outcome: {other:?}"),
        };
        let mut candidate = original.clone();
        candidate.adjustment_evidence = evidence;

        assert!(matches!(
            publisher.publish(&candidate).unwrap(),
            PublishOutcome::NeedsVerification { version_id, reason }
                if version_id == first.version_id
                    && reason == "candidate adjustment evidence is unknown or incompatible with qfq"
        ));
        let latest = store
            .get_latest_effective("000001", Period::Day)
            .unwrap()
            .unwrap();
        assert_eq!(latest.version_id, first.version_id);
        assert_eq!(latest.records, first.records);
        assert_eq!(latest.adjustment_evidence, first.adjustment_evidence);
    }
}

#[test]
fn unknown_price_or_provided_field_units_reject_without_publish() {
    let cases = [
        {
            let mut value = valid_day();
            value.unit_currency.price_currency =
                x_data_mcp::market_data::EvidenceValue::unknown("currency unavailable");
            value
        },
        {
            let mut value = valid_day();
            value.unit_currency.price_unit =
                x_data_mcp::market_data::EvidenceValue::unknown("price unit unavailable");
            value
        },
        {
            let mut value = valid_day();
            value.unit_currency.volume_unit =
                x_data_mcp::market_data::EvidenceValue::unknown("volume unit unavailable");
            value
        },
        {
            let mut value = valid_day();
            value.unit_currency.amount_currency =
                x_data_mcp::market_data::EvidenceValue::unknown("amount currency unavailable");
            value
        },
        {
            let mut value = valid_day();
            value.unit_currency.amount_unit =
                x_data_mcp::market_data::EvidenceValue::unknown("amount unit unavailable");
            value
        },
    ];
    for candidate in cases {
        let dir = tempdir().unwrap();
        let store = JsonFileStore::new(dir.path());
        assert!(matches!(
            MarketDataPublisher::new(store.clone()).publish(&candidate),
            Err(PublishError::UnitEvidence(_))
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

#[test]
fn unit_evidence_is_persisted_and_changes_are_not_no_change() {
    let dir = tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    let publisher = MarketDataPublisher::new(store.clone());
    let original = valid_day();
    let first = match publisher.publish(&original).unwrap() {
        PublishOutcome::Published(version) => version,
        other => panic!("unexpected outcome: {other:?}"),
    };

    let rebuilt = JsonFileStore::new(dir.path());
    assert_eq!(
        rebuilt
            .get_latest_effective("000001", Period::Day)
            .unwrap()
            .unwrap()
            .unit_currency,
        original.unit_currency
    );

    let mut changed = original.clone();
    changed.unit_currency.price_currency =
        x_data_mcp::market_data::EvidenceValue::confirmed("USD", "different source");
    assert!(matches!(
        publisher.publish(&changed).unwrap(),
        PublishOutcome::NeedsVerification { version_id, .. } if version_id == first.version_id
    ));
    assert_eq!(
        store
            .get_latest_effective("000001", Period::Day)
            .unwrap()
            .unwrap()
            .unit_currency,
        original.unit_currency
    );
}

#[test]
fn adjustment_evidence_is_persisted_and_changes_are_not_no_change() {
    let dir = tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    let publisher = MarketDataPublisher::new(store.clone());
    let original = valid_day();
    let first = match publisher.publish(&original).unwrap() {
        PublishOutcome::Published(version) => version,
        other => panic!("unexpected outcome: {other:?}"),
    };

    let rebuilt = JsonFileStore::new(dir.path());
    assert_eq!(
        rebuilt
            .get_latest_effective("000001", Period::Day)
            .unwrap()
            .unwrap()
            .adjustment_evidence,
        confirmed_adjustment()
    );

    let mut changed = original.clone();
    changed.adjustment_evidence =
        AdjustmentEvidence::unknown("qfq compatibility cannot be confirmed");
    assert!(matches!(
        publisher.publish(&changed).unwrap(),
        PublishOutcome::NeedsVerification { version_id, reason }
            if version_id == first.version_id
                && reason == "candidate adjustment evidence is unknown or incompatible with qfq"
    ));
    assert_eq!(
        store
            .get_latest_effective("000001", Period::Day)
            .unwrap()
            .unwrap()
            .version_id,
        first.version_id
    );

    let mut different_confirmed = original;
    different_confirmed.adjustment_evidence =
        AdjustmentEvidence::confirmed("qfq", "different compatible source evidence");
    assert!(matches!(
        publisher.publish(&different_confirmed).unwrap(),
        PublishOutcome::NeedsVerification { version_id, .. } if version_id == first.version_id
    ));
}

#[test]
fn old_effective_without_adjustment_field_reads_as_unknown_after_restart() {
    let dir = tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    MarketDataPublisher::new(store.clone())
        .publish(&valid_day())
        .unwrap();

    let path = dir.path().join("effective/000001.day.qfq.json");
    let mut payload: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    payload[0]
        .as_object_mut()
        .unwrap()
        .remove("adjustment_evidence");
    fs::write(&path, serde_json::to_vec_pretty(&payload).unwrap()).unwrap();

    let rebuilt = JsonFileStore::new(dir.path());
    let latest = rebuilt
        .get_latest_effective("000001", Period::Day)
        .unwrap()
        .unwrap();
    assert_eq!(latest.adjustment_evidence.status, EvidenceStatus::Unknown);
}

#[derive(Clone)]
struct TwoScopeSource;

impl TwoScopeSource {
    fn fetch(
        &self,
        symbol: &str,
        period: Period,
        adjustment: Adjustment,
    ) -> Result<MarketDataBatch, SourceError> {
        if symbol == "000001" {
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
        &["000001", "600000"],
        &[Period::Day],
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
            .get_latest_effective("600000", Period::Day)
            .unwrap()
            .is_some()
    );
}

#[derive(Clone)]
struct AdjustmentScopeSource;

impl AdjustmentScopeSource {
    fn fetch(
        &self,
        symbol: &str,
        period: Period,
        adjustment: Adjustment,
    ) -> Result<MarketDataBatch, SourceError> {
        let mut result = batch(
            period,
            vec![bar(
                if symbol == "000001" {
                    (2026, 9, 10)
                } else {
                    (2025, 1, 1)
                },
                11,
                Some(Decimal::ONE),
                Some(Decimal::TWO),
            )],
        );
        result.symbol = symbol.into();
        result.provenance.symbol = symbol.into();
        result.adjustment = adjustment;
        if symbol == "000001" {
            result.adjustment_evidence =
                AdjustmentEvidence::unknown("source qfq evidence unavailable");
        }
        Ok(result)
    }
}

#[test]
fn adjustment_rejection_does_not_block_another_scope() {
    let dir = tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    let results = Collector::new(AdjustmentScopeSource, store.clone()).collect(
        &["000001", "600000"],
        &[Period::Day],
        Adjustment::Forward,
    );
    assert!(results[0].result.is_err());
    assert!(
        results[0]
            .result
            .as_ref()
            .unwrap_err()
            .contains("adjustment evidence")
    );
    assert!(results[1].result.is_ok());
    assert!(
        store
            .get_latest_effective("000001", Period::Day)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_latest_effective("600000", Period::Day)
            .unwrap()
            .is_some()
    );
}

impl MarketDataPipeline for TwoScopeSource {
    fn materialize_daily(
        &self,
        symbol: &str,
        adjustment: Adjustment,
    ) -> Result<MarketDataBatch, SourceError> {
        self.fetch(symbol, Period::Day, adjustment)
    }

    fn derive_yearly(
        &self,
        daily: &x_data_mcp::publish::EffectiveVersion,
    ) -> Result<MarketDataBatch, SourceError> {
        let mut batch = self.fetch(&daily.symbol, Period::Year, daily.adjustment)?;
        batch.provenance.source_period = "day".into();
        batch.provenance.yearly_derivation = Some(x_data_mcp::market_data::YearlyDerivation {
            base_daily_version_id: daily.version_id.clone(),
            algorithm_version: "fake-yearly-fixture".into(),
            first_trading_date: daily.records.first().unwrap().date,
            last_trading_date: daily.records.last().unwrap().date,
        });
        Ok(batch)
    }
}

impl MarketDataPipeline for AdjustmentScopeSource {
    fn materialize_daily(
        &self,
        symbol: &str,
        adjustment: Adjustment,
    ) -> Result<MarketDataBatch, SourceError> {
        self.fetch(symbol, Period::Day, adjustment)
    }

    fn derive_yearly(
        &self,
        daily: &x_data_mcp::publish::EffectiveVersion,
    ) -> Result<MarketDataBatch, SourceError> {
        let mut batch = self.fetch(&daily.symbol, Period::Year, daily.adjustment)?;
        batch.provenance.source_period = "day".into();
        batch.provenance.yearly_derivation = Some(x_data_mcp::market_data::YearlyDerivation {
            base_daily_version_id: daily.version_id.clone(),
            algorithm_version: "fake-yearly-fixture".into(),
            first_trading_date: daily.records.first().unwrap().date,
            last_trading_date: daily.records.last().unwrap().date,
        });
        Ok(batch)
    }
}

// Issue #57: either a complete publish succeeds, or no new version is visible.
#[test]
fn obstructed_legacy_batch_path_cannot_produce_a_partial_publish() {
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("000001.day.qfq.json")).unwrap();
    let store = JsonFileStore::new(dir.path());
    let batch = valid_day();
    let outcome = MarketDataPublisher::new(store.clone()).publish(&batch);
    let restarted = JsonFileStore::new(dir.path());
    match outcome {
        Ok(PublishOutcome::Published(version)) => {
            assert_eq!(
                restarted
                    .get_latest_effective("000001", Period::Day)
                    .unwrap(),
                Some(version)
            );
            assert_eq!(
                restarted
                    .get("000001", Period::Day, Adjustment::Forward)
                    .unwrap(),
                batch
            );
        }
        Err(_) => assert!(
            restarted
                .get_latest_effective("000001", Period::Day)
                .unwrap()
                .is_none()
        ),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn committed_batch_wins_over_stale_legacy_batch_after_restart() {
    let dir = tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    let mut stale = valid_day();
    stale.provenance.adapter_version = "old-staging-only".into();
    store.put(&stale).unwrap();
    let committed = valid_day();
    MarketDataPublisher::new(store).publish(&committed).unwrap();
    let restarted = JsonFileStore::new(dir.path());
    assert_eq!(
        restarted
            .get("000001", Period::Day, Adjustment::Forward)
            .unwrap(),
        committed
    );
    let old_bytes = fs::read(dir.path().join("000001.day.qfq.json")).unwrap();
    assert_eq!(
        serde_json::from_slice::<MarketDataBatch>(&old_bytes).unwrap(),
        stale
    );
}

#[test]
fn verified_reads_pin_a_version_and_do_not_fall_back_from_damage() {
    let dir = tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    let PublishOutcome::Published(first) = MarketDataPublisher::new(store.clone())
        .publish(&valid_day())
        .unwrap()
    else {
        panic!()
    };
    let mut second = first.clone();
    second.version_id = "version-second".into();
    store.publish_effective(&second).unwrap();
    assert_eq!(
        store
            .get_verified_version(
                "000001",
                Period::Day,
                Adjustment::Forward,
                Some(&first.version_id)
            )
            .unwrap(),
        Some(first.clone())
    );
    assert_eq!(
        store
            .list_verified_versions("000001", Period::Day, Adjustment::Forward)
            .unwrap(),
        vec![first.clone(), second]
    );
    let path = dir.path().join("effective/000001.day.qfq.json");
    let mut values: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    values[1]["records"][0]["close"] = serde_json::json!("10.5");
    fs::write(&path, serde_json::to_vec(&values).unwrap()).unwrap();
    let before = fs::read(&path).unwrap();
    assert!(
        store
            .get_verified_version("000001", Period::Day, Adjustment::Forward, None)
            .is_err()
    );
    assert!(
        store
            .get_verified_version(
                "000001",
                Period::Day,
                Adjustment::Forward,
                Some("version-second")
            )
            .is_err()
    );
    assert_eq!(
        store
            .get_verified_version(
                "000001",
                Period::Day,
                Adjustment::Forward,
                Some(&first.version_id)
            )
            .unwrap(),
        Some(first)
    );
    assert!(
        store
            .get_verified_version("000001", Period::Day, Adjustment::Forward, Some("missing"))
            .is_err()
    );
    assert!(
        store
            .list_verified_versions("000001", Period::Day, Adjustment::Forward)
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn legacy_without_integrity_remains_unknown_without_read_side_effects() {
    let dir = tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    let PublishOutcome::Published(version) = MarketDataPublisher::new(store.clone())
        .publish(&valid_day())
        .unwrap()
    else {
        panic!()
    };
    let path = dir.path().join("effective/000001.day.qfq.json");
    fs::write(&path, serde_json::to_vec(&vec![version.clone()]).unwrap()).unwrap();
    let before = fs::read(&path).unwrap();
    assert!(
        store
            .get_verified_version("000001", Period::Day, Adjustment::Forward, None)
            .is_err()
    );
    assert_eq!(
        store.get_latest_effective("000001", Period::Day).unwrap(),
        Some(version)
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(
        store
            .get_verified_version("../escape", Period::Day, Adjustment::Forward, None)
            .is_err()
    );
}

#[test]
fn verified_read_does_not_create_empty_storage_or_claim_data_exists() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("missing");
    let store = JsonFileStore::new(&root);
    assert!(
        store
            .get_verified_version("sh.600006", Period::Day, Adjustment::Forward, None)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_verified_version(
                "sh.600006",
                Period::Day,
                Adjustment::Forward,
                Some("unknown")
            )
            .is_err()
    );
    assert!(!root.exists());
}

#[test]
fn dedup_counts_identical_duplicates_and_sorts_unique_records() {
    // Identical keys with identical content count as duplicates; output is
    // sorted unique records. Semantics pinned by issue #204 (O(N) membership).
    let batch = batch(
        Period::Day,
        vec![
            bar((2026, 9, 12), 11, Some(Decimal::ONE), Some(Decimal::TWO)),
            bar((2026, 9, 10), 11, Some(Decimal::ONE), Some(Decimal::TWO)),
            bar((2026, 9, 12), 11, Some(Decimal::ONE), Some(Decimal::TWO)),
            bar((2026, 9, 11), 11, Some(Decimal::ONE), Some(Decimal::TWO)),
        ],
    );
    let (records, summary) = x_data_mcp::publish::validate_and_deduplicate(&batch).unwrap();
    assert_eq!(summary.record_count, 4);
    assert_eq!(summary.unique_record_count, 3);
    assert_eq!(summary.duplicate_count, 1);
    let dates: Vec<_> = records.iter().map(|b| b.date.to_string()).collect();
    assert_eq!(dates, ["2026-09-10", "2026-09-11", "2026-09-12"]);
}

#[test]
fn dedup_rejects_conflicting_duplicate_with_record_key() {
    let batch = batch(
        Period::Day,
        vec![
            bar((2026, 9, 10), 11, Some(Decimal::ONE), Some(Decimal::TWO)),
            bar((2026, 9, 10), 12, Some(Decimal::ONE), Some(Decimal::TWO)),
        ],
    );
    match x_data_mcp::publish::validate_and_deduplicate(&batch) {
        Err(PublishError::DuplicateConflict(key)) => assert_eq!(key, "2026-09-10"),
        other => panic!("expected DuplicateConflict, got {other:?}"),
    }
}

#[test]
fn dedup_keys_yearly_records_by_year() {
    // Year period keys on the natural year only: two different dates in the
    // same year conflict, different years keep both.
    let conflict = batch(
        Period::Year,
        vec![
            bar((2026, 6, 30), 11, Some(Decimal::ONE), Some(Decimal::TWO)),
            bar((2026, 12, 31), 12, Some(Decimal::ONE), Some(Decimal::TWO)),
        ],
    );
    match x_data_mcp::publish::validate_and_deduplicate(&conflict) {
        Err(PublishError::DuplicateConflict(key)) => assert_eq!(key, "2026"),
        other => panic!("expected DuplicateConflict, got {other:?}"),
    }
    let distinct = batch(
        Period::Year,
        vec![
            bar((2026, 12, 31), 11, Some(Decimal::ONE), Some(Decimal::TWO)),
            bar((2025, 12, 31), 10, Some(Decimal::ONE), Some(Decimal::TWO)),
        ],
    );
    let (records, summary) = x_data_mcp::publish::validate_and_deduplicate(&distinct).unwrap();
    assert_eq!(summary.unique_record_count, 2);
    assert_eq!(summary.duplicate_count, 0);
    let dates: Vec<_> = records.iter().map(|b| b.date.to_string()).collect();
    assert_eq!(dates, ["2025-12-31", "2026-12-31"]);
}
