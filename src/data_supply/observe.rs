//! Consistent task observations and bounded, authorization-bound snapshots.
use super::{
    Access,
    query::{MAX_BYTES, invalid, page_size},
    wire::*,
};
use crate::{market_storage::Error, task_control::TaskSnapshot};
use chrono::{DateTime, Utc};
use std::{cell::RefCell, collections::VecDeque};
#[derive(Clone)]
pub struct TaskFacts {
    pub request_id: String,
    pub snapshot: TaskSnapshot,
    pub publications: Vec<VersionRef>,
}
pub trait ObservationRepository {
    fn tasks(&self, access: &Access) -> Result<Vec<TaskFacts>, Error>;
    fn task(&self, id: &str, access: &Access) -> Result<TaskFacts, Error>;
    fn availability(
        &self,
        plan: &super::query::QueryPlan,
        access: &Access,
        run: &str,
    ) -> Result<Vec<AvailabilityItem>, Error>;
    fn verify_versions(&self, refs: &[VersionRef]) -> Result<(), Error>;
}
pub fn entity(symbol: &str) -> Option<Security> {
    let (ex, code) = symbol.split_once('.')?;
    Some(Security {
        exchange: match ex {
            "sh" => Exchange::Sh,
            "sz" => Exchange::Sz,
            _ => return None,
        },
        code: code.into(),
    })
}
pub fn authorized(s: &TaskSnapshot, a: &Access) -> bool {
    let datasets = match s.task.key.dataset.as_str() {
        "market.daily" => vec![Dataset::Daily],
        "market.yearly" => vec![Dataset::Yearly],
        "securities.catalog" => vec![Dataset::Catalog],
        "legacy_market_compound" => s
            .items
            .iter()
            .map(|i| {
                if i.spec.target["period"] == "day" {
                    Dataset::Daily
                } else {
                    Dataset::Yearly
                }
            })
            .collect(),
        _ => return false,
    };
    datasets.into_iter().all(|d| a.allows(d, None))
        && s.items.iter().all(|i| {
            i.spec.target["symbol"].as_str().is_none_or(|s| {
                entity(s).is_some_and(|e| {
                    a.securities
                        .as_ref()
                        .is_none_or(|allowed| allowed.contains(&e))
                })
            })
        })
}
pub fn summary(f: &TaskFacts, run: &str) -> TaskSummary {
    let t = &f.snapshot.task;
    TaskSummary {
        task_id: t.task_id.clone(),
        request_id: f.request_id.clone(),
        dataset: t.key.dataset.clone(),
        operation: t.key.operation.clone(),
        contract_version: t.key.contract_version.clone(),
        status: serde_json::from_value(
            serde_json::to_value(f.snapshot.visible_status(run)).unwrap(),
        )
        .unwrap(),
        state_revision: t.state_revision,
        created_at: t.created_at,
        last_progress_at: t.last_progress_at,
    }
}
#[derive(Clone)]
enum Content {
    Tasks(Vec<TaskSummary>),
    Progress(Box<ProgressPage>),
    Availability(Vec<AvailabilityItem>),
}
#[derive(Clone)]
struct Saved {
    token: String,
    key: String,
    access: Access,
    time: DateTime<Utc>,
    id: String,
    offset: usize,
    content: Content,
}
pub struct Observer<R> {
    pub repository: R,
    pub run_id: String,
    cache: RefCell<VecDeque<Saved>>,
    sequence: RefCell<u64>,
    instance: String,
}
impl<R: ObservationRepository> Observer<R> {
    pub fn new(repository: R, run_id: String) -> Self {
        Self {
            repository,
            run_id,
            cache: RefCell::new(VecDeque::new()),
            sequence: RefCell::new(0),
            instance: format!(
                "obs-{}-{}",
                std::process::id(),
                Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ),
        }
    }
    fn id(&self) -> String {
        let mut n = self.sequence.borrow_mut();
        *n += 1;
        format!("{}-{n}", self.instance)
    }
    fn start(&self, key: String, access: &Access, content: Content) -> Saved {
        Saved {
            token: String::new(),
            key,
            access: access.clone(),
            time: Utc::now(),
            id: self.id(),
            offset: 0,
            content,
        }
    }
    fn resume(&self, token: &str, key: &str, access: &Access) -> Result<Saved, Error> {
        let s = self
            .cache
            .borrow()
            .iter()
            .find(|s| s.token == token)
            .cloned()
            .ok_or_else(|| invalid("invalid/expired cursor"))?;
        if s.key != key || &s.access != access {
            return Err(invalid("cursor conditions/authorization changed"));
        }
        Ok(s)
    }
    fn save(&self, mut s: Saved) -> String {
        let token = self.id();
        s.token = token.clone();
        let mut c = self.cache.borrow_mut();
        if c.len() == 32 {
            c.pop_front();
        }
        c.push_back(s);
        token
    }
    pub fn tasks(&self, mut args: TasksQuery, access: &Access) -> Result<TaskListPage, Error> {
        page_size(args.page_size)?;
        if args
            .created_from
            .zip(args.created_to)
            .is_some_and(|(a, b)| a > b)
        {
            return Err(invalid("inverted task time range"));
        }
        if let Some(d) = args.dataset {
            access.require(d, None)?;
        }
        let cursor = args.cursor.take();
        let key = format!("tasks:{}", serde_json::to_string(&args).unwrap());
        let mut s = if let Some(c) = cursor {
            self.resume(&c, &key, access)?
        } else {
            let mut items = self
                .repository
                .tasks(access)?
                .iter()
                .map(|f| summary(f, &self.run_id))
                .filter(|s| {
                    args.dataset
                        .is_none_or(|d| serde_json::to_value(d).unwrap() == s.dataset)
                        && args.status.as_ref().is_none_or(|v| v.contains(&s.status))
                        && args.created_from.is_none_or(|t| s.created_at >= t)
                        && args.created_to.is_none_or(|t| s.created_at <= t)
                })
                .collect::<Vec<_>>();
            items.sort_by(|a, b| {
                b.created_at
                    .cmp(&a.created_at)
                    .then(a.task_id.cmp(&b.task_id))
            });
            budget(&items)?;
            self.start(key, access, Content::Tasks(items))
        };
        let Content::Tasks(items) = &s.content else {
            return Err(invalid("cursor category"));
        };
        let end = (s.offset + args.page_size).min(items.len());
        let output = items[s.offset..end].to_vec();
        let more = end < items.len();
        s.offset = end;
        let mut p = TaskListPage {
            snapshot_id: s.id.clone(),
            snapshot_time: s.time,
            items: output,
            next_cursor: None,
            page_complete: !more,
        };
        if more {
            p.next_cursor = Some(self.save(s));
        }
        Ok(p)
    }
    pub fn progress(
        &self,
        mut args: ProgressQuery,
        access: &Access,
    ) -> Result<ProgressPage, Error> {
        page_size(args.page_size)?;
        let cursor = args.cursor.take();
        let key = format!("progress:{}", serde_json::to_string(&args).unwrap());
        let mut s = if let Some(c) = cursor {
            self.resume(&c, &key, access)?
        } else {
            let facts = self.repository.task(&args.task_id, access)?;
            let mut unfinished = vec![];
            let mut finished = 0;
            let mut succeeded = 0;
            for i in &facts.snapshot.items {
                if let Some(o) = &i.outcome {
                    finished += 1;
                    if o.success {
                        succeeded += 1;
                    }
                } else {
                    unfinished.push(ProgressItem {
                        item_id: i.spec.item_id.clone(),
                        entity: i.spec.target["symbol"].as_str().and_then(entity),
                        period: i.spec.target["period"].as_str().map(str::to_owned),
                        progress_unit: i.spec.progress_unit.clone(),
                        stage: format!("{:?}", i.stage).to_lowercase(),
                        success: None,
                        error: None,
                    });
                }
            }
            unfinished.sort_by(|a, b| a.item_id.cmp(&b.item_id));
            budget(&unfinished)?;
            let p = ProgressPage {
                snapshot_id: String::new(),
                snapshot_time: Utc::now(),
                task: summary(&facts, &self.run_id),
                total: facts.snapshot.items.len(),
                finished,
                succeeded,
                failed: finished - succeeded,
                unfinished,
                next_cursor: None,
                page_complete: false,
                publication_versions: facts.publications,
            };
            self.start(key, access, Content::Progress(Box::new(p)))
        };
        let Content::Progress(p) = &s.content else {
            return Err(invalid("cursor category"));
        };
        self.repository.verify_versions(&p.publication_versions)?;
        let end = (s.offset + args.page_size).min(p.unfinished.len());
        let more = end < p.unfinished.len();
        let mut result = (**p).clone();
        result.unfinished = p.unfinished[s.offset..end].to_vec();
        result.snapshot_id = s.id.clone();
        result.snapshot_time = s.time;
        result.page_complete = !more;
        s.offset = end;
        if more {
            result.next_cursor = Some(self.save(s));
        }
        Ok(result)
    }
    pub fn availability(
        &self,
        mut args: Query,
        access: &Access,
    ) -> Result<AvailabilityPage, Error> {
        let cursor = args.cursor.take();
        let plan = super::query::QueryPlan::new(args, access)?;
        let key = format!(
            "availability:{}",
            serde_json::to_string(&plan.args).unwrap()
        );
        let mut s = if let Some(c) = cursor {
            self.resume(&c, &key, access)?
        } else {
            let items = self.repository.availability(&plan, access, &self.run_id)?;
            budget(&items)?;
            self.start(key, access, Content::Availability(items))
        };
        let Content::Availability(items) = &s.content else {
            return Err(invalid("cursor category"));
        };
        self.repository.verify_versions(
            &items
                .iter()
                .filter_map(|i| i.version_ref.clone())
                .collect::<Vec<_>>(),
        )?;
        let end = (s.offset + plan.args.page_size).min(items.len());
        let more = end < items.len();
        let result = items[s.offset..end].to_vec();
        s.offset = end;
        let mut p = AvailabilityPage {
            snapshot_id: s.id.clone(),
            snapshot_time: s.time,
            items: result,
            next_cursor: None,
            page_complete: !more,
        };
        if more {
            p.next_cursor = Some(self.save(s));
        }
        Ok(p)
    }
}
fn budget<T: serde::Serialize>(v: &[T]) -> Result<(), Error> {
    if v.len() > 10000 || serde_json::to_vec(v).unwrap().len() > MAX_BYTES {
        Err(invalid("observation snapshot budget exceeded"))
    } else {
        Ok(())
    }
}
