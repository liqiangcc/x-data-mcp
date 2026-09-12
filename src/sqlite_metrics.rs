//! Reads a fixed verified yearly and hash-bound fixture coverage in one snapshot.
use crate::{
    data_supply::wire::{Dataset, VersionRef},
    market_storage::Error,
    metrics::{
        consecutive::{Completion, Observation},
        service::InputRepository,
        wire::Years,
    },
    sqlite_market::{SqliteMarket, read_record},
};
use chrono::Datelike;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AnalysisCoverage {
    pub origin: Origin,
    pub years: Vec<YearCoverage>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum Origin {
    #[serde(rename = "fixture")]
    Fixture,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct YearCoverage {
    pub year: i32,
    pub status: Coverage,
    pub evidence_ref: String,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    Complete,
    Incomplete,
    Unknown,
}
#[derive(Clone)]
pub struct SqliteMetrics {
    pub market: SqliteMarket,
}
fn integrity(e: impl std::fmt::Display) -> Error {
    Error::Integrity(e.to_string())
}
impl InputRepository for SqliteMetrics {
    fn observations(
        &self,
        reference: &VersionRef,
        range: &Years,
    ) -> Result<Vec<Observation>, Error> {
        self.market.read(|c|{
        let r=read_record(c,&reference.version_id,true,true)?;let v=r.version();
        if reference.dataset!=Dataset::Yearly||v.period!=crate::market_data::Period::Year||reference.entity.as_ref().map(|e|e.symbol()).as_deref()!=Some(v.symbol.as_str())||r.raw()["integrity_sha256"]!=reference.integrity_sha256{return Err(integrity("fixed computation version identity/digest mismatch"));}
        let mut coverage=std::collections::BTreeMap::new();
        if let Some(e)=r.raw().get("analysis_coverage_v1"){
            if !v.source.starts_with("fixture:"){return Err(integrity("fixture coverage cannot certify a production source"));}
            let evidence:AnalysisCoverage=serde_json::from_value(e.clone()).map_err(integrity)?;
            if evidence.years.len()>1000{return Err(Error::Invalid("coverage evidence budget exceeded".into()));}
            for y in evidence.years {if y.evidence_ref.trim().is_empty()||!v.records.iter().any(|r|r.date.year()==y.year)||coverage.insert(y.year,y.status).is_some(){return Err(integrity("coverage evidence year/ref mismatch"));}}
        }
        let mut observations=vec![];
        for b in v.records.iter().filter(|b|b.date.year()>=range.start_year&&b.date.year()<=range.end_year){
            let (first,last,finished):(String,String,bool)=c.query_row("SELECT first_trade_date,last_trade_date,is_finished FROM yearly_bars WHERE version_id=?1 AND year=?2",rusqlite::params![v.version_id,b.date.year()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(integrity)?;
            observations.push(Observation{year:b.date.year(),first:first.parse().map_err(integrity)?,last:last.parse().map_err(integrity)?,close:b.close,unfinished:!finished,completion:match coverage.get(&b.date.year()){Some(Coverage::Complete)=>Completion::Complete,Some(Coverage::Incomplete)=>Completion::Incomplete,_=>Completion::Unknown}});
            if observations.len()>1000{return Err(Error::Invalid("observation budget exceeded".into()));}
        }Ok(observations)
    })
    }
}
