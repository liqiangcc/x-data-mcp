//! Acquisition service state machine: remote probes, durable download
//! tasks, #131 consistency verification and conditional activation.
//!
//! Every mutating tool call is bound to a caller `request_id` whose outcome
//! is recorded once and replayed verbatim — a lost response never leaves an
//! unrecoverable side effect. Downloads run as durable acquisition tasks
//! driven by the service worker (`worker_once`); each stage commits before
//! the next begins, so a crash at any point reconciles mechanically at
//! `open`. Verification (D1) auto-issues `CompatibilityEvidence` only when
//! every structural probe passes; any uncertainty records a NOT_VERIFIED or
//! CONFLICT decision instead of a pass. Activation (D5) is an atomic
//! expected-previous pointer swap under the service's single-writer lock.
//! Retire (D2) deletes artifact bytes only — never automatically — behind
//! an explicit confirm token and a live-reference check.
use super::{fetch, store, wire};
use crate::{
    file_consistency::{
        ByteIntegrity, CompatibilityEvidence, CompatibilityWindow, ConsistencyInput,
        ConsistencyStatus, FileFact, MemberBinding,
    },
    generation::{self, Installed},
    snapshot::{FileInput, FileRole},
};
use chrono::Utc;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

/// The frozen acquire tool contract this service implements.
pub const GENERATION_CONTRACT_VERSION: &str = "official-generation-v1";
/// Probe suite version (D1): the structural validators whose combined PASS
/// is the only path to auto-issued compatibility evidence.
pub const PROBE_CONTRACT_VERSION: &str = "official-generation-probe-v1";
/// Hot-set bound (D2): active + previous + undecided candidates. New
/// acquisitions are refused at the bound until a candidate retires.
pub const MAX_HOT_GENERATIONS: usize = 6;
/// Automatic retention (D2): after each committed activation the newest
/// `RETAINED_SUPERSEDED` superseded generations (the one just replaced
/// included) keep their bytes; older superseded generations retire.
const RETAINED_SUPERSEDED: usize = 2;
/// Deterministic `.day` members fully parsed during the day probe; the
/// sample is the sorted-member head, so reruns are stable.
const DAY_SAMPLE: usize = 64;
/// A scoped catalog member without a `.day` file is legitimate
/// (long-suspended securities: 11 on the pinned gen-2 set). Above this
/// bound the day archive is treated as grossly behind the base.
const MAX_MISSING_SCOPED_DAY: usize = 64;
/// A day archive whose freshest sampled bar ends more than this many days
/// before the acquisition date is treated as a stale-generation mix.
const STALE_DAY_TOLERANCE_DAYS: i64 = 14;

const KIND_ACQUIRE: &str = "acquire_generation";
const KIND_VERIFY: &str = "verify_generation";
const KIND_ACTIVATE: &str = "activate_generation";
const KIND_RETIRE: &str = "retire_generation";
const KIND_TASK: &str = "download_install";

const ST_ACCEPTED: &str = "accepted";
const ST_RUNNING: &str = "running";
const ST_SUCCEEDED: &str = "succeeded";
const ST_FAILED: &str = "failed";

const GEN_BYTE_VERIFIED: &str = "byte_verified";
const GEN_VERIFIED: &str = "consistency_verified";
const GEN_NOT_VERIFIED: &str = "not_verified";
const GEN_CONFLICT: &str = "conflict";
const GEN_CORRUPT: &str = "corrupt";
const GEN_ACTIVE: &str = "active";
const GEN_SUPERSEDED: &str = "superseded";
const GEN_EVIDENCE_ONLY: &str = "evidence_only";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("storage: {0}")]
    Storage(#[from] store::Error),
    #[error("generation: {0}")]
    Generation(#[from] generation::Error),
    #[error("fetch: {0}")]
    Fetch(#[from] fetch::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Refused(&'static str),
}
impl Error {
    /// Stable reason code carried in wire replies and outcomes.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Refused(code) => code,
            Self::Storage(_) => "storage_error",
            Self::Generation(_) => "generation_error",
            Self::Fetch(_) => "fetch_error",
            Self::Json(_) => "internal_error",
            Self::Io(_) => "io_error",
        }
    }
}

/// Which fetcher the service uses. `Remote` shells out to the pinned
/// helper for the declared TDX URLs; `Mirror` reads the same artifact
/// names from a local directory so pinned snapshots and isolated
/// acceptance flow through the identical byte-verified pipeline.
#[derive(Clone)]
pub enum Source {
    Remote,
    Mirror(PathBuf),
    /// Injected fetcher (tests/isolated acceptance): the service drives
    /// the identical durable pipeline; only the byte source differs.
    Fetcher(Arc<dyn fetch::Fetcher>),
}
impl Source {
    /// `Remote` resolves the pinned helper at construction time and fails
    /// closed when it cannot be located (#375), so a misconfigured instance
    /// never reaches a state where it accepts tasks it cannot download.
    fn fetcher(&self) -> Result<Arc<dyn fetch::Fetcher>, fetch::Error> {
        Ok(match self {
            Self::Remote => Arc::new(fetch::Helper::new()?),
            Self::Mirror(dir) => Arc::new(fetch::Mirror::new(dir)),
            Self::Fetcher(fetcher) => fetcher.clone(),
        })
    }
}
impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Remote => f.write_str("Remote"),
            Self::Mirror(dir) => f.debug_tuple("Mirror").field(dir).finish(),
            Self::Fetcher(_) => f.write_str("Fetcher"),
        }
    }
}

#[derive(Clone)]
pub struct AcquireService {
    store: generation::Store,
    db: store::Db,
    fetcher: Arc<dyn fetch::Fetcher>,
    mutations: Arc<Mutex<()>>,
}

struct Role {
    role: FileRole,
    key: &'static str,
    url: &'static str,
    file: &'static str,
}
const ROLES: [Role; 3] = [
    Role {
        role: FileRole::BaseIndustry,
        key: "base_industry",
        url: "https://www.tdx.com.cn/products/data/data/dbf/base.zip",
        file: "base.zip",
    },
    Role {
        role: FileRole::Gbbq,
        key: "gbbq",
        url: "https://www.tdx.com.cn/products/data/data/dbf/gbbq.zip",
        file: "gbbq.zip",
    },
    Role {
        role: FileRole::Day,
        key: "day",
        url: "https://data.tdx.com.cn/vipdoc/hsjday.zip",
        file: "hsjday.zip",
    },
];

