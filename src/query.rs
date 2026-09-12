//! Read-only pagination of integrity-checked published records.
use crate::{
    market_data::{Adjustment, Period},
    publish::EffectiveVersion,
    storage::JsonFileStore,
};
use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{cell::RefCell, collections::VecDeque};

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MarketQuery {
    pub exchange: String,
    pub code: String,
    pub period: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub version_id: Option<String>,
    pub page_size: Option<usize>,
    pub cursor: Option<String>,
}
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VersionQuery {
    pub exchange: String,
    pub code: String,
    pub period: String,
    pub page_size: Option<usize>,
    pub cursor: Option<String>,
}
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("invalid query: {0}")]
    Invalid(&'static str),
    #[error("invalid, expired or mismatched cursor; start a new query")]
    Cursor,
    #[error("published data unavailable: {0}")]
    Unavailable(String),
}
#[derive(Clone)]
struct Page {
    token: String,
    key: Value,
    rows: Vec<Value>,
    metadata: Value,
    versions: Vec<EffectiveVersion>,
    offset: usize,
}
pub struct QueryService {
    store: JsonFileStore,
    pages: RefCell<VecDeque<Page>>,
    nonce: String,
    sequence: RefCell<u64>,
}
impl QueryService {
    pub fn new(store: JsonFileStore) -> Self {
        Self {
            store,
            pages: RefCell::new(VecDeque::new()),
            nonce: format!(
                "{}-{}",
                std::process::id(),
                Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ),
            sequence: RefCell::new(0),
        }
    }
    pub fn market(&self, mut args: MarketQuery, now: DateTime<Utc>) -> Result<Value, QueryError> {
        let (symbol, period, size) =
            scope(&args.exchange, &args.code, &args.period, args.page_size)?;
        if args.start_date.year() <= 0
            || args.end_date.year() > 9999
            || args.start_date > args.end_date
        {
            return Err(QueryError::Invalid("invalid date range"));
        }
        if args
            .version_id
            .as_ref()
            .is_some_and(|id| id.trim().is_empty())
        {
            return Err(QueryError::Invalid("empty version_id"));
        }
        let cursor = args.cursor.take();
        let key = json!([
            "market",
            args.exchange,
            args.code,
            args.period,
            args.start_date,
            args.end_date,
            args.version_id,
            size
        ]);
        if let Some(cursor) = cursor {
            return self.resume(&cursor, &key, size);
        }
        let version = self
            .store
            .get_verified_version(
                &symbol,
                period,
                Adjustment::Forward,
                args.version_id.as_deref(),
            )
            .map_err(unavailable)?;
        let mut rows = Vec::new();
        let mut versions = Vec::new();
        let today = shanghai_date(now);
        let mut metadata = json!({"symbol":symbol,"period":args.period,"adjustment":"qfq","query_time":now.with_timezone(&FixedOffset::east_opt(28800).unwrap()),"start_date":args.start_date,"end_date":args.end_date,"coverage":"UNKNOWN","missing_data_reason":"UNKNOWN","future_interval_not_occurred":args.end_date>today,"version_id":null});
        if let Some(version) = version {
            if version
                .records
                .windows(2)
                .any(|pair| pair[0].date >= pair[1].date)
            {
                return Err(QueryError::Unavailable(
                    "invalid published date order".into(),
                ));
            }
            if !version.adjustment_evidence.is_confirmed()
                || version.adjustment_evidence.value.as_deref() != Some("qfq")
                || version.source.trim().is_empty()
            {
                return Err(QueryError::Unavailable(
                    "published QFQ/source evidence missing".into(),
                ));
            }
            metadata["first_record_date"] = json!(version.records.first().map(|r| r.date));
            metadata["last_record_date"] = json!(version.records.last().map(|r| r.date));
            metadata["freshness"] = json!("NOT_VERIFIED");
            metadata["version_id"] = json!(version.version_id);
            metadata["source"] = json!(version.source);
            metadata["published_at"] = json!(version.published_at);
            metadata["unit_currency"] = json!(version.unit_currency);
            metadata["adjustment_evidence"] = json!(version.adjustment_evidence);
            if period == Period::Year {
                let lineage = version
                    .yearly_derivation
                    .as_ref()
                    .ok_or_else(|| QueryError::Unavailable("yearly lineage missing".into()))?;
                if lineage.algorithm_version != crate::yearly::ALGORITHM_VERSION {
                    return Err(QueryError::Unavailable(
                        "yearly algorithm not supported for verification".into(),
                    ));
                }
                let daily = self
                    .store
                    .get_verified_version(
                        &symbol,
                        Period::Day,
                        Adjustment::Forward,
                        Some(&lineage.base_daily_version_id),
                    )
                    .map_err(unavailable)?
                    .ok_or_else(|| QueryError::Unavailable("base daily missing".into()))?;
                if daily.source != version.source || daily.unit_currency != version.unit_currency {
                    return Err(QueryError::Unavailable(
                        "yearly base identity/units mismatch".into(),
                    ));
                }
                let years = version.records.iter().map(|bar| bar.date.year()).collect();
                let annual = crate::yearly::aggregate(
                    &daily,
                    Some(&years),
                    shanghai_date(version.published_at),
                )
                .map_err(unavailable)?;
                if annual
                    .records
                    .iter()
                    .map(|r| &r.bar)
                    .ne(version.records.iter())
                    || annual.records.first().map(|r| r.lineage.first_trading_date)
                        != Some(lineage.first_trading_date)
                    || annual.records.last().map(|r| r.lineage.last_trading_date)
                        != Some(lineage.last_trading_date)
                {
                    return Err(QueryError::Unavailable(
                        "published yearly differs from fixed daily aggregation".into(),
                    ));
                }
                metadata["derivation_as_of"] = json!(shanghai_date(version.published_at));
                metadata["derivation"] = json!({"kind":"local_aggregation","lineage":lineage,"daily_source":daily.source});
                rows = annual
                    .records
                    .into_iter()
                    .filter(|r| {
                        (args.start_date.year()..=args.end_date.year()).contains(&r.bar.date.year())
                    })
                    .map(|r| json!(r))
                    .collect();
                versions.push(daily);
            } else {
                rows = version
                    .records
                    .iter()
                    .filter(|bar| bar.date >= args.start_date && bar.date <= args.end_date)
                    .map(|bar| json!(bar))
                    .collect();
            }
            versions.push(version);
        }
        self.page(
            Page {
                token: String::new(),
                key,
                rows,
                metadata,
                versions,
                offset: 0,
            },
            size,
        )
    }
    pub fn versions(
        &self,
        mut args: VersionQuery,
        now: DateTime<Utc>,
    ) -> Result<Value, QueryError> {
        let (symbol, period, size) =
            scope(&args.exchange, &args.code, &args.period, args.page_size)?;
        let cursor = args.cursor.take();
        let key = json!(["versions", args.exchange, args.code, args.period, size]);
        if let Some(cursor) = cursor {
            return self.resume(&cursor, &key, size);
        }
        let mut versions = self
            .store
            .list_verified_versions(&symbol, period, Adjustment::Forward)
            .map_err(unavailable)?;
        versions.sort_by(|a, b| {
            b.published_at
                .cmp(&a.published_at)
                .then_with(|| a.version_id.cmp(&b.version_id))
        });
        let rows = versions.iter().map(|v| json!({"version_id":v.version_id,"published_at":v.published_at,"source":v.source,"record_count":v.records.len(),"yearly_derivation":v.yearly_derivation})).collect();
        self.page(Page {token:String::new(),key,rows,metadata:json!({"symbol":symbol,"period":args.period,"adjustment":"qfq","query_time":now.with_timezone(&FixedOffset::east_opt(28800).unwrap())}),versions,offset:0},size)
    }
    fn resume(&self, token: &str, key: &Value, size: usize) -> Result<Value, QueryError> {
        let page = self
            .pages
            .borrow()
            .iter()
            .find(|p| p.token == token && &p.key == key)
            .cloned()
            .ok_or(QueryError::Cursor)?;
        for v in &page.versions {
            let current = self
                .store
                .get_verified_version(&v.symbol, v.period, v.adjustment, Some(&v.version_id))
                .map_err(unavailable)?;
            if current.as_ref() != Some(v) {
                return Err(QueryError::Unavailable(
                    "fixed version changed; start a new query".into(),
                ));
            }
        }
        self.page(page, size)
    }
    fn page(&self, mut page: Page, size: usize) -> Result<Value, QueryError> {
        let end = (page.offset + size).min(page.rows.len());
        let rows = page.rows[page.offset..end].to_vec();
        let mut response = page.metadata.clone();
        response["records"] = json!(rows);
        response["next_cursor"] = Value::Null;
        response["page_complete"] = json!(end == page.rows.len());
        if end < page.rows.len() {
            let mut sequence = self.sequence.borrow_mut();
            *sequence += 1;
            page.token = format!("{}-{}", self.nonce, sequence);
            page.offset = end;
            response["next_cursor"] = json!(page.token);
            let mut pages = self.pages.borrow_mut();
            if pages.len() >= 64 {
                pages.pop_front();
            }
            pages.push_back(page);
        }
        Ok(response)
    }
}
fn scope(
    exchange: &str,
    code: &str,
    period: &str,
    size: Option<usize>,
) -> Result<(String, Period, usize), QueryError> {
    if !matches!(exchange, "sh" | "sz")
        || code.len() != 6
        || !code.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(QueryError::Invalid(
            "explicit sh/sz exchange and six digit code required",
        ));
    }
    let period = match period {
        "day" => Period::Day,
        "year" => Period::Year,
        _ => return Err(QueryError::Invalid("unsupported period")),
    };
    let size = size.unwrap_or(100);
    if !(1..=1000).contains(&size) {
        return Err(QueryError::Invalid("page_size must be 1..1000"));
    }
    Ok((format!("{exchange}.{code}"), period, size))
}
fn shanghai_date(time: DateTime<Utc>) -> NaiveDate {
    time.with_timezone(&FixedOffset::east_opt(28800).unwrap())
        .date_naive()
}
fn unavailable(error: impl std::fmt::Display) -> QueryError {
    QueryError::Unavailable(error.to_string())
}
