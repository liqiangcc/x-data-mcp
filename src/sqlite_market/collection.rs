//! Collection-publication repository surface for the official-file source.
use super::*;
use crate::official_market::DailyCollectionRepository;
impl DailyCollectionRepository for SqliteMarket {
    fn collection_receipt(&self, task: &str, item: &str) -> Result<Option<Receipt>, Error> {
        self.read(|c| {
            let raw: Option<String> = c
                .query_row(
                    "SELECT receipt_json FROM publication_batches WHERE task_id=?1 AND item_id=?2",
                    params![task, item],
                    |r| r.get(0),
                )
                .optional()
                .map_err(storage)?;
            raw.map(|raw| serde_json::from_str(&raw).map_err(integrity))
                .transpose()
        })
    }
    fn effective_head(&self, scope: &Scope) -> Result<Option<String>, Error> {
        self.read(|c| head(c, scope))
    }
    fn transition_task(
        &self,
        task: &str,
        revision: u64,
        run: &str,
        change: crate::task_control::Change,
    ) -> Result<crate::task_control::TaskSnapshot, Error> {
        use crate::task_control::TaskRepository;
        Ok(self.tasks.transition(task, revision, run, change)?)
    }
}
