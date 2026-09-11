use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub enum Exchange {
    Shanghai,
    Shenzhen,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SecurityType {
    AShare,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IndustryInfo {
    Assigned { code: String, name: String },
    Missing { reason: String },
    Conflicted { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub struct SecurityKey {
    pub exchange: Exchange,
    pub symbol: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityRecord {
    pub symbol: String,
    pub name: String,
    pub exchange: Exchange,
    pub security_type: SecurityType,
    pub source_security_id: String,
    pub industry: IndustryInfo,
    pub source: String,
    pub acquired_at: DateTime<Utc>,
}

impl SecurityRecord {
    pub fn key(&self) -> SecurityKey {
        SecurityKey {
            exchange: self.exchange.clone(),
            symbol: self.symbol.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompletenessEvidence {
    Complete { expected_count: usize },
    Incomplete { reason: String },
    Unknown { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSnapshot {
    pub source: String,
    pub acquired_at: DateTime<Utc>,
    pub completeness: CompletenessEvidence,
    pub records: Vec<SecurityRecord>,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum CatalogSourceError {
    #[error("catalog source unavailable: {0}")]
    Unavailable(String),
}

pub trait SecurityCatalogSource {
    fn fetch(&self) -> Result<CatalogSnapshot, CatalogSourceError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogVersion {
    pub version_id: String,
    pub published_at: DateTime<Utc>,
    pub source: String,
    pub acquired_at: DateTime<Utc>,
    pub records: Vec<SecurityRecord>,
}

pub trait CatalogRepository {
    fn latest(&self) -> Result<Option<CatalogVersion>, io::Error>;
    fn publish_atomic(&self, version: &CatalogVersion) -> Result<(), io::Error>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefreshStatus {
    Published,
    Rejected,
    NotReady,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectReason {
    SourceFailure { message: String },
    Incomplete { message: String },
    EmptySnapshot,
    InvalidRecord { message: String },
    DuplicateSecurity { key: SecurityKey },
    IdentityConflict { message: String },
    MemberDisappeared { members: Vec<SecurityKey> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshResult {
    pub status: RefreshStatus,
    pub published_version_id: Option<String>,
    pub retained_version_id: Option<String>,
    pub reason: Option<RejectReason>,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("catalog repository I/O failed: {0}")]
    Storage(#[from] io::Error),
}

pub struct CatalogService<S, R> {
    source: S,
    repository: R,
}

impl<S, R> CatalogService<S, R>
where
    S: SecurityCatalogSource,
    R: CatalogRepository,
{
    pub fn new(source: S, repository: R) -> Self {
        Self { source, repository }
    }

    pub fn refresh(&self) -> Result<RefreshResult, CatalogError> {
        let previous = self.repository.latest()?;
        let snapshot = match self.source.fetch() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return Ok(self.rejected(
                    previous.as_ref(),
                    RejectReason::SourceFailure {
                        message: error.to_string(),
                    },
                ));
            }
        };

        if let Err(reason) = validate_snapshot(&snapshot, previous.as_ref()) {
            return Ok(self.rejected(previous.as_ref(), reason));
        }

        let published_at = Utc::now();
        let version = CatalogVersion {
            version_id: format!(
                "catalog-{}",
                published_at.timestamp_nanos_opt().unwrap_or_default()
            ),
            published_at,
            source: snapshot.source,
            acquired_at: snapshot.acquired_at,
            records: snapshot.records,
        };
        self.repository.publish_atomic(&version)?;
        Ok(RefreshResult {
            status: RefreshStatus::Published,
            published_version_id: Some(version.version_id),
            retained_version_id: None,
            reason: None,
        })
    }

    pub fn current(&self) -> Result<Option<CatalogVersion>, CatalogError> {
        Ok(self.repository.latest()?)
    }

    fn rejected(&self, previous: Option<&CatalogVersion>, reason: RejectReason) -> RefreshResult {
        RefreshResult {
            status: if previous.is_some() {
                RefreshStatus::Rejected
            } else {
                RefreshStatus::NotReady
            },
            published_version_id: None,
            retained_version_id: previous.map(|version| version.version_id.clone()),
            reason: Some(reason),
        }
    }
}

fn validate_snapshot(
    snapshot: &CatalogSnapshot,
    previous: Option<&CatalogVersion>,
) -> Result<(), RejectReason> {
    if snapshot.source.trim().is_empty() {
        return Err(RejectReason::InvalidRecord {
            message: "catalog source must not be empty".into(),
        });
    }
    match &snapshot.completeness {
        CompletenessEvidence::Complete { expected_count } if *expected_count > 0 => {
            if *expected_count != snapshot.records.len() {
                return Err(RejectReason::Incomplete {
                    message: format!(
                        "expected {expected_count} records but received {}",
                        snapshot.records.len()
                    ),
                });
            }
        }
        CompletenessEvidence::Complete { .. } => return Err(RejectReason::EmptySnapshot),
        CompletenessEvidence::Incomplete { reason } | CompletenessEvidence::Unknown { reason } => {
            return Err(RejectReason::Incomplete {
                message: reason.clone(),
            });
        }
    }
    if snapshot.records.is_empty() {
        return Err(RejectReason::EmptySnapshot);
    }

    let mut records_by_key = HashMap::<SecurityKey, String>::new();
    let mut source_ids = HashMap::<String, SecurityKey>::new();
    for record in &snapshot.records {
        validate_record(snapshot, record)?;
        let key = record.key();
        if let Some(previous_source_id) = records_by_key.get(&key) {
            if previous_source_id == &record.source_security_id {
                return Err(RejectReason::DuplicateSecurity { key });
            }
            return Err(RejectReason::IdentityConflict {
                message: format!(
                    "security key {:?} maps to both source security ids {} and {}",
                    key, previous_source_id, record.source_security_id
                ),
            });
        }
        records_by_key.insert(key.clone(), record.source_security_id.clone());
        if let Some(previous_key) =
            source_ids.insert(record.source_security_id.clone(), key.clone())
        {
            return Err(RejectReason::IdentityConflict {
                message: format!(
                    "source security id {} maps to both {:?} and {:?}",
                    record.source_security_id, previous_key, key
                ),
            });
        }
    }

    if let Some(previous) = previous {
        let current_keys = records_by_key.keys().cloned().collect::<HashSet<_>>();
        let mut disappeared = previous
            .records
            .iter()
            .map(SecurityRecord::key)
            .filter(|key| !current_keys.contains(key))
            .collect::<Vec<_>>();
        disappeared.sort();
        if !disappeared.is_empty() {
            return Err(RejectReason::MemberDisappeared {
                members: disappeared,
            });
        }
    }
    Ok(())
}

fn validate_record(
    snapshot: &CatalogSnapshot,
    record: &SecurityRecord,
) -> Result<(), RejectReason> {
    for (field, value) in [
        ("symbol", record.symbol.as_str()),
        ("name", record.name.as_str()),
        ("source_security_id", record.source_security_id.as_str()),
        ("source", record.source.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(RejectReason::InvalidRecord {
                message: format!("{field} must not be empty"),
            });
        }
    }
    if record.source != snapshot.source {
        return Err(RejectReason::IdentityConflict {
            message: format!(
                "record source {} differs from snapshot source {}",
                record.source, snapshot.source
            ),
        });
    }
    if !matches!(&record.exchange, Exchange::Shanghai | Exchange::Shenzhen) {
        return Err(RejectReason::InvalidRecord {
            message: "record exchange is outside the Shanghai/Shenzhen scope".into(),
        });
    }
    if !matches!(&record.security_type, SecurityType::AShare) {
        return Err(RejectReason::InvalidRecord {
            message: "record security type is outside the A-share scope".into(),
        });
    }
    match &record.industry {
        IndustryInfo::Assigned { code, name }
            if !code.trim().is_empty() && !name.trim().is_empty() => {}
        IndustryInfo::Assigned { .. } => {
            return Err(RejectReason::InvalidRecord {
                message: "assigned industry requires code and name".into(),
            });
        }
        IndustryInfo::Missing { reason } | IndustryInfo::Conflicted { reason }
            if !reason.trim().is_empty() => {}
        IndustryInfo::Missing { .. } | IndustryInfo::Conflicted { .. } => {
            return Err(RejectReason::InvalidRecord {
                message: "industry missing/conflict requires a reason".into(),
            });
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct JsonCatalogStore {
    root: PathBuf,
}

impl JsonCatalogStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn current(&self) -> Result<Option<CatalogVersion>, io::Error> {
        self.latest()
    }

    fn path(&self) -> PathBuf {
        self.root.join("catalog.effective.json")
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

impl CatalogRepository for JsonCatalogStore {
    fn latest(&self) -> Result<Option<CatalogVersion>, io::Error> {
        let path = self.path();
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(path)?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(io::Error::other)
    }

    fn publish_atomic(&self, version: &CatalogVersion) -> Result<(), io::Error> {
        fs::create_dir_all(&self.root)?;
        let payload = serde_json::to_vec_pretty(version).map_err(io::Error::other)?;
        let suffix = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temporary = self.root.join(format!(
            ".catalog.effective.json.tmp-{}-{suffix}",
            std::process::id()
        ));
        fs::write(&temporary, payload)?;
        fs::rename(temporary, self.path())
    }
}