fn active_change(
    head: Option<&fetch::Head>,
    active_manifest_present: bool,
    active: Option<&FileInput>,
) -> wire::Change {
    let Some(head) = head else {
        return wire::Change::Unknown;
    };
    if !active_manifest_present {
        return wire::Change::Changed;
    }
    let Some(active) = active else {
        return wire::Change::Unknown;
    };
    if let (Some(active_etag), Some(head_etag)) = (&active.etag, &head.etag) {
        return if active_etag == head_etag {
            wire::Change::Unchanged
        } else {
            wire::Change::Changed
        };
    }
    if active.etag.is_some() || head.etag.is_some() {
        return wire::Change::Unknown;
    }
    match (
        &active.last_modified,
        &head.last_modified,
        active.bytes,
        head.content_length,
    ) {
        (Some(active_modified), Some(head_modified), active_bytes, Some(head_bytes))
            if active_modified == head_modified && active_bytes == head_bytes =>
        {
            wire::Change::Unchanged
        }
        (Some(_), Some(_), _, Some(_)) => wire::Change::Changed,
        _ => wire::Change::Unknown,
    }
}

impl AcquireService {
    /// Open (and initialize) the durable store under `root`, then reconcile
    /// every crash-reachable fact: orphaned installs are adopted, stale
    /// running tasks return to `accepted`, and the generations table is
    /// re-aligned with the on-disk active pointer.
    pub fn open(root: impl Into<PathBuf>, source: Source) -> Result<Self, Error> {
        let store = generation::Store::new(root.into());
        std::fs::create_dir_all(store.staging_dir())?;
        std::fs::create_dir_all(store.generations_dir())?;
        std::fs::create_dir_all(store.pins_dir())?;
        let db = store::Db::new(store.acquire_db_path());
        db.open()?;
        // The daemon's single-writer lease requires the store inode to be
        // owner-private; create it that way rather than depending on umask.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(store.acquire_db_path())?.permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(store.acquire_db_path(), perms)?;
        }
        let service = Self {
            store,
            db,
            fetcher: source.fetcher()?,
            mutations: Arc::new(Mutex::new(())),
        };
        service.reconcile()?;
        Ok(service)
    }

    /// The durable acquisition store path — the daemon takes its
    /// single-writer lease on this inode.
    pub fn db_path(&self) -> PathBuf {
        self.store.acquire_db_path()
    }
    /// The generation store, for source-session pinning and read paths.
    pub fn generations(&self) -> &generation::Store {
        &self.store
    }

    fn reconcile(&self) -> Result<(), Error> {
        // A task marked running died mid-stage; its stage/detail row lets
        // the next worker turn resume exactly where bytes last committed.
        for task in self.db.tasks_in(&[ST_RUNNING])? {
            self.db.update_task(&store::Task {
                state: ST_ACCEPTED.into(),
                updated_at: Utc::now(),
                ..task
            })?;
        }
        // Adopt generation directories the DB never recorded (crash
        // between rename and row upsert); corrupt dirs are recorded, never
        // deleted or repaired.
        for id in self.store.installed_ids()? {
            if self.db.generation(&id)?.is_some() {
                continue;
            }
            let (state, sha) = match self.store.verify_installed(&id, Utc::now()) {
                Ok(installed) => (GEN_BYTE_VERIFIED, installed.manifest_sha256),
                Err(e) => (GEN_CORRUPT, format!("unverified: {e}")),
            };
            self.db.upsert_generation(&store::Generation {
                generation_id: id,
                state: state.into(),
                manifest_sha256: sha,
                consistency_status: None,
                consistency_reasons: None,
                decision_sha256: None,
                evidence_json: None,
                acquisition_request_id: None,
                installed_at: Utc::now(),
                updated_at: Utc::now(),
            })?;
        }
        // The on-disk pointer wins over row state after a crash between
        // pointer swap and row update.
        if let Some(pointer) = self.store.active()? {
            for row in self
                .db
                .generations_in(&[GEN_ACTIVE, GEN_SUPERSEDED, GEN_VERIFIED])?
            {
                let expected = if row.generation_id == pointer.generation_id {
                    GEN_ACTIVE
                } else if row.state == GEN_ACTIVE {
                    GEN_SUPERSEDED
                } else {
                    continue;
                };
                if row.state != expected {
                    self.db.upsert_generation(&store::Generation {
                        state: expected.into(),
                        updated_at: Utc::now(),
                        ..row
                    })?;
                }
            }
        }
        // Staging areas of terminal tasks are download residue; remove
        // them only now that the outcome is durably recorded.
        for task in self.db.tasks_in(&[ST_SUCCEEDED, ST_FAILED])? {
            self.store.remove_stage(&task.request_id)?;
        }
        Ok(())
    }

    // ---------- remote observation ----------

    /// Read-only remote probe. Per role, `seen` facts are compared; ETag
    /// is the only reliable validator — without it the result is never
    /// reported `unchanged` (AC-A03). Fresh observations are recorded. The
    /// operational pending baseline is the active generation manifest, so a
    /// changed probe that did not lead to activation remains pending.
    pub fn check_updates(&self) -> Result<wire::UpdateCheck, Error> {
        let active_pointer = self.store.active()?;
        let active_manifest = match &active_pointer {
            Some(pointer) => Some(self.store.installed_manifest(&pointer.generation_id)?),
            None => None,
        };
        let active_files = active_manifest.as_ref().map(|manifest| {
            manifest
                .files
                .iter()
                .map(|file| (file.role, file))
                .collect::<BTreeMap<_, _>>()
        });
        let mut files = vec![];
        let mut changed = vec![];
        let mut pending_roles = vec![];
        let mut unknown = vec![];
        for role in ROLES {
            let previous = self.db.seen(role.key)?;
            let head = self.fetcher.head(role.url).ok();
            let change = match (&previous, &head) {
                (_, None) => wire::Change::Unknown,
                (None, Some(_)) => wire::Change::Unknown,
                (Some(prev), Some(head)) => {
                    if let (Some(a), Some(b)) = (&prev.etag, &head.etag) {
                        if a == b {
                            wire::Change::Unchanged
                        } else {
                            wire::Change::Changed
                        }
                    } else if prev.etag.is_some() || head.etag.is_some() {
                        wire::Change::Unknown
                    } else {
                        match (
                            &prev.last_modified,
                            &head.last_modified,
                            prev.content_length,
                            head.content_length,
                        ) {
                            (Some(pl), Some(hl), Some(pc), Some(hc)) if pl != hl || pc != hc => {
                                wire::Change::Changed
                            }
                            _ => wire::Change::Unknown,
                        }
                    }
                }
            };
            let active_file = active_files
                .as_ref()
                .and_then(|files| files.get(&role.role))
                .copied();
            let active_change =
                active_change(head.as_ref(), active_manifest.is_some(), active_file);
            if change == wire::Change::Changed {
                changed.push(role.key.to_string());
            }
            if active_change == wire::Change::Changed {
                pending_roles.push(role.key.to_string());
            }
            if change == wire::Change::Unknown {
                unknown.push(role.key.to_string());
            }
            if let Some(head) = &head {
                self.db.record_seen(&store::Seen {
                    role: role.key.into(),
                    etag: head.etag.clone(),
                    last_modified: head.last_modified.clone(),
                    content_length: head.content_length,
                    seen_at: Utc::now(),
                })?;
            }
            files.push(wire::RemoteFile {
                role: role.key.into(),
                url: role.url.into(),
                status: head.as_ref().map(|h| h.status).unwrap_or(0),
                etag: head.as_ref().and_then(|h| h.etag.clone()),
                last_modified: head.as_ref().and_then(|h| h.last_modified.clone()),
                content_length: head.as_ref().and_then(|h| h.content_length),
                change,
                active: active_file.map(|file| wire::ActiveFile {
                    etag: file.etag.clone(),
                    last_modified: file.last_modified.clone(),
                    content_length: Some(file.bytes),
                    sha256: file.sha256.clone(),
                }),
                active_change,
            });
        }
        Ok(wire::UpdateCheck {
            checked_at: Utc::now().to_rfc3339(),
            active_generation_id: active_pointer.map(|p| p.generation_id),
            files,
            changed,
            pending_roles,
            unknown,
        })
    }

    // ---------- acquisition task ----------

    /// Accept a generation download. The request row is the idempotency
    /// registry; replaying returns the original acceptance. Capacity is
    /// fail-closed: the unretired hot set has a fixed bound (D2) and a new
    /// candidate is refused, never silently queued beyond it (AC-R04).
    pub fn acquire_generation(
        &self,
        request_id: &str,
    ) -> Result<wire::AcquisitionAcceptance, Error> {
        let (request, reused) = self.db.accept_request(
            request_id,
            KIND_ACQUIRE,
            &serde_json::to_string(&json!({}))?,
            Utc::now(),
        )?;
        if reused {
            let task = self
                .db
                .task_by_request(&request.request_id)?
                .ok_or(Error::Refused("request_binding_lost"))?;
            return Ok(wire::AcquisitionAcceptance {
                request_id: request.request_id,
                task_id: task.task_id,
                state: task.state,
                reused: true,
            });
        }
        let _m = self.mutations.lock().expect("mutation lock poisoned");
        let hot = self
            .db
            .generations()?
            .into_iter()
            .filter(|g| g.state != GEN_EVIDENCE_ONLY)
            .count();
        if hot >= MAX_HOT_GENERATIONS {
            let _ = self.db.record_outcome(
                request_id,
                &json!({"state":"refused","reason_code":"generation_capacity"}),
                Utc::now(),
            );
            return Err(Error::Refused("generation_capacity"));
        }
        let task_id = format!("acq-{}", &crate::hash::sha256(request_id.as_bytes())[..24]);
        let task = store::Task {
            task_id: task_id.clone(),
            request_id: request.request_id.clone(),
            kind: KIND_TASK.into(),
            state: ST_ACCEPTED.into(),
            generation_id: None,
            stage: Some(format!("fetch:{}", ROLES[0].key)),
            bytes_done: 0,
            error: None,
            detail: Some(json!({"files":{}}).to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        self.db.create_task(&task)?;
        // A stale staging area from an earlier attempt belongs to a
        // recorded task outcome; this fresh request starts clean.
        if self.store.stage_dir(request_id).exists() {
            self.store.remove_stage(request_id)?;
        }
        self.store.create_stage(request_id)?;
        Ok(wire::AcquisitionAcceptance {
            request_id: request.request_id,
            task_id,
            state: ST_ACCEPTED.into(),
            reused: false,
        })
    }

    /// Advance the oldest open acquisition task by one stage. Called only
    /// by the service worker loop; stage commits make every crash point
    /// mechanically recoverable (AC-A02).
    pub fn worker_once(&self) -> Result<Option<store::Task>, Error> {
        let _m = self.mutations.lock().expect("mutation lock poisoned");
        let Some(task) = self
            .db
            .tasks_in(&[ST_ACCEPTED, ST_RUNNING])?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        if task.state != ST_RUNNING {
            self.db.update_task(&store::Task {
                state: ST_RUNNING.into(),
                updated_at: Utc::now(),
                ..task.clone()
            })?;
        }
        match self.advance(&task) {
            Ok(next) => Ok(Some(self.db.task(&task.task_id)?.unwrap_or(next))),
            Err(e) => {
                self.db.update_task(&store::Task {
                    state: ST_FAILED.into(),
                    error: Some(e.to_string()),
                    updated_at: Utc::now(),
                    ..task.clone()
                })?;
                let _ = self.db.record_outcome(
                    &task.request_id,
                    &json!({"state":"failed","task_id":task.task_id,"error":e.to_string()}),
                    Utc::now(),
                );
                Ok(Some(self.db.task(&task.task_id)?.unwrap_or(task)))
            }
        }
    }

    fn advance(&self, task: &store::Task) -> Result<store::Task, Error> {
        let mut detail: Value =
            serde_json::from_str(task.detail.as_deref().unwrap_or("{\"files\":{}}"))?;
        let stage = task.stage.as_deref().unwrap_or_default();
        if let Some(key) = stage.strip_prefix("fetch:") {
            let role = ROLES
                .iter()
                .find(|r| r.key == key)
                .ok_or(Error::Refused("unknown_stage"))?;
            self.fetch_role(task, role, &mut detail)?;
            let next = if role.key == "day" {
                "manifest".to_string()
            } else {
                let index = ROLES.iter().position(|r| r.key == role.key).unwrap();
                format!("fetch:{}", ROLES[index + 1].key)
            };
            self.db.update_task(&store::Task {
                state: ST_RUNNING.into(),
                generation_id: None,
                stage: Some(next),
                bytes_done: detail_bytes(&detail),
                error: None,
                detail: Some(detail.to_string()),
                updated_at: Utc::now(),
                ..task.clone()
            })?;
            return Ok(self.db.task(&task.task_id)?.unwrap_or_else(|| task.clone()));
        }
        match stage {
            "manifest" => {
                self.write_manifest(task, &detail)?;
                self.db.update_task(&store::Task {
                    state: ST_RUNNING.into(),
                    generation_id: None,
                    stage: Some("install".into()),
                    bytes_done: detail_bytes(&detail),
                    error: None,
                    detail: Some(detail.to_string()),
                    updated_at: Utc::now(),
                    ..task.clone()
                })?;
            }
            "install" => {
                let installed = self.store.install_staged(&task.request_id, Utc::now())?;
                self.db.upsert_generation(&store::Generation {
                    generation_id: installed.generation_id.clone(),
                    state: GEN_BYTE_VERIFIED.into(),
                    manifest_sha256: installed.manifest_sha256.clone(),
                    consistency_status: None,
                    consistency_reasons: None,
                    decision_sha256: None,
                    evidence_json: None,
                    acquisition_request_id: Some(task.request_id.clone()),
                    installed_at: Utc::now(),
                    updated_at: Utc::now(),
                })?;
                self.db.update_task(&store::Task {
                    state: ST_SUCCEEDED.into(),
                    generation_id: Some(installed.generation_id.clone()),
                    stage: Some("done".into()),
                    bytes_done: detail_bytes(&detail),
                    error: None,
                    detail: Some(detail.to_string()),
                    updated_at: Utc::now(),
                    ..task.clone()
                })?;
                self.db.record_outcome(
                    &task.request_id,
                    &json!({"state":"succeeded","task_id":task.task_id,"generation_id":installed.generation_id,"manifest_sha256":installed.manifest_sha256}),
                    Utc::now(),
                )?;
                self.store.remove_stage(&task.request_id)?;
            }
            _ => return Err(Error::Refused("unknown_stage")),
        }
        Ok(self.db.task(&task.task_id)?.unwrap_or_else(|| task.clone()))
    }

    /// Fetch one role artifact into the staging area. A file whose staged
    /// bytes still match its recorded pin is kept (resume); anything else
    /// is fetched fresh through the helper's temp+rename+fsync path.
    fn fetch_role(&self, task: &store::Task, role: &Role, detail: &mut Value) -> Result<(), Error> {
        let stage = self.store.stage_dir(&task.request_id);
        let dest = stage.join(role.file);
        let recorded = detail["files"][role.key].clone();
        if let (Some(sha), Some(bytes)) = (recorded["sha256"].as_str(), recorded["bytes"].as_u64())
            && dest.is_file()
            && std::fs::metadata(&dest)?.len() == bytes
            // Streamed, not `fs::read`: the day archive is ~550 MB, and the
            // resume check must not be the peak of the switch (XDM-81).
            && crate::hash::digest_file(&dest)?.1 == sha
        {
            return Ok(());
        }
        let started = Utc::now();
        let fetched = self.fetcher.fetch(role.url, &dest)?;
        let completed = Utc::now();
        detail["files"][role.key] = serde_json::to_value(FileInput {
            role: role.role,
            relative_path: role.file.into(),
            source_url: role.url.into(),
            acquisition_started_at: started,
            acquisition_completed_at: completed,
            last_modified: fetched.head.last_modified.clone(),
            etag: fetched.head.etag.clone(),
            bytes: fetched.bytes,
            sha256: fetched.sha256,
        })?;
        Ok(())
    }

    fn write_manifest(&self, task: &store::Task, detail: &Value) -> Result<(), Error> {
        let mut files = vec![];
        for role in ROLES {
            let value = &detail["files"][role.key];
            if value.is_null() {
                return Err(Error::Refused("manifest_incomplete"));
            }
            files.push(serde_json::from_value::<FileInput>(value.clone())?);
        }
        let manifest = serde_json::to_vec_pretty(&crate::snapshot::SnapshotManifest {
            schema: "x-data-mcp.official-file-input.v1".into(),
            files,
        })?;
        let path = self
            .store
            .stage_dir(&task.request_id)
            .join(generation::MANIFEST_NAME);
        let mut file = std::fs::File::create(&path)?;
        std::io::Write::write_all(&mut file, &manifest)?;
        file.sync_all()?;
        Ok(())
    }

    // ---------- task/request views ----------

    pub fn task(&self, task_id: &str) -> Result<wire::AcquisitionTaskView, Error> {
        let task = self
            .db
            .task(task_id)?
            .ok_or(Error::Refused("task_not_found"))?;
        Ok(task_view(&task))
    }
    pub fn request(&self, request_id: &str) -> Result<wire::AcquisitionRequestView, Error> {
        let request = self.db.request(request_id)?;
        let task = self.db.task_by_request(request_id)?;
        let outcome: Option<Value> = self.db.outcome(request_id)?;
        Ok(wire::AcquisitionRequestView {
            request_id: request_id.into(),
            bound: request.is_some() || task.is_some() || outcome.is_some(),
            task: task.as_ref().map(task_view),
            terminal_result: outcome,
        })
    }

    // ---------- consistency verification (D1) ----------

    /// Run the probe suite and the #131 contract validator over an
    /// installed generation, then record the once-written decision. The
    /// request outcome is durable; replay returns the recorded reply.
    pub fn verify_generation(
        &self,
        request_id: &str,
        generation_id: &str,
    ) -> Result<wire::VerifyReply, Error> {
        let payload = serde_json::to_string(&json!({"generation_id":generation_id}))?;
        let (_, reused) = self
            .db
            .accept_request(request_id, KIND_VERIFY, &payload, Utc::now())?;
        if reused && let Some(reply) = self.db.outcome::<wire::VerifyReply>(request_id)? {
            return Ok(wire::VerifyReply {
                reused: true,
                ..reply
            });
        }
        let _m = self.mutations.lock().expect("mutation lock poisoned");
        let reply = self.verify_now(request_id, generation_id)?;
        self.db.record_outcome(request_id, &reply, Utc::now())?;
        Ok(reply)
    }

    fn verify_now(
        &self,
        request_id: &str,
        generation_id: &str,
    ) -> Result<wire::VerifyReply, Error> {
        // A once-written decision is a durable fact: a second verify
        // request replays it rather than re-deciding.
        if let Some(existing) = self.store.decision_artifact(generation_id)? {
            let state = self
                .db
                .generation(generation_id)?
                .map(|g| g.state)
                .unwrap_or_else(|| GEN_BYTE_VERIFIED.into());
            return Ok(wire::VerifyReply {
                request_id: request_id.into(),
                generation_id: generation_id.into(),
                state,
                consistency_status: Some(consistency_status_name(&existing.status).into()),
                consistency_reasons: existing
                    .reasons
                    .iter()
                    .map(|r| serde_json::to_string(r).unwrap_or_default())
                    .collect(),
                decision_sha256: self
                    .store
                    .decision(generation_id)?
                    .map(|d| crate::hash::sha256(&d)),
                reason_code: None,
                reused: false,
            });
        }
        let installed = match self.store.verify_installed(generation_id, Utc::now()) {
            Ok(installed) => installed,
            Err(generation::Error::NotInstalled(_)) => {
                return Ok(refused_verify(
                    request_id,
                    generation_id,
                    "generation_not_installed",
                ));
            }
            Err(_) => {
                return Ok(refused_verify(
                    request_id,
                    generation_id,
                    "generation_bytes_unverified",
                ));
            }
        };
        let probe = probes(&installed);
        let facts = file_facts(&installed);
        let evidence = if probe.failures.is_empty() {
            Some(build_evidence(&installed, &facts))
        } else {
            None
        };
        let evidence_json = evidence.as_ref().map(serde_json::to_vec).transpose()?;
        let evidence_sha256 = match (&installed.dir, &evidence_json) {
            (_, Some(bytes)) => Some(self.store.ensure_artifact(
                generation_id,
                generation::EVIDENCE_NAME,
                bytes,
            )?),
            _ => None,
        };
        let decision = crate::file_consistency::validate(ConsistencyInput {
            files: facts,
            compatibility: evidence,
        });
        let artifact = generation::DecisionArtifact {
            schema: generation::DecisionArtifact::SCHEMA.into(),
            generation_id: generation_id.into(),
            manifest_sha256: installed.manifest_sha256.clone(),
            issuer: "x-data-acquire.auto-probe".into(),
            validator_contract_version: format!(
                "{}+{PROBE_CONTRACT_VERSION}",
                crate::file_consistency::CONTRACT_VERSION
            ),
            evaluated_at: Utc::now(),
            status: decision.status,
            reasons: decision.reasons.clone(),
            evidence_sha256: evidence_sha256.clone(),
            probes: probe.facts,
        };
        let decision_bytes = serde_json::to_vec_pretty(&artifact)?;
        let decision_sha256 = self.store.write_decision(generation_id, &decision_bytes)?;
        let state = match decision.status {
            ConsistencyStatus::VerifiedCompatible => GEN_VERIFIED,
            ConsistencyStatus::NotVerified => GEN_NOT_VERIFIED,
            ConsistencyStatus::Conflict => GEN_CONFLICT,
        };
        self.db.upsert_generation(&store::Generation {
            generation_id: generation_id.into(),
            state: state.into(),
            manifest_sha256: installed.manifest_sha256.clone(),
            consistency_status: Some(consistency_status_name(&decision.status).into()),
            consistency_reasons: Some(serde_json::to_string(&decision.reasons)?),
            decision_sha256: Some(decision_sha256.clone()),
            evidence_json: evidence_json
                .as_ref()
                .map(|b| String::from_utf8_lossy(b).into_owned()),
            acquisition_request_id: self
                .db
                .generation(generation_id)?
                .and_then(|g| g.acquisition_request_id),
            installed_at: self
                .db
                .generation(generation_id)?
                .map(|g| g.installed_at)
                .unwrap_or_else(Utc::now),
            updated_at: Utc::now(),
        })?;
        Ok(wire::VerifyReply {
            request_id: request_id.into(),
            generation_id: generation_id.into(),
            state: state.into(),
            consistency_status: Some(consistency_status_name(&decision.status).into()),
            consistency_reasons: decision
                .reasons
                .iter()
                .map(|r| serde_json::to_string(r).unwrap_or_default())
                .collect(),
            decision_sha256: Some(decision_sha256),
            reason_code: None,
            reused: false,
        })
    }

    // ---------- activation (D5) ----------

    /// Conditional activation: only a `consistency_verified` generation may
    /// swap the atomic pointer, and only when `expected_previous` still
    /// matches. Refusals are durable terminal outcomes, not transient
    /// errors — replay returns the same reply.
    pub fn activate_generation(
        &self,
        request_id: &str,
        generation_id: &str,
        expected_previous: Option<&str>,
    ) -> Result<wire::ActivateReply, Error> {
        let payload = serde_json::to_string(&json!({
            "generation_id": generation_id,
            "expected_previous": expected_previous,
        }))?;
        let (_, reused) =
            self.db
                .accept_request(request_id, KIND_ACTIVATE, &payload, Utc::now())?;
        if reused && let Some(reply) = self.db.outcome::<wire::ActivateReply>(request_id)? {
            return Ok(wire::ActivateReply {
                reused: true,
                ..reply
            });
        }
        let _m = self.mutations.lock().expect("mutation lock poisoned");
        let reply = self.activate_now(request_id, generation_id, expected_previous)?;
        self.db.record_outcome(request_id, &reply, Utc::now())?;
        Ok(reply)
    }

    fn activate_now(
        &self,
        request_id: &str,
        generation_id: &str,
        expected_previous: Option<&str>,
    ) -> Result<wire::ActivateReply, Error> {
        let installed = match self.store.usable(generation_id, Utc::now()) {
            Ok(installed) => installed,
            Err(generation::Error::NotInstalled(_)) => {
                return Ok(refused_activate(
                    request_id,
                    generation_id,
                    "generation_not_installed",
                ));
            }
            Err(generation::Error::Corrupt(_, _)) => {
                return Ok(refused_activate(
                    request_id,
                    generation_id,
                    "generation_bytes_unverified",
                ));
            }
            Err(generation::Error::NotConsistencyVerified(_)) => {
                return Ok(refused_activate(
                    request_id,
                    generation_id,
                    "generation_not_consistency_verified",
                ));
            }
            Err(_) => {
                return Ok(refused_activate(
                    request_id,
                    generation_id,
                    "generation_bytes_unverified",
                ));
            }
        };
        match self
            .store
            .activate(&installed, expected_previous, request_id, Utc::now())
        {
            Ok(pointer) => {
                for row in self.db.generations()? {
                    let state = if row.generation_id == generation_id {
                        GEN_ACTIVE
                    } else if row.state == GEN_ACTIVE {
                        GEN_SUPERSEDED
                    } else {
                        continue;
                    };
                    self.db.upsert_generation(&store::Generation {
                        state: state.into(),
                        updated_at: Utc::now(),
                        ..row
                    })?;
                }
                // The swap is committed; retention failures never undo it.
                let retired_generations =
                    self.retire_expired_superseded(pointer.previous_generation_id.as_deref());
                Ok(wire::ActivateReply {
                    request_id: request_id.into(),
                    generation_id: generation_id.into(),
                    state: GEN_ACTIVE.into(),
                    reason_code: None,
                    detail: None,
                    manifest_sha256: Some(pointer.manifest_sha256),
                    activated_at: Some(pointer.activated_at.to_rfc3339()),
                    previous_generation_id: pointer.previous_generation_id,
                    reused: false,
                    retired_generations,
                })
            }
            Err(generation::Error::ActivationConflict { actual, .. }) => Ok(wire::ActivateReply {
                request_id: request_id.into(),
                generation_id: generation_id.into(),
                state: "refused".into(),
                reason_code: Some("activation_conflict".into()),
                detail: Some(format!("active generation is {actual:?}")),
                manifest_sha256: None,
                activated_at: None,
                previous_generation_id: actual,
                reused: false,
                retired_generations: vec![],
            }),
            Err(e) => Err(Error::Generation(e)),
        }
    }

    // ---------- retire (D2/D5) ----------

    /// Destructive retire to `evidence_only`: requires the generation's own
    /// manifest digest as confirm token, refuses the active generation and
    /// any generation with live pins (sessions/tasks). Bytes are deleted;
    /// manifest, decision and evidence stay for audit.
    pub fn retire_generation(
        &self,
        request_id: &str,
        generation_id: &str,
        confirm_token: &str,
    ) -> Result<wire::RetireReply, Error> {
        let payload = serde_json::to_string(&json!({
            "generation_id": generation_id,
            "confirm_token": confirm_token,
        }))?;
        let (_, reused) = self
            .db
            .accept_request(request_id, KIND_RETIRE, &payload, Utc::now())?;
        if reused && let Some(reply) = self.db.outcome::<wire::RetireReply>(request_id)? {
            return Ok(wire::RetireReply {
                reused: true,
                ..reply
            });
        }
        let _m = self.mutations.lock().expect("mutation lock poisoned");
        let reply = self.retire_now(request_id, generation_id, confirm_token)?;
        self.db.record_outcome(request_id, &reply, Utc::now())?;
        Ok(reply)
    }

    fn retire_now(
        &self,
        request_id: &str,
        generation_id: &str,
        confirm_token: &str,
    ) -> Result<wire::RetireReply, Error> {
        let Some(row) = self.db.generation(generation_id)? else {
            return Ok(refused_retire(
                request_id,
                generation_id,
                "generation_not_installed",
            ));
        };
        if row.state == GEN_EVIDENCE_ONLY {
            return Ok(wire::RetireReply {
                request_id: request_id.into(),
                generation_id: generation_id.into(),
                state: GEN_EVIDENCE_ONLY.into(),
                reason_code: None,
                detail: None,
                reused: false,
            });
        }
        if row.state == GEN_ACTIVE
            || self
                .store
                .active()?
                .is_some_and(|p| p.generation_id == generation_id)
        {
            return Ok(refused_retire(
                request_id,
                generation_id,
                "generation_active",
            ));
        }
        if confirm_token != row.manifest_sha256 {
            return Ok(refused_retire(
                request_id,
                generation_id,
                "confirm_token_mismatch",
            ));
        }
        match self.retire_bytes_and_mark(row)? {
            Ok(()) => Ok(wire::RetireReply {
                request_id: request_id.into(),
                generation_id: generation_id.into(),
                state: GEN_EVIDENCE_ONLY.into(),
                reason_code: None,
                detail: None,
                reused: false,
            }),
            Err(RetireBlock::Pinned(pins)) => Ok(wire::RetireReply {
                request_id: request_id.into(),
                generation_id: generation_id.into(),
                state: "refused".into(),
                reason_code: Some("generation_referenced".into()),
                detail: Some(format!("live pins: {}", pins.join(","))),
                reused: false,
            }),
            Err(RetireBlock::Inflight(task_id)) => Ok(wire::RetireReply {
                request_id: request_id.into(),
                generation_id: generation_id.into(),
                state: "refused".into(),
                reason_code: Some("generation_referenced".into()),
                detail: Some(format!("inflight acquisition task {task_id}")),
                reused: false,
            }),
        }
    }

    /// Automatic retention after a committed swap: superseded generations
    /// beyond the newest `RETAINED_SUPERSEDED` (by install time) retire
    /// through the same pin and in-flight checks as the manual retire.
    /// Blocked or failing generations are skipped and never fail the
    /// activation that triggered them.
    fn retire_expired_superseded(&self, previous: Option<&str>) -> Vec<String> {
        let mut superseded = match self.db.generations_in(&[GEN_SUPERSEDED]) {
            Ok(rows) => rows,
            Err(e) => {
                eprintln!("acquire retention: listing superseded generations failed: {e}");
                return vec![];
            }
        };
        superseded.sort_by_key(|g| std::cmp::Reverse(g.installed_at));
        let mut retired = vec![];
        for row in superseded.into_iter().skip(RETAINED_SUPERSEDED) {
            // The generation this activation just replaced is the rollback
            // target; it keeps its bytes whatever its install time.
            if previous == Some(row.generation_id.as_str()) {
                continue;
            }
            let generation_id = row.generation_id.clone();
            match self.retire_bytes_and_mark(row) {
                Ok(Ok(())) => retired.push(generation_id),
                Ok(Err(_blocked)) => {}
                Err(e) => {
                    eprintln!("acquire retention: retiring {generation_id} failed: {e}");
                }
            }
        }
        retired
    }

    /// Shared retire action: a live pin or an accepted/running acquisition
    /// task blocks the retire (not an error); otherwise the bytes are
    /// deleted and the row becomes `evidence_only`. Callers own the
    /// active-generation and confirm-token checks.
    fn retire_bytes_and_mark(
        &self,
        row: store::Generation,
    ) -> Result<Result<(), RetireBlock>, Error> {
        let pins = self.store.pins(&row.generation_id)?;
        if !pins.is_empty() {
            return Ok(Err(RetireBlock::Pinned(pins)));
        }
        if let Some(acq_request) = row.acquisition_request_id.as_deref()
            && let Some(task) = self.db.task_by_request(acq_request)?
            && matches!(task.state.as_str(), ST_ACCEPTED | ST_RUNNING)
        {
            return Ok(Err(RetireBlock::Inflight(task.task_id)));
        }
        self.store.retire_bytes(&row.generation_id)?;
        self.db.upsert_generation(&store::Generation {
            state: GEN_EVIDENCE_ONLY.into(),
            updated_at: Utc::now(),
            ..row
        })?;
        Ok(Ok(()))
    }

    // ---------- status ----------

    pub fn sync_status(&self) -> Result<wire::StatusView, Error> {
        let mut generations = vec![];
        for g in self.db.generations()? {
            generations.push(wire::GenerationView {
                generation_id: g.generation_id,
                state: g.state,
                manifest_sha256: g.manifest_sha256,
                consistency_status: g.consistency_status,
                consistency_reasons: g
                    .consistency_reasons
                    .and_then(|r| serde_json::from_str(&r).ok()),
                decision_sha256: g.decision_sha256,
                acquisition_request_id: g.acquisition_request_id,
                installed_at: g.installed_at.to_rfc3339(),
            });
        }
        let inflight = self
            .db
            .tasks_in(&[ST_ACCEPTED, ST_RUNNING])?
            .iter()
            .map(task_view)
            .collect();
        Ok(wire::StatusView {
            active: self.store.active()?,
            generations,
            inflight_tasks: inflight,
            generation_contract_version: GENERATION_CONTRACT_VERSION.into(),
            consistency_contract_version: format!(
                "{}+{PROBE_CONTRACT_VERSION}",
                crate::file_consistency::CONTRACT_VERSION
            ),
        })
    }
}

/// Why `retire_bytes_and_mark` declined to retire a generation.
enum RetireBlock {
    Pinned(Vec<String>),
    Inflight(String),
}

fn detail_bytes(detail: &Value) -> u64 {
    detail["files"]
        .as_object()
        .map(|f| f.values().filter_map(|v| v["bytes"].as_u64()).sum())
        .unwrap_or(0)
}
fn task_view(task: &store::Task) -> wire::AcquisitionTaskView {
    wire::AcquisitionTaskView {
        task_id: task.task_id.clone(),
        request_id: task.request_id.clone(),
        kind: task.kind.clone(),
        state: task.state.clone(),
        generation_id: task.generation_id.clone(),
        stage: task.stage.clone(),
        bytes_done: task.bytes_done,
        error: task.error.clone(),
        detail: task.detail.clone(),
        created_at: task.created_at.to_rfc3339(),
        updated_at: task.updated_at.to_rfc3339(),
    }
}
fn consistency_status_name(status: &ConsistencyStatus) -> &'static str {
    match status {
        ConsistencyStatus::VerifiedCompatible => "VERIFIED_COMPATIBLE",
        ConsistencyStatus::NotVerified => "NOT_VERIFIED",
        ConsistencyStatus::Conflict => "CONFLICT",
    }
}
fn refused_verify(request_id: &str, generation_id: &str, code: &'static str) -> wire::VerifyReply {
    wire::VerifyReply {
        request_id: request_id.into(),
        generation_id: generation_id.into(),
        state: "refused".into(),
        consistency_status: None,
        consistency_reasons: vec![],
        decision_sha256: None,
        reason_code: Some(code.into()),
        reused: false,
    }
}
fn refused_activate(
    request_id: &str,
    generation_id: &str,
    code: &'static str,
) -> wire::ActivateReply {
    wire::ActivateReply {
        request_id: request_id.into(),
        generation_id: generation_id.into(),
        state: "refused".into(),
        reason_code: Some(code.into()),
        detail: None,
        manifest_sha256: None,
        activated_at: None,
        previous_generation_id: None,
        reused: false,
        retired_generations: vec![],
    }
}
fn refused_retire(request_id: &str, generation_id: &str, code: &'static str) -> wire::RetireReply {
    wire::RetireReply {
        request_id: request_id.into(),
        generation_id: generation_id.into(),
        state: "refused".into(),
        reason_code: Some(code.into()),
        detail: None,
        reused: false,
    }
}
fn file_facts(installed: &Installed) -> Vec<FileFact> {
    installed
        .manifest
        .files
        .iter()
        .map(|f| FileFact {
            role: f.role,
            bytes: f.bytes,
            sha256: f.sha256.clone(),
            source_url: f.source_url.clone(),
            acquisition_started_at: f.acquisition_started_at,
            acquisition_completed_at: f.acquisition_completed_at,
            last_modified: f.last_modified.clone(),
            etag: f.etag.clone(),
            byte_integrity: ByteIntegrity::Verified,
            source_revision: f.etag.clone().or_else(|| f.last_modified.clone()),
            source_identity: Some(f.source_url.clone()),
        })
        .collect()
}
/// The auto-issued proof (D1): member bindings pin each role's exact hash
/// plus its remote revision facts; the validity window is the acquisition
/// interval recorded in the manifest. Constructed only when every
/// structural probe passed — `file_consistency::validate` still applies
/// the full #131 contract to it.
fn build_evidence(installed: &Installed, facts: &[FileFact]) -> CompatibilityEvidence {
    let mut member_bindings = BTreeMap::new();
    for fact in facts {
        member_bindings.insert(
            fact.role,
            MemberBinding {
                sha256: fact.sha256.clone(),
                source_revision: fact.source_revision.clone(),
                source_identity: fact.source_identity.clone(),
            },
        );
    }
    CompatibilityEvidence {
        evidence_ref: format!("x-data-acquire.auto-probe-v1/{}", installed.generation_id),
        rule_version: crate::file_consistency::CONTRACT_VERSION.into(),
        compatibility_set_revision: format!("{PROBE_CONTRACT_VERSION}:{}", installed.generation_id),
        compatibility_set_identity: "tdx-official-public-files".into(),
        member_bindings,
        valid_window: CompatibilityWindow {
            valid_from: facts
                .iter()
                .map(|f| f.acquisition_started_at)
                .min()
                .unwrap_or_else(Utc::now),
            valid_until: facts
                .iter()
                .map(|f| f.acquisition_completed_at)
                .max()
                .unwrap_or_else(Utc::now),
        },
    }
}

