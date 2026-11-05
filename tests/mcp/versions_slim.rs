//! #295/#525: the `versions` block of a v2 price read stays small.
//!
//! The fixtures follow the evidence texts the writers emit
//! (`src/official_market.rs`, `src/yearly_publication/runner.rs`): per-entity
//! counts inside `adjustment.reason`, a base-daily id inside the yearly
//! `coverage_reason`, the same unit sentences for every entity. They are a
//! model of production rows, not production bytes.
//!
//! `old_shape` rebuilds what the response looked like before the block was
//! slimmed (only a value every entity shares goes to `metadata`, every long
//! text inline). The tests check that the new shape decodes to exactly that
//! information and that its bytes stay under a fixed cap.
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use x_data_mcp::data_supply::{
    columnar::{self, PageMeta},
    wire::*,
};

const SCOPE: &str = "DECLARED_SCOPE: owner personal research, local-only, no external exposure \
                     (#71 comment-5749934485); external distribution is not authorized";

fn hex(tag: &str) -> String {
    Sha256::digest(tag.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn evidence(field: &str, value: &str, reason: &str) -> Evidence {
    Evidence {
        field: field.into(),
        status: "confirmed".into(),
        value: Some(value.into()),
        reason: reason.into(),
    }
}

fn stock_units() -> Vec<Evidence> {
    let price = ".day raw OHLC are fen integers of yuan-denominated prices (#166/#167 anchored)";
    let volume = ".day volume is u32 shares; anchored by #239 TdxQuant oracle (37,345-bar full-history match, PR #242); not adjustment-adjusted";
    let amount = ".day amount is f32 yuan; anchored by #239 TdxQuant oracle (37,288/37,345 within tolerance, PR #242); not adjustment-adjusted";
    vec![
        evidence("price_currency", "CNY", price),
        evidence("price_unit", "yuan", price),
        evidence("volume_unit", "share", volume),
        evidence("amount_currency", "CNY", amount),
        evidence("amount_unit", "yuan", amount),
    ]
}

fn etf_units() -> Vec<Evidence> {
    vec![
        evidence(
            "price_currency",
            "CNY",
            "ETF `.day` raw OHLC are 厘 integers of yuan-denominated prices; 861/861 SSE prev_close and 558/558 SZSE OHLC match raw/1000 (M3-2 §2.1)",
        ),
        evidence(
            "price_unit",
            "yuan",
            "ETF `.day` price unit is yuan at price_scale=3 (厘); raw/100 matches 0/861 SSE and 0/558 SZSE samples, so 2dp is a 10× misprice",
        ),
        evidence(
            "volume_unit",
            "share",
            "ETF `.day` volume is 份; hand-recorded bars (r7 marker + amount/volume consistency) are multiplied by 100 and stored with volume_quantum=100",
        ),
        evidence(
            "amount_currency",
            "CNY",
            "ETF `.day` amount is f32 yuan; 557/558 SZSE samples match the official 万元×1e4 value within 1000 元",
        ),
        evidence(
            "amount_unit",
            "yuan",
            "ETF `.day` amount is f32 yuan and is never converted or adjustment-scaled",
        ),
    ]
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    StockDaily,
    StockYearly,
    Etf,
}

fn security(kind: Kind, i: usize) -> Security {
    match kind {
        Kind::Etf => Security {
            exchange: Exchange::Sh,
            code: format!("{:06}", 510_000 + i),
        },
        _ => Security {
            exchange: if i.is_multiple_of(2) {
                Exchange::Sh
            } else {
                Exchange::Sz
            },
            code: format!("{:06}", 600_000 + i),
        },
    }
}

fn day(text: &str) -> NaiveDate {
    text.parse().unwrap()
}

fn metadata(kind: Kind, i: usize) -> Metadata {
    let published_at: DateTime<Utc> =
        DateTime::from_timestamp(1_793_000_000 + (i as i64) * 37, 123_456_000).unwrap();
    let manifest = hex("manifest");
    let (coverage_reason, units, adjustment, lineage) = match kind {
        Kind::StockDaily => (
            format!(
                "complete .day member history from inspected snapshot manifest {manifest}; {SCOPE}"
            ),
            stock_units(),
            evidence(
                "price_adjustment",
                "qfq",
                &format!(
                    "qfq-exact-v1: exact rational affine chain over category==1 GBBQ events, one final HALF_UP rounding at price_scale=6 ({} events, {} non-category-1 records excluded uninterpreted); {SCOPE}",
                    (i * 7) % 23,
                    (i * 13) % 41
                ),
            ),
            None,
        ),
        Kind::StockYearly => {
            let base = hex(&format!("base{i}"));
            (
                format!(
                    "selected years verified by re-aggregating fixed daily {base} against bound generation {} as of 2026-11-05; not a listing-history claim",
                    hex("generation")
                ),
                stock_units(),
                evidence(
                    "price_adjustment",
                    "qfq",
                    &format!(
                        "qfq-exact-v1: exact rational affine chain over category==1 GBBQ events, one final HALF_UP rounding at price_scale=6 ({} events, {} non-category-1 records excluded uninterpreted); {SCOPE}",
                        (i * 7) % 23,
                        (i * 13) % 41
                    ),
                ),
                Some(Lineage {
                    base_daily_version_id: base,
                    algorithm_version: "local-natural-year-v1".into(),
                    first_trading_date: day(&format!("20{:02}-01-04", 1 + i % 22)),
                    last_trading_date: day("2025-12-31"),
                }),
            )
        }
        Kind::Etf => (
            format!(
                "complete .day member history from inspected snapshot manifest {manifest}; tail zero-volume bars are preserved as recorded (no truncation, no fill); {SCOPE}"
            ),
            etf_units(),
            evidence(
                "price_adjustment",
                "raw",
                &format!(
                    ".day OHLC promoted verbatim with price_scale=3 (厘, no adjustment); category-1 and category-11 ETF factor facts are stored but not applied to raw prices (other gbbq categories are excluded uninterpreted); {SCOPE}"
                ),
            ),
            None,
        ),
    };
    Metadata {
        source: "tdx-official-file-snapshot".into(),
        published_at,
        coverage_status: "complete".into(),
        coverage_reason,
        units,
        adjustment: Some(adjustment),
        lineage,
        index: None,
        verified_through: None,
        price_scale: (kind == Kind::Etf).then_some(3),
        last_trade_date: (kind == Kind::Etf).then(|| day("2026-11-05")),
        delisting_date: None,
    }
}

fn entry(kind: Kind, i: usize) -> ManifestEntry {
    let entity = security(kind, i);
    ManifestEntry {
        version_ref: Some(VersionRef {
            dataset: Dataset::Daily,
            contract_version: ContractVersion::V1,
            entity: Some(entity.clone()),
            version_id: hex(&format!("version{}{i}", kind as u8)),
            integrity_sha256: String::new(),
        }),
        entity: Some(entity),
        status: ManifestStatus::Present,
        metadata: Some(metadata(kind, i)),
        stale: None,
        as_of_day: None,
        reason: None,
    }
}

fn bars(entities: &[Security], yearly: bool) -> Vec<Row> {
    let dates: &[&str] = if yearly {
        &["2022-12-30", "2023-12-29", "2024-12-31", "2025-12-31"]
    } else {
        &[
            "2026-10-30",
            "2026-11-02",
            "2026-11-03",
            "2026-11-04",
            "2026-11-05",
        ]
    };
    let mut rows = Vec::new();
    for entity in entities {
        for (index, date) in dates.iter().enumerate() {
            rows.push(Row::Market {
                entity: entity.clone(),
                version_ref: VersionRef {
                    dataset: Dataset::Daily,
                    contract_version: ContractVersion::V1,
                    entity: Some(entity.clone()),
                    version_id: String::new(),
                    integrity_sha256: String::new(),
                },
                date: day(date),
                first_trading_date: yearly.then(|| day("2022-01-04")),
                last_trading_date: yearly.then(|| day(date)),
                year_unfinished: yearly.then_some(false),
                open: format!("{}.{:06}", 10 + index, entity.code.len() * 1111),
                high: format!("{}.{:06}", 11 + index, 482_517),
                low: format!("{}.{:06}", 9 + index, 117_330),
                close: format!("{}.{:06}", 10 + index, 903_114),
                volume: Some(format!("{}", 1_000_000 * (index + 1))),
                amount: Some(format!("{}.5", 12_345_678 * (index + 1))),
                no_trade: None,
            });
        }
    }
    rows
}

/// One `panel` answer for `manifest` (the shape `query_data` returns for
/// `single` and `batch`).
fn panel(dataset: Dataset, manifest: &[ManifestEntry], yearly: bool) -> Value {
    let entities: Vec<Security> = manifest
        .iter()
        .map(|entry| entry.entity.clone().unwrap())
        .collect();
    let rows = bars(&entities, yearly);
    let fields: Vec<String> = if yearly {
        columnar::YEARLY_FIELDS
    } else {
        &columnar::DAILY_FIELDS[..6]
    }
    .iter()
    .map(|field| (*field).to_owned())
    .collect();
    columnar::panel(
        dataset,
        manifest,
        &entities,
        &rows,
        &fields,
        &PageMeta {
            snapshot_id: "snap-1",
            snapshot_time: "2026-11-05T10:00:00Z".parse().unwrap(),
            next_cursor: None,
            page_complete: true,
            limitations: &[],
            temporal: None,
        },
    )
}

fn bytes(value: &Value) -> usize {
    serde_json::to_vec(value).unwrap().len()
}

/// The metadata keys `split_metadata` has always moved between the shared
/// block and the rows.
const KEYS: &[&str] = &[
    "source",
    "coverage_status",
    "coverage_reason",
    "units",
    "adjustment",
    "lineage",
    "last_trade_date",
    "price_scale",
];

/// What the response said before this change: a key is shared only when every
/// present entry holds the same value, otherwise each row carries its own.
fn old_shape(manifest: &[ManifestEntry]) -> (Vec<Value>, Map<String, Value>) {
    let values: Vec<Value> = manifest
        .iter()
        .map(|entry| serde_json::to_value(entry.metadata.as_ref().unwrap()).unwrap())
        .collect();
    let mut shared = Map::new();
    let mut rows: Vec<Map<String, Value>> = manifest
        .iter()
        .zip(&values)
        .map(|(entry, value)| {
            let mut row = Map::new();
            row.insert(
                "entity".into(),
                json!(entry.entity.as_ref().unwrap().symbol()),
            );
            row.insert("status".into(), json!("present"));
            row.insert(
                "version_id".into(),
                json!(entry.version_ref.as_ref().unwrap().version_id),
            );
            row.insert("published_at".into(), value["published_at"].clone());
            row
        })
        .collect();
    for key in KEYS {
        let column: Vec<&Value> = values
            .iter()
            .map(|value| value.get(*key).unwrap_or(&Value::Null))
            .collect();
        if column.iter().all(|value| value.is_null()) {
            continue;
        }
        if column.windows(2).all(|pair| pair[0] == pair[1]) {
            shared.insert((*key).to_owned(), column[0].clone());
        } else {
            for (row, value) in rows.iter_mut().zip(column) {
                if !value.is_null() {
                    row.insert((*key).to_owned(), value.clone());
                }
            }
        }
    }
    (rows.into_iter().map(Value::Object).collect(), shared)
}

/// Rebuild one text field: a string is the text itself, an array is parts
/// (a number indexes `texts`, a string is literal).
fn text(value: &Value, texts: &[Value]) -> Value {
    match value {
        Value::Array(parts) => Value::String(
            parts
                .iter()
                .map(|part| match part {
                    Value::Number(index) => texts[index.as_u64().unwrap() as usize]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                    Value::String(literal) => literal.clone(),
                    other => panic!("unexpected part {other}"),
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

pub(crate) fn expand(value: &Value, texts: &[Value]) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| match key.strip_suffix("_parts") {
                    Some(name) => (name.to_owned(), text(value, texts)),
                    None => (key.clone(), expand(value, texts)),
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(|item| expand(item, texts)).collect()),
        other => other.clone(),
    }
}

/// The reader's rule: expand texts, then lay each row's object keys over the
/// shared `metadata` object of the same key, field by field.
fn decode(page: &Value) -> (Vec<Value>, Map<String, Value>) {
    let texts = page["texts"].as_array().cloned().unwrap_or_default();
    let metadata = expand(&page["metadata"], &texts);
    let metadata = metadata.as_object().cloned().unwrap_or_default();
    let mut shared = Map::new();
    let mut rows = Vec::new();
    let mut touched: Vec<String> = Vec::new();
    for row in page["versions"].as_array().unwrap() {
        let row = expand(row, &texts);
        let mut merged = row.as_object().unwrap().clone();
        for (key, value) in &metadata {
            if let (Some(Value::Object(own)), Value::Object(common)) = (merged.get(key), value) {
                let mut both = common.clone();
                both.extend(own.clone());
                merged.insert(key.clone(), Value::Object(both));
                touched.push(key.clone());
            }
        }
        rows.push(Value::Object(merged));
    }
    // A key that some row overrides is no longer "the same for everyone":
    // give the rows without an override the shared object, and the old shape
    // keeps that key on the rows.
    for (key, value) in &metadata {
        if touched.contains(key) {
            for row in rows.iter_mut() {
                row.as_object_mut()
                    .unwrap()
                    .entry(key.clone())
                    .or_insert_with(|| value.clone());
            }
        } else {
            shared.insert(key.clone(), value.clone());
        }
    }
    (rows, shared)
}

fn assert_lossless(name: &str, manifest: &[ManifestEntry], page: &Value) {
    let (old_rows, old_shared) = old_shape(manifest);
    let (rows, shared) = decode(page);
    assert_eq!(shared, old_shared, "{name}: shared metadata");
    assert_eq!(rows, old_rows, "{name}: versions rows");
}

fn old_bytes(manifest: &[ManifestEntry], page: &Value) -> usize {
    let (rows, shared) = old_shape(manifest);
    let mut old = page.clone();
    let object = old.as_object_mut().unwrap();
    object.remove("texts");
    object.insert("versions".into(), Value::Array(rows));
    if shared.is_empty() {
        object.remove("metadata");
    } else {
        object.insert("metadata".into(), Value::Object(shared));
    }
    bytes(&old)
}

struct Case {
    name: &'static str,
    manifest: Vec<ManifestEntry>,
    yearly: bool,
    /// Fixed upper bound on the whole response (bytes of compact JSON), about
    /// 3% above what the fixture measured when the cap was set.
    cap: usize,
    /// The same response in the old shape, as measured before the change.
    before: usize,
}

fn cases() -> Vec<Case> {
    let many = |kind, n: usize| (0..n).map(|i| entry(kind, i)).collect::<Vec<_>>();
    vec![
        Case {
            name: "yearly batch of 100 A-shares",
            manifest: many(Kind::StockYearly, 100),
            yearly: true,
            cap: 99_500,
            before: 157_097,
        },
        Case {
            name: "daily batch of 100 A-shares",
            manifest: many(Kind::StockDaily, 100),
            yearly: false,
            cap: 70_200,
            before: 103_420,
        },
        Case {
            name: "one A-share, daily",
            manifest: many(Kind::StockDaily, 1),
            yearly: false,
            cap: 2_520,
            before: 2_604,
        },
        Case {
            name: "one A-share, yearly",
            manifest: many(Kind::StockYearly, 1),
            yearly: true,
            cap: 2_850,
            before: 2_920,
        },
        Case {
            name: "one A-share and one ETF",
            manifest: vec![entry(Kind::StockDaily, 0), entry(Kind::Etf, 0)],
            yearly: false,
            cap: 4_580,
            before: 5_019,
        },
        Case {
            name: "100 A-shares and 100 ETFs",
            manifest: many(Kind::StockDaily, 100)
                .into_iter()
                .chain(many(Kind::Etf, 100))
                .collect(),
            yearly: false,
            cap: 236_000,
            before: 462_829,
        },
    ]
}

/// Fixed fixtures: the response bytes stay under their cap, and are below
/// what the same answer cost before the change.
#[test]
fn response_bytes_stay_under_the_cap() {
    for case in cases() {
        let page = panel(Dataset::Daily, &case.manifest, case.yearly);
        let size = bytes(&page);
        eprintln!(
            "{:34} {size:7} B (before {:7} B, {:+.1}%), versions {:6} B",
            case.name,
            case.before,
            (size as f64 / case.before as f64 - 1.0) * 100.0,
            bytes(&page["versions"]),
        );
        assert!(
            size <= case.cap,
            "{}: {size} B over the cap {}",
            case.name,
            case.cap
        );
        assert!(
            size < case.before,
            "{}: {size} B is not below {}",
            case.name,
            case.before
        );
        // The cost of the old shape is a property of the fixture, not of this
        // change: rebuilding it from the manifest must give the figure above.
        assert_eq!(
            old_bytes(&case.manifest, &page),
            case.before,
            "{}",
            case.name
        );
    }
}

/// Every field of the old `versions` rows and of the old shared `metadata` is
/// recoverable from the new shape.
#[test]
fn the_new_shape_decodes_to_the_old_one() {
    for case in cases() {
        let page = panel(Dataset::Daily, &case.manifest, case.yearly);
        assert_lossless(case.name, &case.manifest, &page);
    }
}

/// `adjustment.value` is `qfq` and stays in the shared `metadata` whenever
/// every entity has it; `published_at` stays on the row; the coverage fields
/// stay under the names and places they had.
#[test]
fn consumer_contract_fields_keep_name_and_place() {
    let manifest: Vec<ManifestEntry> = (0..100).map(|i| entry(Kind::StockYearly, i)).collect();
    let page = panel(Dataset::Yearly, &manifest, true);
    assert_eq!(page["metadata"]["adjustment"]["value"], "qfq");
    assert_eq!(page["metadata"]["adjustment"]["status"], "confirmed");
    assert_eq!(page["metadata"]["adjustment"]["field"], "price_adjustment");
    assert_eq!(page["metadata"]["coverage_status"], "complete");
    for row in page["versions"].as_array().unwrap() {
        assert!(row["published_at"].is_string());
        assert!(row["version_id"].is_string());
        // Per entity facts that differ stay on the row under their own names.
        assert!(row["lineage"]["base_daily_version_id"].is_string());
        assert!(row.get("coverage_reason").is_none());
        assert!(row["coverage_reason_parts"].is_array());
    }
    // A single entity keeps everything in `metadata`, as before.
    let one = panel(Dataset::Daily, &manifest[..1], false);
    assert_eq!(one["metadata"]["adjustment"]["value"], "qfq");
    assert!(
        one["metadata"]["coverage_reason"].is_string()
            || one["metadata"]["coverage_reason_parts"].is_array()
    );
}

/// A and ETF mixed: `adjustment.value` differs (`qfq`, `raw`), so the value
/// moves to the rows while `field` and `status` stay shared.
#[test]
fn mixed_adjustment_values_stay_per_row() {
    let manifest = vec![entry(Kind::StockDaily, 0), entry(Kind::Etf, 0)];
    let page = panel(Dataset::Daily, &manifest, false);
    assert_eq!(page["metadata"]["adjustment"]["field"], "price_adjustment");
    assert!(page["metadata"]["adjustment"].get("value").is_none());
    assert_eq!(page["versions"][0]["adjustment"]["value"], "qfq");
    assert_eq!(page["versions"][1]["adjustment"]["value"], "raw");
    assert_lossless("mixed", &manifest, &page);
}

/// Absent entries take no part in sharing and carry no text parts.
#[test]
fn absent_entries_are_untouched() {
    let mut manifest: Vec<ManifestEntry> = (0..10).map(|i| entry(Kind::StockYearly, i)).collect();
    manifest[3] = ManifestEntry {
        entity: manifest[3].entity.clone(),
        version_ref: None,
        status: ManifestStatus::Absent,
        metadata: None,
        stale: None,
        as_of_day: None,
        reason: Some(AbsentReason::DailyReplacedSinceDerivation),
    };
    let page = panel(Dataset::Yearly, &manifest, true);
    let absent = &page["versions"][3];
    assert_eq!(absent["status"], "absent");
    assert_eq!(absent["reason"], "daily_replaced_since_derivation");
    assert_eq!(absent.as_object().unwrap().len(), 3, "{absent}");
    // The nine present rows decode to what they were.
    let present: Vec<ManifestEntry> = manifest
        .iter()
        .filter(|entry| entry.metadata.is_some())
        .cloned()
        .collect();
    let (old_rows, old_shared) = old_shape(&present);
    let (mut rows, shared) = decode(&page);
    rows.remove(3);
    assert_eq!(shared, old_shared);
    assert_eq!(rows, old_rows);
}

/// Texts that differ inside multibyte characters are cut on character
/// boundaries and join back byte for byte.
#[test]
fn multibyte_texts_round_trip() {
    let mut manifest: Vec<ManifestEntry> = (0..6).map(|i| entry(Kind::Etf, i)).collect();
    for (i, entry) in manifest.iter_mut().enumerate() {
        let adjustment = entry
            .metadata
            .as_mut()
            .unwrap()
            .adjustment
            .as_mut()
            .unwrap();
        adjustment.reason = format!(
            "不复权口径：厘 and 份 are stored as recorded 厘厘厘厘厘厘厘厘厘厘厘厘 ({} 笔事件，{} 条排除) 厘厘厘厘厘厘厘厘厘厘厘厘 raw 价格不调整",
            i * 3,
            i * 5
        );
    }
    let page = panel(Dataset::Daily, &manifest, false);
    assert!(page["texts"].is_array());
    assert_lossless("multibyte", &manifest, &page);
}

/// Same input, same bytes.
#[test]
fn the_table_is_deterministic() {
    let manifest: Vec<ManifestEntry> = (0..20).map(|i| entry(Kind::StockYearly, i)).collect();
    let first = serde_json::to_string(&panel(Dataset::Yearly, &manifest, true)).unwrap();
    let second = serde_json::to_string(&panel(Dataset::Yearly, &manifest, true)).unwrap();
    assert_eq!(first, second);
}

/// A moved text is always smaller in the table than inline, and every index
/// points inside the table.
#[test]
fn text_references_are_valid_and_pay_off() {
    let manifest: Vec<ManifestEntry> = (0..100).map(|i| entry(Kind::StockDaily, i)).collect();
    let page = panel(Dataset::Daily, &manifest, false);
    let texts = page["texts"].as_array().unwrap();
    let mut referenced = vec![false; texts.len()];
    fn walk(value: &Value, referenced: &mut [bool]) {
        match value {
            Value::Object(object) => {
                for (key, value) in object {
                    if key.ends_with("_parts") {
                        for part in value.as_array().unwrap() {
                            if let Some(index) = part.as_u64() {
                                referenced[index as usize] = true;
                            }
                        }
                    } else {
                        walk(value, referenced);
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|item| walk(item, referenced)),
            _ => {}
        }
    }
    walk(&page["versions"], &mut referenced);
    walk(&page["metadata"], &mut referenced);
    assert!(
        referenced.iter().all(|used| *used),
        "an entry of texts is never referenced"
    );
    assert!(texts.len() < 20, "{} table entries", texts.len());
}
