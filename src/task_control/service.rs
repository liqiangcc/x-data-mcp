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
        let existing = self.repository.read_request(id)?;
        if existing.as_ref().is_some_and(|r| r.command.key != key) {
            return Err(Error::RequestConflict);
        }
        let contract = self.contract(&key)?;
        let command = Command {
            key,
            intent: contract.normalize(payload)?,
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
        let work = contract.resolve(&command.intent)?;
        work.validate()?;
        self.repository
            .accept(id, &command, &work, &self.run_id, None)
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
        let work = FrozenWork { items, scopes };
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
        let Some(mut s) = self.repository.candidates(&self.run_id)?.into_iter().next() else {
            return Ok(None);
        };
        self.contract(&s.task.key)?;
        if s.task.status == Status::Pending {
            s = self.repository.transition(
                &s.task.task_id,
                s.task.state_revision,
                &self.run_id,
                Change::Claim,
            )?;
        }
        for item in s.items.clone() {
            s = self.read_task(&s.task.task_id)?;
            if s.task.status == Status::CancelRequested {
                break;
            }
            if item.stage != Stage::Pending {
                continue;
            }
            match self.repository.transition(
                &s.task.task_id,
                s.task.state_revision,
                &self.run_id,
                Change::StartItem(item.spec.item_id.clone()),
            ) {
                Ok(next) => s = next,
                Err(Error::RevisionConflict) => {
                    s = self.read_task(&s.task.task_id)?;
                    if s.task.status == Status::CancelRequested {
                        break;
                    }
                    return Err(Error::RevisionConflict);
                }
                Err(e) => return Err(e),
            }
            let outcome = executor.execute(&s.task.key, &item.spec);
            // A concurrent cancellation may have advanced the revision while the
            // synchronous executor was finishing. Re-read actual state, never undo it.
            s = self.read_task(&s.task.task_id)?;
            s = self.repository.transition(
                &s.task.task_id,
                s.task.state_revision,
                &self.run_id,
                Change::FinishItem(item.spec.item_id, outcome),
            )?;
        }
        s = self.read_task(&s.task.task_id)?;
        self.repository
            .transition(
                &s.task.task_id,
                s.task.state_revision,
                &self.run_id,
                Change::FinishExecution,
            )
            .map(Some)
    }
}