// ---------- structural probes (D1 gate) ----------

struct ProbeReport {
    facts: Value,
    failures: Vec<String>,
}

/// Run every structural validator against the installed bytes. Each probe
/// records its facts verbatim into the decision artifact; any failure
/// means no evidence is issued and the #131 decision stays NOT_VERIFIED.
fn probes(installed: &Installed) -> ProbeReport {
    let mut facts = json!({});
    let mut failures = vec![];
    let artifact = |role: FileRole| -> Option<PathBuf> {
        installed
            .manifest
            .files
            .iter()
            .find(|f| f.role == role)
            .map(|f| installed.dir.join(&f.relative_path))
    };
    let scoped = match probe_base(artifact(FileRole::BaseIndustry), &mut facts) {
        Ok(members) => Some(members),
        Err(e) => {
            failures.push(format!("base_archive:{e}"));
            None
        }
    };
    match probe_gbbq(artifact(FileRole::Gbbq), &mut facts) {
        Ok(()) => {}
        Err(e) => failures.push(format!("gbbq_archive:{e}")),
    }
    let day = match probe_day(artifact(FileRole::Day), &mut facts) {
        Ok(report) => Some(report),
        Err(e) => {
            failures.push(format!("day_archive:{e}"));
            None
        }
    };
    if let (Some(scoped), Some(day)) = (&scoped, &day) {
        probe_coverage(scoped, day, installed, &mut facts, &mut failures);
    }
    ProbeReport { facts, failures }
}

