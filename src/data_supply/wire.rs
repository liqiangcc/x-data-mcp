//! One typed contract feeds decoding, schema discovery and handler registration.
use crate::market_data::Period;
use chrono::{DateTime, NaiveDate, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub enum Dataset {
    #[serde(rename = "securities.catalog")]
    Catalog,
    #[serde(rename = "market.daily")]
    Daily,
    #[serde(rename = "market.yearly")]
    Yearly,
}
impl Dataset {
    pub fn period(self) -> Option<Period> {
        match self {
            Self::Daily => Some(Period::Day),
            Self::Yearly => Some(Period::Year),
            Self::Catalog => None,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ContractVersion {
    #[serde(rename = "v1")]
    V1,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Security {
    pub exchange: Exchange,
    pub code: String,
}
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Exchange {
    Sh,
    Sz,
}
impl Security {
    pub fn symbol(&self) -> String {
        format!(
            "{}.{}",
            match self.exchange {
                Exchange::Sh => "sh",
                Exchange::Sz => "sz",
            },
            self.code
        )
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.code.len() == 6 && self.code.bytes().all(|b| b.is_ascii_digit()) {
            Ok(())
        } else {
            Err("six digit security code required".into())
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Selection {
    Single { entity: Security },
    Batch { entities: Vec<Security> },
    Catalog { scope: CatalogScope },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum CatalogScope {
    #[serde(rename = "current_sh_sz_a")]
    CurrentShSzA,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Filter {
    Daily {
        start_date: NaiveDate,
        end_date: NaiveDate,
    },
    Yearly {
        start_year: i32,
        end_year: i32,
    },
    Catalog {
        code: Option<String>,
        name_query: Option<String>,
        industry_code: Option<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VersionBinding {
    pub entity: Security,
    pub version_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VersionChoice {
    LatestPerEntity,
    ExplicitVersionMap { versions: Vec<VersionBinding> },
    CatalogVersion { version_id: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub dataset: Dataset,
    pub contract_version: ContractVersion,
    pub selection: Selection,
    pub filter: Filter,
    pub version: VersionChoice,
    #[serde(default = "default_page")]
    pub page_size: usize,
    pub cursor: Option<String>,
}
pub fn default_page() -> usize {
    100
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VersionRef {
    pub dataset: Dataset,
    pub contract_version: ContractVersion,
    pub entity: Option<Security>,
    pub version_id: String,
    pub integrity_sha256: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ManifestEntry {
    pub entity: Option<Security>,
    pub version_ref: Option<VersionRef>,
    pub status: ManifestStatus,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManifestStatus {
    Present,
    Absent,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Industry {
    Assigned { code: String, name: String },
    Missing { reason: String },
    Conflicted { reason: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Row {
    Market {
        entity: Security,
        version_ref: VersionRef,
        date: NaiveDate,
        open: String,
        high: String,
        low: String,
        close: String,
        volume: Option<String>,
        amount: Option<String>,
    },
    Catalog {
        entity: Security,
        version_ref: VersionRef,
        name: String,
        industry: Industry,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DataPage {
    pub dataset: Dataset,
    pub contract_version: ContractVersion,
    pub snapshot_id: String,
    pub query_time: DateTime<Utc>,
    pub version_manifest: Vec<ManifestEntry>,
    pub records: Vec<Row>,
    pub next_cursor: Option<String>,
    pub page_complete: bool,
    pub selection_complete: bool,
    pub limitations: Vec<String>,
}
