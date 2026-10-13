use super::*;

pub struct TaskControl<R> {
    pub repository: R,
    pub run_id: String,
    contracts: Vec<Box<dyn Contract>>,
}
impl<R: TaskRepository> TaskControl<R> {
    pub fn new(
        repository: R,
        run_id: String,
        contracts: Vec<Box<dyn Contract>>,
    ) -> Result<Self, Error> {
        if run_id.trim().is_empty() {
            return Err(Error::InvalidCommand("empty run identity".into()));
        }
        let mut keys = BTreeSet::new();
        for c in &contracts {
            if !keys.insert(c.key()) {
                return Err(Error::InvalidCommand(
                    "duplicate contract registration".into(),
                ));
            }
        }
        Ok(Self {
            repository,
            run_id,
            contracts,
        })
    }
    fn contract(&self, key: &CommandKey) -> Result<&dyn Contract, Error> {
        self.contracts
            .iter()
            .find(|c| &c.key() == key)
            .map(|c| c.as_ref())
            .ok_or(Error::UnsupportedContract)
    }
    fn known(&self, r: &RequestRecord) -> Result<(), Error> {
        match &r.outcome {
            RequestOutcome::Task(s) => {
                self.contract(&s.task.key)?;
            }
            RequestOutcome::Terminal(_) => {
                self.contract(&r.command.key)?;
            }
        }
        Ok(())
    }
    pub fn read_request(&self, id: &str) -> Result<Option<RequestRecord>, Error> {
        let r = self.repository.read_request(id)?;
        if let Some(r) = &r {
            self.known(r)?;
        }
        Ok(r)
    }
    pub fn read_task(&self, id: &str) -> Result<TaskSnapshot, Error> {
        let s = self.repository.read_task(id)?.ok_or(Error::TaskNotFound)?;
        self.contract(&s.task.key)?;
        Ok(s)
    }
    pub fn accept(&self, id: &str, key: CommandKey, payload: Value) -> Result<Acceptance, Error> {
        if id.trim().is_empty() {
            return Err(Error::InvalidCommand("empty request_id".into()));
        }
        let existing = crate::stage_trace::timed("svc.accept.read_request", || {
            self.repository.read_request(id)
        })?;
        if existing.as_ref().is_some_and(|r| r.command.key != key) {
            return Err(Error::RequestConflict);
        }
        let contract = self.contract(&key)?;
        let command = Command {
            key,
            intent: crate::stage_trace::timed("svc.accept.normalize", || {
                contract.normalize(payload)
            })?,
        };
        if let Some(record) = existing {
            self.known(&record)?;
            if record.command != command {
                return Err(Error::RequestConflict);
            }
            return Ok(Acceptance {
                record,
                reused: true,
            });
        }
        let work =
            crate::stage_trace::timed("svc.accept.resolve", || contract.resolve(&command.intent))?;
        crate::stage_trace::timed("svc.accept.validate", || work.validate())?;
        let _repo = crate::stage_trace::span("svc.accept.repository");
        self.repository
            .accept(id, &command, &work, &self.run_id, None)
    }
    /// Acceptance for pre-resolved streamed work — the same binding
    /// semantics as `accept` (request replay first, then gates plus a
    /// single-transaction persist) but item specs arrive as an iterator
    /// so a whole-market admission never holds the full frozen-work tree
    /// in memory at once. The intent is still contract-normalized before
    /// binding.
    pub fn accept_streamed(
        &self,
        id: &str,
        command: &Command,
        shared_evidence: Option<Value>,
        scopes: BTreeSet<String>,
        items: &mut dyn Iterator<Item = ItemSpec>,
    ) -> Result<Acceptance, Error> {
        if id.trim().is_empty() {
            return Err(Error::InvalidCommand("empty request_id".into()));
        }
        let normalized = Command {
            key: command.key.clone(),
            intent: crate::stage_trace::timed("svc.accept.normalize", || {
                self.contract(&command.key)?
                    .normalize(command.intent.clone())
            })?,
        };
        let existing = crate::stage_trace::timed("svc.accept.read_request", || {
            self.repository.read_request(id)
        })?;
        if let Some(record) = existing {
            self.known(&record)?;
            if record.command != normalized {
                return Err(Error::RequestConflict);
            }
            return Ok(Acceptance {
                record,
                reused: true,
            });
        }
        let _repo = crate::stage_trace::span("svc.accept.repository");
        self.repository.accept_streamed(
            id,
            &normalized,
            shared_evidence,
            scopes,
            items,
            &self.run_id,
        )
    }
    pub fn cancel(&self, id: &str, revision: u64) -> Result<TaskSnapshot, Error> {
        self.read_task(id)?;
        self.repository
            .transition(id, revision, &self.run_id, Change::RequestCancel)
    }
    pub fn retry(
        &self,
        original: &str,
        id: &str,
        selected: BTreeSet<String>,
    ) -> Result<Acceptance, Error> {
        // Legacy-observable precedence belongs to the retry use case, not its wire Adapter.
        let old = self.read_task(original)?;
        let existing = self.read_request(id)?;
        if existing.as_ref().is_some_and(
            |r| matches!(&r.outcome, RequestOutcome::Task(s) if s.task.task_id == original),
        ) {
            return Err(Error::RetryRequestIdConflict);
        }
        if selected.is_empty() {
            return Err(Error::EmptyRetrySelection);
        }
        let invalid: BTreeSet<_> = selected
            .iter()
            .filter(|id| {
                !old.items.iter().any(|i| {
                    &i.spec.item_id == *id && !i.outcome.as_ref().is_some_and(|o| o.success)
                })
            })
            .cloned()
            .collect();
        if !invalid.is_empty() {
            return Err(Error::RetrySelectionInvalid { items: invalid });
        }
        if id.trim().is_empty() {
            return Err(Error::EmptyRequestId);
        }
        let intent = serde_json::json!({"original_task_id":original,"selected_items":selected});
        if let Some(record) = existing {
            if record.command.key.operation != "retry_task" || record.command.intent != intent {
                return Err(Error::RequestConflict);
            }
            return Ok(Acceptance {
                record,
                reused: true,
            });
        }
        let guard = RetryGuard {
            original_task_id: original.into(),
            expected_revision: old.task.state_revision,
            selected_items: selected.clone(),
        };
        let items = old
            .items
            .iter()
            .filter(|i| selected.contains(&i.spec.item_id))
            .map(|i| i.spec.clone())
            .collect::<Vec<_>>();
        // Each item owns its explicit resource keys through the frozen work.
        // Explicit resource bindings support any task kind, not security field inference.
        let scopes = items
            .iter()
            .flat_map(|i| i.scope_keys.iter().cloned())
            .collect();
        let work = FrozenWork {
            items,
            scopes,
            shared_evidence: None,
        };
        work.validate()?;
        let command = Command {
            key: CommandKey {
                operation: "retry_task".into(),
                ..old.task.key.clone()
            },
            intent,
        };
        self.repository
            .accept(id, &command, &work, &self.run_id, Some(&guard))
    }
    pub fn run_once(&self, executor: &dyn Executor) -> Result<Option<TaskSnapshot>, Error> {
        let Some(s) = self.repository.candidates(&self.run_id)?.into_iter().next() else {
            return Ok(None);
        };
        self.run_selected(s, executor)
    }
    /// Execute the application-selected snapshot; revision conditions reject a
    /// concurrent claim instead of silently selecting a different task.
    pub(crate) fn run_selected(
        &self,
        mut s: TaskSnapshot,
        executor: &dyn Executor,
    ) -> Result<Option<TaskSnapshot>, Error> {
        self.contract(&s.task.key)?;
        if s.task.status == Status::Pending {
            self.repository
                .transition_light(&mut s, &self.run_id, Change::Claim)?;
        }
        for index in 0..s.items.len() {
            if s.task.status == Status::CancelRequested {
                break;
            }
            if s.items[index].stage != Stage::Pending {
                continue;
            }
            let item_id = s.items[index].spec.item_id.clone();
            match self.repository.transition_light(
                &mut s,
                &self.run_id,
                Change::StartItem(item_id.clone()),
            ) {
                Ok(()) => {}
                Err(Error::RevisionConflict) => {
                    s = self.read_task(&s.task.task_id)?;
                    if s.task.status == Status::CancelRequested {
                        break;
                    }
                    return Err(Error::RevisionConflict);
                }
                Err(e) => return Err(e),
            }
            let outcome = executor.execute(&s.task.key, &s.items[index].spec);
            // A concurrent cancellation may have advanced the revision while the
            // synchronous executor was finishing; transition_light re-reads the
            // actual state once instead of undoing it.
            self.repository.transition_light(
                &mut s,
                &self.run_id,
                Change::FinishItem(item_id, outcome),
            )?;
        }
        self.repository
            .transition_light(&mut s, &self.run_id, Change::FinishExecution)?;
        Ok(Some(s))
    }
}