fn member_bytes(zip: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Result<Vec<u8>, String> {
    let mut entry = zip
        .by_name(name)
        .map_err(|e| format!("member {name} unavailable: {e}"))?;
    if entry.size() > 64 << 20 {
        return Err(format!("member {name} exceeds bounded size"));
    }
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    std::io::Read::read_to_end(&mut entry, &mut bytes)
        .map_err(|e| format!("member {name} unreadable: {e}"))?;
    Ok(bytes)
}

/// base.zip must carry and parse the two members the catalog pipeline
/// consumes, and scope resolution must yield at least one member.
fn probe_base(path: Option<PathBuf>, facts: &mut Value) -> Result<Vec<(u8, String)>, String> {
    let path = path.ok_or("base.zip absent from manifest")?;
    let file = std::fs::File::open(&path).map_err(|e| format!("unreadable: {e}"))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("invalid zip: {e}"))?;
    let tdxhy = member_bytes(&mut zip, "tdxhy.cfg")?;
    let ex = member_bytes(&mut zip, "infoharbor_ex.code")?;
    let rows = crate::catalog_tdx::tdxhy::parse(&tdxhy).map_err(|e| format!("tdxhy: {e}"))?;
    let ex_rows =
        crate::catalog_tdx::ex_code::parse(&ex).map_err(|e| format!("infoharbor_ex: {e}"))?;
    let scoped = crate::catalog_tdx::scope::apply(&rows);
    if scoped.members.is_empty() {
        return Err("scope yields no members".into());
    }
    facts["base_archive"] = json!({
        "status": "pass",
        "tdxhy_rows": rows.len(),
        "infoharbor_ex_rows": ex_rows.len(),
        "scoped_members": scoped.members.len(),
        "excluded_rows": scoped.excluded.len(),
    });
    Ok(scoped
        .members
        .iter()
        .map(|m| (m.market, m.code.clone()))
        .collect())
}

