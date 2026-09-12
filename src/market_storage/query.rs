//! Internal Stage B fixed-version paging; no tool registration or new wire schema.
use super::*;
use crate::query::{MarketQuery, VersionQuery};
use chrono::{DateTime, Utc};
use std::{cell::RefCell, collections::VecDeque};
#[derive(Debug)]
pub struct MarketPage {
    pub data: Option<ReadPage>,
    pub next_cursor: Option<String>,
    pub query_time: DateTime<Utc>,
}
#[derive(Debug)]
pub struct VersionPage {
    pub versions: Vec<EffectiveVersion>,
    pub next_cursor: Option<String>,
    pub query_time: DateTime<Utc>,
}
#[derive(Clone)]
enum Fixed {
    Market {
        version: String,
        digest: String,
        after: NaiveDate,
    },
    Versions {
        versions: Vec<EffectiveVersion>,
        offset: usize,
    },
}
#[derive(Clone)]
struct Cursor {
    token: String,
    key: Value,
    time: DateTime<Utc>,
    fixed: Fixed,
}
pub struct Pages<R> {
    repository: R,
    cursors: RefCell<VecDeque<Cursor>>,
    sequence: RefCell<u64>,
    nonce: String,
}
impl<R: MarketRepository> Pages<R> {
    pub fn new(repository: R) -> Self {
        Self {
            repository,
            cursors: RefCell::new(VecDeque::new()),
            sequence: RefCell::new(0),
            nonce: format!(
                "{}-{}",
                std::process::id(),
                Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ),
        }
    }
    fn find(&self, token: &str, key: &Value) -> Result<Cursor, Error> {
        self.cursors
            .borrow()
            .iter()
            .find(|c| c.token == token && &c.key == key)
            .cloned()
            .ok_or_else(|| Error::Invalid("invalid/expired/mismatched instance cursor".into()))
    }
    fn save(&self, key: Value, time: DateTime<Utc>, fixed: Fixed) -> String {
        let mut sequence = self.sequence.borrow_mut();
        *sequence += 1;
        let token = format!("{}-{}", self.nonce, sequence);
        let mut cursors = self.cursors.borrow_mut();
        if cursors.len() >= 64 {
            cursors.pop_front();
        }
        cursors.push_back(Cursor {
            token: token.clone(),
            key,
            time,
            fixed,
        });
        token
    }
    pub fn market(&self, args: MarketQuery, now: DateTime<Utc>) -> Result<MarketPage, Error> {
        let (symbol, period, size) =
            crate::query::scope(&args.exchange, &args.code, &args.period, args.page_size)
                .map_err(|e| Error::Invalid(e.to_string()))?;
        let scope = Scope { symbol, period };
        let mut range = Range {
            start: args.start_date,
            end: args.end_date,
            after: None,
            limit: size,
        };
        range.validate()?;
        if args
            .version_id
            .as_ref()
            .is_some_and(|id| id.trim().is_empty())
        {
            return Err(Error::Invalid("empty version_id".into()));
        }
        let key = serde_json::json!([
            "market",
            args.exchange,
            args.code,
            args.period,
            args.start_date,
            args.end_date,
            args.version_id,
            size
        ]);
        let mut version = args.version_id.clone();
        let mut expected = None;
        let mut time = now;
        if let Some(token) = &args.cursor {
            let cursor = self.find(token, &key)?;
            time = cursor.time;
            let Fixed::Market {
                version: fixed,
                digest,
                after,
            } = cursor.fixed
            else {
                return Err(Error::Invalid("wrong cursor kind".into()));
            };
            version = Some(fixed);
            expected = Some(digest);
            range.after = Some(after);
        }
        let data = match self.repository.range(&scope, version.as_deref(), &range) {
            Ok(page) => Some(page),
            Err(Error::NotFound) if version.is_none() => None,
            Err(e) => return Err(e),
        };
        if expected
            .as_ref()
            .is_some_and(|digest| data.as_ref().map(|p| &p.integrity_sha256) != Some(digest))
        {
            return Err(Error::Integrity(
                "fixed version changed between pages".into(),
            ));
        }
        let next_cursor = data.as_ref().and_then(|page| {
            page.next_after.map(|after| {
                self.save(
                    key,
                    time,
                    Fixed::Market {
                        version: page.version.version_id.clone(),
                        digest: page.integrity_sha256.clone(),
                        after,
                    },
                )
            })
        });
        Ok(MarketPage {
            data,
            next_cursor,
            query_time: time,
        })
    }
    pub fn versions(&self, args: VersionQuery, now: DateTime<Utc>) -> Result<VersionPage, Error> {
        let (symbol, period, size) =
            crate::query::scope(&args.exchange, &args.code, &args.period, args.page_size)
                .map_err(|e| Error::Invalid(e.to_string()))?;
        let key = serde_json::json!(["versions", args.exchange, args.code, args.period, size]);
        let (versions, offset, time) = if let Some(token) = args.cursor {
            let cursor = self.find(&token, &key)?;
            let Fixed::Versions { versions, offset } = cursor.fixed else {
                return Err(Error::Invalid("wrong cursor kind".into()));
            };
            let ids = versions
                .iter()
                .map(|v| v.version_id.clone())
                .collect::<Vec<_>>();
            let current = self.repository.fixed_many(&ids)?;
            if current.iter().map(|r| r.version()).ne(versions.iter()) {
                return Err(Error::Integrity("version list member changed".into()));
            }
            (versions, offset, cursor.time)
        } else {
            (self.repository.versions(&Scope { symbol, period })?, 0, now)
        };
        let end = (offset + size).min(versions.len());
        let rows = versions[offset..end].to_vec();
        let next_cursor = if end < versions.len() {
            Some(self.save(
                key,
                time,
                Fixed::Versions {
                    versions,
                    offset: end,
                },
            ))
        } else {
            None
        };
        Ok(VersionPage {
            versions: rows,
            next_cursor,
            query_time: time,
        })
    }
}