#[cfg(test)]
mod selected_tests {
    use super::*;
    #[test]
    fn selected_task_race_does_not_substitute_another_pending_task() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::sqlite_tasks::SqliteTasks::new(dir.path().join("selected.sqlite"));
        db.initialize().unwrap();
        let ctl = TaskControl::new(
            db.clone(),
            "run".into(),
            vec![Box::new(legacy::LegacyCollection)],
        )
        .unwrap();
        for (id, symbol) in [("first", "sh.600006"), ("second", "sh.600007")] {
            ctl.accept(
                id,
                legacy::LegacyCollection.key(),
                serde_json::json!({"symbols":[symbol],"periods":["day"]}),
            )
            .unwrap();
        }
        let chosen = db.candidates("run").unwrap().remove(0);
        db.transition(
            &chosen.task.task_id,
            chosen.task.state_revision,
            "run",
            Change::Claim,
        )
        .unwrap();
        struct Count(std::cell::Cell<usize>);
        impl Executor for Count {
            fn execute(&self, _key: &CommandKey, _item: &ItemSpec) -> ItemOutcome {
                self.0.set(self.0.get() + 1);
                ItemOutcome {
                    success: false,
                    error: Some("unexpected execution".into()),
                    evidence: None,
                }
            }
        }
        let executor = Count(std::cell::Cell::new(0));
        assert!(matches!(
            ctl.run_selected(chosen, &executor),
            Err(Error::RevisionConflict)
        ));
        assert_eq!(executor.0.get(), 0);
        let remaining = db.candidates("run").unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].task.state_revision, 0);
        assert_eq!(remaining[0].items[0].stage, Stage::Pending);
    }
    #[test]
    fn stale_selected_executor_is_never_called_after_never_started_interruption() {
        use crate::task_verification::{
            StopVerification, never_started::NeverStarted, runner::StopRunner,
        };
        let dir = tempfile::tempdir().unwrap();
        let db = crate::sqlite_tasks::SqliteTasks::new(dir.path().join("fence.sqlite"));
        db.initialize().unwrap();
        let old = TaskControl::new(
            db.clone(),
            "old".into(),
            vec![Box::new(legacy::LegacyCollection)],
        )
        .unwrap();
        old.accept(
            "target",
            legacy::LegacyCollection.key(),
            serde_json::json!({"symbols":["sh.600006"],"periods":["day"]}),
        )
        .unwrap();
        let stale = db.candidates("old").unwrap().remove(0);
        let current = TaskControl::new(
            db.clone(),
            "new".into(),
            vec![Box::new(
                StopVerification::new(db.clone(), "new".into()).unwrap(),
            )],
        )
        .unwrap();
        let accepted = current
            .accept(
                "check",
                crate::task_verification::command_key(),
                serde_json::json!({"target_task_id":stale.task.task_id}),
            )
            .unwrap();
        let RequestOutcome::Task(check) = accepted.record.outcome else {
            panic!("check required")
        };
        StopRunner {
            repository: db.clone(),
            run_id: "new".into(),
            actor: "test-owner".into(),
        }
        .run(&check.task.task_id, &NeverStarted { repository: db })
        .unwrap();
        struct MustNotExecute;
        impl Executor for MustNotExecute {
            fn execute(&self, _: &CommandKey, _: &ItemSpec) -> ItemOutcome {
                panic!("old executor must not reach source or effects")
            }
        }
        assert!(matches!(
            old.run_selected(stale, &MustNotExecute),
            Err(Error::RevisionConflict)
        ));
    }
}