/// gbbq.zip must carry the `gbbq` member and decode to at least one event.
fn probe_gbbq(path: Option<PathBuf>, facts: &mut Value) -> Result<(), String> {
    let path = path.ok_or("gbbq.zip absent from manifest")?;
    let file = std::fs::File::open(&path).map_err(|e| format!("unreadable: {e}"))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("invalid zip: {e}"))?;
    let member = member_bytes(&mut zip, "gbbq")?;
    let raw = crate::gbbq::parse(&member).map_err(|e| format!("gbbq parse: {e}"))?;
    let events = crate::gbbq::decode_events(&raw).map_err(|e| format!("gbbq decode: {e}"))?;
    if events.is_empty() {
        return Err("gbbq member decodes zero events".into());
    }
    facts["gbbq_archive"] = json!({
        "status": "pass",
        "member_bytes": member.len(),
        "events": events.len(),
    });
    Ok(())
}

struct DayReport {
    /// Normalized member name (`sh/lday/sh600000.day`) → stored zip name
    /// (TDX archives use `\` separators).
    member_names: std::collections::BTreeMap<String, String>,
    max_last_bar: Option<chrono::NaiveDate>,
}

/// hsjday.zip must carry `.day` members under `<market>/lday/`; a
/// deterministic sample of members must fully parse, and the freshest
/// sampled bar must be near the acquisition date (stale-mix guard).
fn probe_day(path: Option<PathBuf>, facts: &mut Value) -> Result<DayReport, String> {
    let path = path.ok_or("hsjday.zip absent from manifest")?;
    let file = std::fs::File::open(&path).map_err(|e| format!("unreadable: {e}"))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("invalid zip: {e}"))?;
    let mut names = std::collections::BTreeMap::new();
    for name in zip.file_names() {
        let n = name.replace('\\', "/");
        if n.ends_with(".day") && matches!(n.get(..7), Some("sh/lday" | "sz/lday" | "bj/lday")) {
            names.insert(n, name.to_string());
        }
    }
    if names.is_empty() {
        return Err("no lday .day members".into());
    }
    // Deterministic sample: every Nth sorted member so reruns are stable.
    let stride = (names.len() / DAY_SAMPLE).max(1);
    let sample: Vec<&String> = names.keys().step_by(stride).take(DAY_SAMPLE).collect();
    let mut max_last_bar: Option<chrono::NaiveDate> = None;
    let mut parsed = 0usize;
    for name in &sample {
        let member =
            member_bytes(&mut zip, &names[*name]).map_err(|e| format!("sample {name}: {e}"))?;
        let records =
            crate::day_file::parse(&member).map_err(|e| format!("sample {name} malformed: {e}"))?;
        let last = records
            .last()
            .ok_or_else(|| format!("sample {name} carries no records"))?;
        parsed += 1;
        max_last_bar = Some(match max_last_bar {
            Some(max) => max.max(last.date),
            None => last.date,
        });
    }
    facts["day_archive"] = json!({
        "status": "pass",
        "members": names.len(),
        "parsed_sample": parsed,
        "max_sample_last_bar": max_last_bar.map(|d| d.to_string()),
    });
    Ok(DayReport {
        member_names: names,
        max_last_bar,
    })
}

