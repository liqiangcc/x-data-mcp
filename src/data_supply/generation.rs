//! Generation-bound source sessions (D4) and frozen-input binding.
//!
//! A task that reads official files freezes `source_generation_id` into
//! every item's `frozen_inputs` at acceptance via the `Bound` contract
//! decorator. At execution the worker opens a `BoundSource`: the id
//! resolves once to the immutable generation directory through
//! `Store::usable` (byte-verified + VERIFIED_COMPATIBLE decision), and a
//! filesystem pin protects the generation from destructive retire for the
//! session's life. A task frozen on generation A keeps reading A while B
//! is acquired, verified and activated — the active pointer is only
//! consulted at acceptance, never re-read mid-task.
use crate::{
    generation::{self, Pin},
    market_data::SourceError,
    official_market::TdxMarketSource,
    task_control::{CommandKey, Contract, Error, FrozenWork, ItemSpec, TaskRepository},
};
use serde_json::Value;
use std::path::PathBuf;

/// Frozen-input key carrying the bound generation identity.
pub const SOURCE_GENERATION_KEY: &str = "source_generation_id";
/// Config variable installing generation-bound source resolution; absent
/// configuration keeps the legacy inspected-env path unchanged.
pub const GENERATION_ROOT_ENV: &str = "X_DATA_MCP_GENERATION_ROOT";

/// Read-only handle over the private generation root the data service
/// consumes. Construction never creates or mutates the store.
#[derive(Clone)]
pub struct GenerationSource {
    store: generation::Store,
}
impl GenerationSource {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            store: generation::Store::new(root),
        }
    }
    pub fn from_env() -> Option<Self> {
        std::env::var_os(GENERATION_ROOT_ENV).map(Self::new)
    }
    pub fn store(&self) -> &generation::Store {
        &self.store
    }
    /// The generation id currently usable as a source: an active pointer
    /// whose generation still passes byte verification plus a
    /// VERIFIED_COMPATIBLE decision bound to it. Anything less yields
    /// `None` — recorded into frozen inputs as `null` and refused at
    /// execution, never silently substituted.
    pub fn active_verified_id(&self) -> Option<String> {
        let pointer = self.store.active().ok()??;
        self.store
            .usable(&pointer.generation_id, chrono::Utc::now())
            .ok()
            .map(|_| pointer.generation_id)
    }
    /// Open a session pinned to `generation_id`. Fails closed on a
    /// missing, corrupt, unverified or evidence-only generation.
    pub fn open_bound(
        &self,
        generation_id: &str,
        holder: &str,
    ) -> Result<BoundSource, SourceError> {
        let installed = self
            .store
            .usable(generation_id, chrono::Utc::now())
            .map_err(|e| SourceError::NotVerified(format!("bound generation unusable: {e}")))?;
        let pin = self
            .store
            .pin(generation_id, holder)
            .map_err(|e| SourceError::Invocation(format!("generation pin failed: {e}")))?;
        let source = TdxMarketSource::inspect(
            installed.dir.clone(),
            &installed.manifest_bytes,
            chrono::Utc::now(),
        )?;
        Ok(BoundSource {
            generation_id: installed.generation_id,
            manifest_sha256: installed.manifest_sha256,
            dir: installed.dir,
            source,
            _pin: pin,
        })
    }
    /// Reconcile pins left by crashed sessions. `task:<id>` holders are
    /// kept while the task is nonterminal; every other holder is kept —
    /// unknown references fail closed, never dropped.
    pub fn sweep(&self, repository: &crate::sqlite_tasks::SqliteTasks) {
        let Ok(ids) = self.store.installed_ids() else {
            return;
        };
        for generation_id in ids {
            let _ = self.store.sweep_pins(&generation_id, |holder| {
                let Some(task_id) = holder.strip_prefix("task:") else {
                    return true;
                };
                match repository.read_task(task_id) {
                    Ok(Some(snapshot)) => !snapshot.task.status.terminal(),
                    Ok(None) | Err(_) => false,
                }
            });
        }
    }
}

/// A live generation-bound session: immutable directory, byte-verified
/// source view, and the pin guarding it. Dropping releases the pin.
pub struct BoundSource {
    pub generation_id: String,
    pub manifest_sha256: String,
    pub dir: PathBuf,
    pub source: TdxMarketSource,
    _pin: Pin,
}
impl BoundSource {
    /// Catalog executor reading base/gbbq members from this exact
    /// generation directory — the same immutable bytes the session pins.
    pub fn catalog_executor(
        &self,
        tasks: &crate::sqlite_tasks::SqliteTasks,
    ) -> crate::catalog_tdx::source::TdxCatalogExecutor {
        crate::catalog_tdx::source::TdxCatalogExecutor::new(
            crate::sqlite_catalog::SqliteCatalog::new(tasks.clone()),
            self.dir.join(generation::MANIFEST_NAME),
            self.dir.clone(),
        )
    }
}

/// Contract decorator stamping `source_generation_id` into every resolved
/// item's frozen inputs — one acceptance-time fact, identical for all
/// items of the task. Wraps only contracts whose executors read official
/// files; other contracts stay untouched.
pub struct Bound {
    pub inner: Box<dyn Contract>,
    pub source: GenerationSource,
}
impl Contract for Bound {
    fn key(&self) -> CommandKey {
        self.inner.key()
    }
    fn normalize(&self, payload: Value) -> Result<Value, Error> {
        self.inner.normalize(payload)
    }
    fn resolve(&self, intent: &Value) -> Result<FrozenWork, Error> {
        let mut work = self.inner.resolve(intent)?;
        let binding = match self.source.active_verified_id() {
            Some(id) => Value::String(id),
            None => Value::Null,
        };
        for item in &mut work.items {
            item.frozen_inputs[SOURCE_GENERATION_KEY] = binding.clone();
        }
        Ok(work)
    }
}

/// The generation binding frozen into an item, if the key is present.
/// `Some(None)` means the task was frozen without a usable active
/// generation — every item keeps the source refusal. Absent binding key
/// on a task in generation mode is likewise refused: an unbound write
/// must never fall back to a mutable shared root.
pub fn binding_of(item: &ItemSpec) -> Option<Option<String>> {
    item.frozen_inputs
        .get(SOURCE_GENERATION_KEY)
        .map(|v| v.as_str().map(str::to_string))
}

/// Why a task's items cannot resolve a single generation binding. Every
/// case is a refusal, never a fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingRefusal {
    /// No items to bind.
    NoItems,
    /// At least one item lacks the `source_generation_id` frozen input.
    Missing,
    /// Items were frozen while no verified active generation existed.
    Null,
    /// Items disagree on the bound generation.
    Mixed,
}
/// Uniform binding resolution over a task's items: `Ok(id)` only when
/// every item froze the same non-null `source_generation_id`. Missing,
/// null and mixed bindings are refusals — the worker keeps the per-item
/// source refusal rather than reading a mutable shared root.
pub fn uniform_binding<'a>(
    items: impl IntoIterator<Item = &'a ItemSpec>,
) -> Result<String, BindingRefusal> {
    let mut bindings = items.into_iter().map(binding_of);
    let Some(first) = bindings.next() else {
        return Err(BindingRefusal::NoItems);
    };
    if bindings.any(|b| b != first) {
        return Err(BindingRefusal::Mixed);
    }
    match first {
        None => Err(BindingRefusal::Missing),
        Some(None) => Err(BindingRefusal::Null),
        Some(Some(id)) => Ok(id),
    }
}
