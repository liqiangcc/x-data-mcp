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
    /// Verified sessions shared across clones for the process lifetime:
    /// `open_bound` otherwise re-verifies ~hundreds of MB of artifact bytes
    /// (and re-inspects them again inside the source construction) for
    /// every task bound to the same generation. Verification errors are
    /// never cached — a corrupt or unverified generation retries the full
    /// check on the next open and still fails closed. The per-open pin is
    /// unchanged: each task holds a fresh pin, so retire fencing and pin
    /// sweep semantics are exactly as before. Relaxation: an in-place
    /// artifact rewrite between two opens on the same process is detected
    /// at the next process start rather than the next open — the store
    /// never claimed protection against concurrent byte replacement, and
    /// pins only fence retire.
    sessions: std::sync::Arc<
        std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<BoundSession>>>,
    >,
}
/// One byte-verified generation resolution reused by every bound open.
struct BoundSession {
    manifest_sha256: String,
    dir: PathBuf,
    source: TdxMarketSource,
}
impl GenerationSource {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            store: generation::Store::new(root),
            sessions: Default::default(),
        }
    }
    pub fn from_env() -> Option<Self> {
        std::env::var_os(GENERATION_ROOT_ENV).map(Self::new)
    }
    pub fn store(&self) -> &generation::Store {
        &self.store
    }
    /// The generation id currently usable as a source: an active pointer
    /// whose generation passed byte verification plus a
    /// VERIFIED_COMPATIBLE decision bound to it. Anything less yields
    /// `None` — recorded into frozen inputs as `null` and refused at
    /// execution, never silently substituted. The pointer itself is
    /// re-read on every call, so activating a different generation still
    /// pays full verification; the byte-level verdict shares the
    /// process-scoped session cache with `open_bound` — an in-place
    /// artifact rewrite is detected at the next process start, exactly
    /// as for bound opens.
    pub fn active_verified_id(&self) -> Option<String> {
        let pointer = self.store.active().ok()??;
        self.session(&pointer.generation_id)
            .ok()
            .map(|_| pointer.generation_id)
    }
    /// The active verified generation's id plus a read-only handle on its
    /// official source, WITHOUT pinning it. For plan-time facts only (the
    /// handle is dropped before the call returns to the caller's scope of
    /// interest); execution always goes through `open_bound`, which pins.
    pub fn active_source(&self) -> Option<(String, TdxMarketSource)> {
        let pointer = self.store.active().ok()??;
        let session = self.session(&pointer.generation_id).ok()?;
        Some((pointer.generation_id, session.source.clone()))
    }
    /// Open a session pinned to `generation_id`. Fails closed on a
    /// missing, corrupt, unverified or evidence-only generation.
    pub fn open_bound(
        &self,
        generation_id: &str,
        holder: &str,
    ) -> Result<BoundSource, SourceError> {
        let session = self.session(generation_id)?;
        let pin = self
            .store
            .pin(generation_id, holder)
            .map_err(|e| SourceError::Invocation(format!("generation pin failed: {e}")))?;
        Ok(BoundSource {
            generation_id: generation_id.into(),
            manifest_sha256: session.manifest_sha256.clone(),
            dir: session.dir.clone(),
            source: session.source.clone(),
            _pin: pin,
        })
    }
    /// The process-scoped verified session for `generation_id`: the first
    /// open byte-verifies and inspects; later opens on the same generation
    /// reuse the verdict. Misses and failures are never cached.
    fn session(&self, generation_id: &str) -> Result<std::sync::Arc<BoundSession>, SourceError> {
        if let Some(session) = self.sessions.lock().unwrap().get(generation_id) {
            return Ok(session.clone());
        }
        let installed = self
            .store
            .usable(generation_id, chrono::Utc::now())
            .map_err(|e| SourceError::NotVerified(format!("bound generation unusable: {e}")))?;
        let source = TdxMarketSource::from_installed(&installed)?;
        let session = std::sync::Arc::new(BoundSession {
            manifest_sha256: installed.manifest_sha256,
            dir: installed.dir,
            source,
        });
        self.sessions
            .lock()
            .unwrap()
            .insert(generation_id.into(), session.clone());
        Ok(session)
    }
    /// Reconcile pins left by crashed sessions. `task:<id>` holders are
    /// kept while the task is nonterminal; every other holder is kept —
    /// unknown references fail closed, never dropped.
    pub fn sweep(&self, repository: &crate::sqlite_tasks::SqliteTasks) {
        let Ok(ids) = self.store.installed_ids() else {
            return;
        };
        for generation_id in &ids {
            let _ = self.store.sweep_pins(generation_id, |holder| {
                let Some(task_id) = holder.strip_prefix("task:") else {
                    return true;
                };
                match repository.read_task(task_id) {
                    Ok(Some(snapshot)) => !snapshot.task.status.terminal(),
                    Ok(None) | Err(_) => false,
                }
            });
        }
        // XDM-82: forget the byte view of every generation the store no
        // longer lists. A cached session owns its source's process-scoped
        // archive indexes, whose open file handles would otherwise keep a
        // retired generation's unlinked bytes allocated until a restart.
        // Live tasks hold their own session clone, so this only drops the
        // idle cache entry; a later open of a retired id still fails closed.
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.retain(|generation_id, _| keeps_installed(generation_id, &ids));
        }
    }
}

/// The eviction rule behind `sweep`: a cached session is kept only while the
/// generation store still lists its generation. Split out so it is testable
/// without a generation store.
fn keeps_installed(generation_id: &String, installed: &[String]) -> bool {
    installed.contains(generation_id)
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
        let mut work =
            crate::stage_trace::timed("svc.bound.resolve_inner", || self.inner.resolve(intent))?;
        let binding = match crate::stage_trace::timed("svc.bound.active_verified_id", || {
            self.source.active_verified_id()
        }) {
            Some(id) => Value::String(id),
            None => Value::Null,
        };
        for item in &mut work.items {
            // A contract that resolves against a specific generation (the M3
            // listed-ever scope) freezes its own binding; if the active
            // generation moved between resolution and this stamp, accepting
            // the task would bind a list the reader cannot reproduce.
            if let Some(existing) = item.frozen_inputs.get(SOURCE_GENERATION_KEY)
                && existing != &binding
            {
                return Err(Error::InvalidCommand(format!(
                    "contract resolved against generation {existing} but the active verified \
                     generation is {binding}"
                )));
            }
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

#[cfg(test)]
mod sweep_tests {
    use super::keeps_installed;

    /// XDM-82: the session cache (and with it the source's open archive
    /// handles) must not outlive the generation it belongs to.
    #[test]
    fn retired_generations_leave_the_session_cache() {
        let mut sessions = std::collections::HashMap::new();
        for id in ["gen-1", "gen-2", "gen-retired"] {
            sessions.insert(id.to_string(), id.len());
        }
        let installed = vec!["gen-1".to_string(), "gen-2".to_string()];
        sessions.retain(|generation_id, _| keeps_installed(generation_id, &installed));
        let mut kept: Vec<String> = sessions.keys().cloned().collect();
        kept.sort();
        assert_eq!(kept, vec!["gen-1".to_string(), "gen-2".to_string()]);
    }
}