/// Cross-file coherence: scoped members must overwhelmingly have `.day`
/// members, and the freshest sampled day bar must sit inside the stale-mix
/// tolerance of the acquisition date.
fn probe_coverage(
    scoped: &[(u8, String)],
    day: &DayReport,
    installed: &Installed,
    facts: &mut Value,
    failures: &mut Vec<String>,
) {
    let mut missing = vec![];
    for (market, code) in scoped {
        let market_name = if *market == 1 { "sh" } else { "sz" };
        let member = format!("{market_name}/lday/{market_name}{code}.day");
        if !day.member_names.contains_key(&member) {
            missing.push(format!("{market_name}:{code}"));
        }
    }
    let acquired = installed
        .manifest
        .files
        .iter()
        .map(|f| f.acquisition_completed_at)
        .max()
        .map(crate::calendar::shanghai_date);
    let mut stale = false;
    if let (Some(max_bar), Some(acquired)) = (day.max_last_bar, acquired) {
        let age = acquired - max_bar;
        stale = age.num_days() > STALE_DAY_TOLERANCE_DAYS || age.num_days() < 0;
        facts["day_recency"] = json!({
            "max_sample_last_bar": max_bar.to_string(),
            "acquisition_date": acquired.to_string(),
            "age_days": age.num_days(),
            "tolerance_days": STALE_DAY_TOLERANCE_DAYS,
        });
    } else {
        failures.push("day_recency:unmeasurable".into());
    }
    facts["cross_file_coverage"] = json!({
        "scoped_members": scoped.len(),
        "missing_day_members": missing.len(),
        "missing_sample": missing.iter().take(20).cloned().collect::<Vec<_>>(),
        "bound": MAX_MISSING_SCOPED_DAY,
    });
    if missing.len() > MAX_MISSING_SCOPED_DAY {
        failures.push(format!(
            "cross_file_coverage:{} scoped members lack .day members",
            missing.len()
        ));
    }
    if stale {
        failures.push("day_recency:freshest bar outside acquisition tolerance".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `fetch_role` resume check re-hashes the staged artifact with the
    /// streaming digest (XDM-81). This pins both directions: a staged copy
    /// that still matches its pin is kept, and a same-length tampered copy is
    /// refetched instead of being accepted as the pinned bytes.
    #[test]
    fn resume_keeps_a_matching_staged_artifact_and_refetches_a_tampered_one() {
        let mirror = tempfile::tempdir().unwrap();
        let pinned = vec![7u8; 3 * crate::hash::STREAM_BUFFER + 1];
        std::fs::write(mirror.path().join("hsjday.zip"), &pinned).unwrap();
        let root = tempfile::tempdir().unwrap();
        let service =
            AcquireService::open(root.path(), Source::Mirror(mirror.path().into())).unwrap();
        let task = store::Task {
            task_id: "task".into(),
            request_id: "req".into(),
            kind: KIND_TASK.into(),
            state: ST_RUNNING.into(),
            generation_id: None,
            stage: None,
            bytes_done: 0,
            error: None,
            detail: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let role = ROLES.iter().find(|r| r.key == "day").unwrap();
        let stage = service.store.create_stage("req").unwrap();

        // A crash between the artifact write and the stage commit leaves the
        // staged copy in place with its pin already recorded.
        std::fs::write(stage.join("hsjday.zip"), &pinned).unwrap();
        let mut detail = json!({"files": {"day": {
            "sha256": crate::hash::sha256(&pinned), "bytes": pinned.len() as u64,
        }}});
        service.fetch_role(&task, role, &mut detail).unwrap();
        assert_eq!(std::fs::read(stage.join("hsjday.zip")).unwrap(), pinned);
        assert_eq!(
            detail["files"]["day"]["sha256"],
            json!(crate::hash::sha256(&pinned))
        );

        // Same length, different bytes: only the digest can catch it, so the
        // resume check must reject it and fetch the mirror's bytes instead.
        let mut tampered = pinned.clone();
        tampered[tampered.len() / 2] ^= 1;
        std::fs::write(stage.join("hsjday.zip"), &tampered).unwrap();
        service.fetch_role(&task, role, &mut detail).unwrap();
        assert_eq!(std::fs::read(stage.join("hsjday.zip")).unwrap(), pinned);
        assert_eq!(
            detail["files"]["day"]["sha256"],
            json!(crate::hash::sha256(&pinned))
        );
    }
}
