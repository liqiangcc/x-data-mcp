//! Read-only listing of the export root (`export_research --list-snapshots`,
//! D-83, 备忘录 T8, Refs #383 #549).
//!
//! Storage is single-version, so an export directory is the only record of the
//! inputs a backtest ran on. D-83 therefore has three rules: nothing deletes an
//! export automatically, only the owner deletes one, and a snapshot whose
//! source generation has been retired is marked irreplaceable. This module is
//! the tool for the second and third rule: it **lists**, and nothing else. It
//! opens no file for writing, removes nothing and creates nothing, not even a
//! missing root.
//!
//! Each snapshot is described from its own `manifest.json` (schema, source
//! generations) plus the filesystem (size, creation time). The state of each
//! source generation comes from the acquire store (`generations` table of
//! `acquire.sqlite`), opened read-only. The market database does not hold it:
//! `source_bindings` carries the generation id and manifest sha256 but not the
//! lifecycle state.
//!
//! A listing never reports a problem as clean:
//!
//! - an entry without a readable `manifest.json`, a directory name that is not
//!   the manifest's `snapshot_id`, a listed file that is missing or has another
//!   size, and a file the manifest does not list are `damaged` with the reason;
//! - a generation the acquire store does not know, or a store that cannot be
//!   read, gives state `unknown` and `irreplaceable: null` (not `false`);
//! - anything else in the root (loose files, `.tmp-*` leftovers of a crashed
//!   export, symlinks) is listed under `unexpected_entries`.
//!
//! File contents are not hashed here: that reads gigabytes per snapshot. The
//! byte check is `scripts/export_smoke.py --snapshot-dir` (`files[].sha256` and
//! the `snapshot_id` recomputation); this listing checks existence and size.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

use super::Error;

/// The acquire store state of a generation whose bytes were deleted
/// (`src/acquire/service.rs`, `GEN_EVIDENCE_ONLY`).
pub const STATE_EVIDENCE_ONLY: &str = "evidence_only";
/// A generation the store does not list, or a store that could not be read.
pub const STATE_UNKNOWN: &str = "unknown";

/// What the acquire store says about one generation id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredGeneration {
    pub state: String,
    pub manifest_sha256: String,
}

/// The acquire store, read once: either its `generations` rows or why they
/// could not be read.
#[derive(Debug)]
pub enum GenerationStates {
    /// No acquire store was named or found; every state is unknown.
    NotConfigured,
    Unreadable(String),
    Loaded(BTreeMap<String, StoredGeneration>),
}

impl GenerationStates {
    /// Read `generations` from `path` with a read-only, `query_only`
    /// connection. Never creates the file and never writes to it.
    pub fn load(path: &Path) -> Self {
        match read_generations(path) {
            Ok(map) => Self::Loaded(map),
            Err(reason) => Self::Unreadable(format!("{}: {reason}", path.display())),
        }
    }
}

fn read_generations(path: &Path) -> Result<BTreeMap<String, StoredGeneration>, String> {
    if !path.is_file() {
        return Err("no such file".into());
    }
    let c = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| e.to_string())?;
    c.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    c.pragma_update(None, "query_only", true)
        .map_err(|e| e.to_string())?;
    let mut statement = c
        .prepare("SELECT generation_id,state,manifest_sha256 FROM generations")
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                StoredGeneration {
                    state: r.get(1)?,
                    manifest_sha256: r.get(2)?,
                },
            ))
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<BTreeMap<_, _>, _>>()
        .map_err(|e| e.to_string())
}

/// The listing of one export root.
#[derive(Debug)]
pub struct Listing {
    pub root: PathBuf,
    pub acquire_db: Option<PathBuf>,
    /// Why generation states could not be read; `None` when they were.
    pub states_problem: Option<String>,
    pub snapshots: Vec<Value>,
    pub unexpected_entries: Vec<Value>,
}

impl Listing {
    /// Damaged snapshots, unexpected entries and unreadable states, counted.
    /// Zero means nothing in the listing needs attention.
    pub fn problem_count(&self) -> usize {
        let damaged = self
            .snapshots
            .iter()
            .filter(|s| s["status"] != "ok")
            .count();
        let unknown = self
            .snapshots
            .iter()
            .filter(|s| s["irreplaceable"].is_null())
            .count();
        damaged + unknown + self.unexpected_entries.len()
    }

    pub fn to_json(&self) -> Value {
        let total: u64 = self
            .snapshots
            .iter()
            .filter_map(|s| s["bytes"].as_u64())
            .sum();
        json!({
            "root": self.root.display().to_string(),
            "acquire_db": self.acquire_db.as_ref().map(|p| p.display().to_string()),
            "generation_states": self.states_problem.as_deref().unwrap_or("read"),
            "snapshot_count": self.snapshots.len(),
            "total_bytes": total,
            "irreplaceable_count": self.snapshots.iter().filter(|s| s["irreplaceable"] == true).count(),
            "problem_count": self.problem_count(),
            "snapshots": self.snapshots,
            "unexpected_entries": self.unexpected_entries,
        })
    }

    /// The text summary: one line per snapshot, problems indented under it.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "{} snapshot(s) under {}\n",
            self.snapshots.len(),
            self.root.display()
        ));
        match (&self.acquire_db, &self.states_problem) {
            (_, Some(problem)) => {
                out.push_str(&format!("generation states NOT READ: {problem}\n"));
            }
            (Some(path), None) => {
                out.push_str(&format!("generation states from {}\n", path.display()));
            }
            (None, None) => {}
        }
        for s in &self.snapshots {
            let flag = match &s["irreplaceable"] {
                Value::Bool(true) => "IRREPLACEABLE",
                Value::Bool(false) => "replaceable-source",
                _ => "irreplaceable=UNKNOWN",
            };
            let generations = s["generations"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .map(|g| {
                            format!(
                                "{}={}",
                                short(g["generation_id"].as_str().unwrap_or("?")),
                                g["state"].as_str().unwrap_or("?")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            out.push_str(&format!(
                "{}  {}  {} bytes  created {}  {}  [{}]  {}\n",
                s["snapshot_id"].as_str().unwrap_or("?"),
                if s["status"] == "ok" { "ok" } else { "DAMAGED" },
                s["bytes"],
                s["created_at"].as_str().unwrap_or("?"),
                s["schema"].as_str().unwrap_or("schema?"),
                generations,
                flag,
            ));
            for problem in s["problems"].as_array().into_iter().flatten() {
                out.push_str(&format!(
                    "    problem: {}\n",
                    problem.as_str().unwrap_or("?")
                ));
            }
        }
        for e in &self.unexpected_entries {
            out.push_str(&format!(
                "unexpected entry {}: {}\n",
                e["name"].as_str().unwrap_or("?"),
                e["reason"].as_str().unwrap_or("?")
            ));
        }
        out.push_str(&format!(
            "{} item(s) need attention; nothing was changed or deleted\n",
            self.problem_count()
        ));
        out
    }
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

/// List `root`. Fails only when the root itself cannot be read; every problem
/// below it is reported inside the [`Listing`].
pub fn list_snapshots(
    root: &Path,
    acquire_db: Option<&Path>,
    states: &GenerationStates,
) -> Result<Listing, Error> {
    let entries = fs::read_dir(root)
        .map_err(|e| Error::Io(format!("cannot read export root {}: {e}", root.display())))?;
    let mut names: Vec<(String, fs::FileType)> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| Error::Io(format!("{}: {e}", root.display())))?;
        let file_type = entry
            .file_type()
            .map_err(|e| Error::Io(format!("{}: {e}", entry.path().display())))?;
        names.push((entry.file_name().to_string_lossy().into_owned(), file_type));
    }
    names.sort_by(|a, b| a.0.cmp(&b.0));

    let mut snapshots = Vec::new();
    let mut unexpected = Vec::new();
    for (name, file_type) in names {
        let reason = if file_type.is_symlink() {
            Some("a symbolic link, not a snapshot directory")
        } else if !file_type.is_dir() {
            Some("a file, not a snapshot directory")
        } else if name.starts_with(".tmp-") {
            Some("leftover of an export that did not finish (no manifest.json is promised)")
        } else {
            None
        };
        match reason {
            Some(reason) => unexpected.push(json!({"name": name, "reason": reason})),
            None => snapshots.push(describe(&root.join(&name), &name, states)),
        }
    }
    let states_problem = match states {
        GenerationStates::NotConfigured => {
            Some("no acquire store: pass --acquire-db, or --db so generations/acquire.sqlite next to it is used".to_string())
        }
        GenerationStates::Unreadable(reason) => Some(reason.clone()),
        GenerationStates::Loaded(_) => None,
    };
    Ok(Listing {
        root: root.to_path_buf(),
        acquire_db: acquire_db.map(Path::to_path_buf),
        states_problem,
        snapshots,
        unexpected_entries: unexpected,
    })
}

fn time_text(time: SystemTime) -> String {
    DateTime::<Utc>::from(time).to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Sum of file sizes below `dir`; symlinks are counted as links, not followed.
fn tree_bytes(dir: &Path, problems: &mut Vec<String>) -> u64 {
    let mut total = 0;
    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        Err(e) => {
            problems.push(format!("cannot read {}: {e}", dir.display()));
            return 0;
        }
    };
    for entry in read.flatten() {
        match entry.file_type() {
            Ok(t) if t.is_dir() => total += tree_bytes(&entry.path(), problems),
            Ok(_) => match entry.metadata() {
                Ok(m) => total += m.len(),
                Err(e) => problems.push(format!("cannot stat {}: {e}", entry.path().display())),
            },
            Err(e) => problems.push(format!("cannot stat {}: {e}", entry.path().display())),
        }
    }
    total
}

fn describe(dir: &Path, name: &str, states: &GenerationStates) -> Value {
    let mut problems: Vec<String> = Vec::new();
    let bytes = tree_bytes(dir, &mut problems);
    let manifest_path = dir.join("manifest.json");

    let (created_at, created_source) =
        match fs::metadata(&manifest_path).or_else(|_| fs::metadata(dir)) {
            Ok(m) => match m.created() {
                Ok(t) => (Some(time_text(t)), "birth"),
                Err(_) => (m.modified().ok().map(time_text), "mtime"),
            },
            Err(_) => (None, "unavailable"),
        };

    let manifest: Option<Value> = match fs::read(&manifest_path) {
        Err(e) => {
            problems.push(format!("manifest.json cannot be read: {e}"));
            None
        }
        Ok(raw) => match serde_json::from_slice::<Value>(&raw) {
            Ok(v) if v.is_object() => Some(v),
            Ok(_) => {
                problems.push("manifest.json is not a JSON object".into());
                None
            }
            Err(e) => {
                problems.push(format!("manifest.json is not valid JSON: {e}"));
                None
            }
        },
    };
    let manifest_sha256 = fs::read(&manifest_path)
        .ok()
        .map(|raw| crate::hash::sha256(&raw));

    let mut schema = None;
    let mut generations: Vec<Value> = Vec::new();
    if let Some(m) = &manifest {
        schema = m["schema"].as_str().map(str::to_string);
        if schema.is_none() {
            problems.push("manifest has no schema string".into());
        }
        match m["snapshot_id"].as_str() {
            Some(id) if id == name => {}
            Some(id) => problems.push(format!(
                "directory name {name} is not the manifest snapshot_id {id}"
            )),
            None => problems.push("manifest has no snapshot_id".into()),
        }
        check_files(dir, m, &mut problems);
        match m["generations"].as_array() {
            Some(list) if !list.is_empty() => {
                for g in list {
                    generations.push(generation_entry(g, states));
                }
            }
            _ => problems.push("manifest names no source generation".into()),
        }
    }

    let irreplaceable = verdict(&generations);
    let reason = match &irreplaceable {
        Value::Bool(true) => {
            "a source generation is evidence_only: its bytes are deleted, so this directory is the only record"
        }
        Value::Bool(false) => {
            "every source generation still holds its bytes; it can retire later, so this is not a promise"
        }
        _ => "a source generation's state could not be determined",
    };
    json!({
        "snapshot_id": name,
        "directory": dir.display().to_string(),
        "status": if problems.is_empty() { "ok" } else { "damaged" },
        "problems": problems,
        "bytes": bytes,
        "created_at": created_at,
        "created_at_source": created_source,
        "schema": schema,
        "manifest_sha256": manifest_sha256,
        "generations": generations,
        "irreplaceable": irreplaceable,
        "irreplaceable_reason": reason,
    })
}

fn check_files(dir: &Path, manifest: &Value, problems: &mut Vec<String>) {
    let mut listed: Vec<String> = Vec::new();
    match manifest["files"].as_array() {
        Some(files) if !files.is_empty() => {
            for entry in files {
                let Some(file) = entry["path"].as_str() else {
                    problems.push("a files[] entry has no path".into());
                    continue;
                };
                let plain = Path::new(file)
                    .file_name()
                    .is_some_and(|n| n.to_str() == Some(file));
                if !plain {
                    problems.push(format!("files[].path {file:?} is not a plain file name"));
                    continue;
                }
                listed.push(file.to_string());
                match fs::metadata(dir.join(file)) {
                    Err(_) => problems.push(format!("{file}: listed in the manifest but missing")),
                    Ok(m) => match entry["bytes"].as_u64() {
                        Some(want) if want == m.len() => {}
                        Some(want) => problems.push(format!(
                            "{file}: {} bytes on disk, manifest says {want}",
                            m.len()
                        )),
                        None => problems.push(format!("{file}: manifest records no byte size")),
                    },
                }
            }
        }
        _ => problems.push("manifest lists no file".into()),
    }
    if let Ok(read) = fs::read_dir(dir) {
        for entry in read.flatten() {
            let n = entry.file_name().to_string_lossy().into_owned();
            if n != "manifest.json" && !listed.contains(&n) {
                problems.push(format!(
                    "{n}: in the directory but not listed in the manifest"
                ));
            }
        }
    }
}

fn generation_entry(g: &Value, states: &GenerationStates) -> Value {
    let id = g["generation_id"].as_str().unwrap_or("");
    let sha = g["manifest_sha256"].as_str().unwrap_or("");
    let (state, note) = match states {
        GenerationStates::NotConfigured => (STATE_UNKNOWN.to_string(), "acquire store not named"),
        GenerationStates::Unreadable(_) => (STATE_UNKNOWN.to_string(), "acquire store unreadable"),
        GenerationStates::Loaded(map) => match map.get(id) {
            None => (
                STATE_UNKNOWN.to_string(),
                "generation not in the acquire store",
            ),
            Some(stored) if stored.manifest_sha256 != sha => (
                STATE_UNKNOWN.to_string(),
                "acquire store has this id with another manifest sha256",
            ),
            Some(stored) => (stored.state.clone(), ""),
        },
    };
    json!({
        "generation_id": id,
        "manifest_sha256": sha,
        "state": state,
        "note": note,
    })
}

/// `true` when any source generation is retired, `null` when none is but some
/// state is unknown (or there is none to look at), `false` otherwise.
fn verdict(generations: &[Value]) -> Value {
    if generations
        .iter()
        .any(|g| g["state"] == STATE_EVIDENCE_ONLY)
    {
        return Value::Bool(true);
    }
    if generations.is_empty() || generations.iter().any(|g| g["state"] == STATE_UNKNOWN) {
        return Value::Null;
    }
    Value::Bool(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "ab";

    fn write_snapshot(root: &Path, id: &str, generation: &str, schema: &str) -> PathBuf {
        let dir = root.join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.parquet"), b"12345").unwrap();
        let manifest = json!({
            "schema": schema,
            "snapshot_id": id,
            "generations": [{"generation_id": generation, "manifest_sha256": SHA}],
            "files": [{"path": "a.parquet", "bytes": 5, "sha256": "x"}],
        });
        fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
        dir
    }

    fn store(root: &Path, rows: &[(&str, &str, &str)]) -> PathBuf {
        let path = root.join("acquire.sqlite");
        let c = Connection::open(&path).unwrap();
        c.execute_batch(
            "CREATE TABLE generations (generation_id TEXT PRIMARY KEY, state TEXT NOT NULL, \
             manifest_sha256 TEXT NOT NULL)",
        )
        .unwrap();
        for (id, state, sha) in rows {
            c.execute(
                "INSERT INTO generations VALUES (?1,?2,?3)",
                [id, state, sha],
            )
            .unwrap();
        }
        path
    }

    fn list(root: &Path, db: Option<&Path>) -> Listing {
        let states = db.map_or(GenerationStates::NotConfigured, GenerationStates::load);
        list_snapshots(root, db, &states).unwrap()
    }

    #[test]
    fn states_and_the_irreplaceable_flag_follow_the_acquire_store() {
        let tmp = tempfile::tempdir().unwrap();
        let exports = tmp.path().join("exports");
        fs::create_dir(&exports).unwrap();
        write_snapshot(&exports, "snapshot-a", "g-active", "x.v5");
        write_snapshot(&exports, "snapshot-b", "g-old", "x.v4");
        write_snapshot(&exports, "snapshot-c", "g-super", "x.v3");
        let db = store(
            tmp.path(),
            &[
                ("g-active", "active", SHA),
                ("g-old", "evidence_only", SHA),
                ("g-super", "superseded", SHA),
            ],
        );
        let listing = list(&exports, Some(&db));
        assert_eq!(listing.problem_count(), 0, "{}", listing.to_text());
        let by_id = |id: &str| {
            listing
                .snapshots
                .iter()
                .find(|s| s["snapshot_id"] == id)
                .unwrap()
        };
        assert_eq!(by_id("snapshot-a")["generations"][0]["state"], "active");
        assert_eq!(by_id("snapshot-a")["irreplaceable"], false);
        assert_eq!(
            by_id("snapshot-b")["generations"][0]["state"],
            "evidence_only"
        );
        assert_eq!(by_id("snapshot-b")["irreplaceable"], true);
        assert_eq!(by_id("snapshot-c")["irreplaceable"], false);
        assert_eq!(by_id("snapshot-b")["schema"], "x.v4");
        assert!(by_id("snapshot-b")["bytes"].as_u64().unwrap() > 5);
        assert!(by_id("snapshot-b")["created_at"].is_string());
        assert_eq!(listing.to_json()["irreplaceable_count"], 1);
        assert!(listing.to_text().contains("IRREPLACEABLE"));
    }

    #[test]
    fn an_unreadable_or_missing_store_is_unknown_not_clean() {
        let tmp = tempfile::tempdir().unwrap();
        write_snapshot(tmp.path(), "snapshot-a", "g", "x.v5");
        let none = list(tmp.path(), None);
        assert_eq!(none.snapshots[0]["irreplaceable"], Value::Null);
        assert_eq!(none.snapshots[0]["generations"][0]["state"], "unknown");
        assert!(none.states_problem.is_some());
        assert!(none.problem_count() > 0);

        let missing = tmp.path().join("nope.sqlite");
        let gone = list(tmp.path(), Some(&missing));
        assert_eq!(gone.snapshots[0]["irreplaceable"], Value::Null);
        assert!(gone.states_problem.unwrap().contains("nope.sqlite"));

        // A file that is not a database.
        let junk = tmp.path().join("junk.sqlite");
        fs::write(&junk, b"not sqlite at all, not sqlite at all").unwrap();
        assert!(list(tmp.path(), Some(&junk)).states_problem.is_some());

        // A store that lacks the generation, or has it with another sha.
        let other = store(tmp.path(), &[("h", "active", SHA)]);
        let absent = list(tmp.path(), Some(&other));
        assert_eq!(absent.snapshots[0]["irreplaceable"], Value::Null);
    }

    #[test]
    fn a_generation_with_another_manifest_sha_is_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let exports = tmp.path().join("e");
        fs::create_dir(&exports).unwrap();
        write_snapshot(&exports, "snapshot-a", "g", "x.v5");
        let db = store(tmp.path(), &[("g", "evidence_only", "ff")]);
        let listing = list(&exports, Some(&db));
        assert_eq!(listing.snapshots[0]["generations"][0]["state"], "unknown");
        assert_eq!(listing.snapshots[0]["irreplaceable"], Value::Null);
    }

    #[test]
    fn damage_is_reported_with_its_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let exports = tmp.path().join("e");
        fs::create_dir(&exports).unwrap();
        let db = store(tmp.path(), &[("g", "active", SHA)]);

        // Missing manifest.
        fs::create_dir(exports.join("snapshot-nomanifest")).unwrap();
        // Truncated file, an unlisted file, a missing file.
        let dir = write_snapshot(&exports, "snapshot-size", "g", "x.v5");
        fs::write(dir.join("a.parquet"), b"123").unwrap();
        fs::write(dir.join("stray.bin"), b"x").unwrap();
        let dir = write_snapshot(&exports, "snapshot-gone", "g", "x.v5");
        fs::remove_file(dir.join("a.parquet")).unwrap();
        // Name that is not the id; invalid JSON.
        let dir = write_snapshot(&exports, "snapshot-renamed", "g", "x.v5");
        fs::rename(&dir, exports.join("snapshot-other")).unwrap();
        let dir = exports.join("snapshot-json");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("manifest.json"), b"{").unwrap();
        // Not directories.
        fs::write(exports.join("notes.txt"), b"hi").unwrap();
        fs::create_dir(exports.join(".tmp-123")).unwrap();

        let listing = list(&exports, Some(&db));
        let problems = |id: &str| -> String {
            let s = listing
                .snapshots
                .iter()
                .find(|s| s["snapshot_id"] == id)
                .unwrap();
            assert_eq!(s["status"], "damaged", "{id}");
            s["problems"].to_string()
        };
        assert!(problems("snapshot-nomanifest").contains("manifest.json cannot be read"));
        let size = problems("snapshot-size");
        assert!(size.contains("3 bytes on disk, manifest says 5"), "{size}");
        assert!(size.contains("stray.bin"), "{size}");
        assert!(problems("snapshot-gone").contains("listed in the manifest but missing"));
        assert!(problems("snapshot-other").contains("is not the manifest snapshot_id"));
        assert!(problems("snapshot-json").contains("not valid JSON"));
        let names: Vec<&str> = listing
            .unexpected_entries
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, [".tmp-123", "notes.txt"]);
        assert!(listing.problem_count() >= 7);
        assert!(listing.to_text().contains("DAMAGED"));
    }

    #[test]
    fn listing_changes_nothing_and_a_missing_root_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let exports = tmp.path().join("e");
        fs::create_dir(&exports).unwrap();
        let dir = write_snapshot(&exports, "snapshot-a", "g", "x.v5");
        fs::write(exports.join("loose"), b"x").unwrap();
        let db = store(tmp.path(), &[("g", "active", SHA)]);
        let snapshot = |root: &Path| -> Vec<(PathBuf, u64)> {
            let mut all = Vec::new();
            let mut stack = vec![root.to_path_buf()];
            while let Some(d) = stack.pop() {
                for e in fs::read_dir(&d).unwrap().flatten() {
                    let m = e.metadata().unwrap();
                    if m.is_dir() {
                        stack.push(e.path());
                    }
                    all.push((e.path(), m.len()));
                }
            }
            all.sort();
            all
        };
        let before = (snapshot(tmp.path()), fs::read(&db).unwrap());
        let _ = list(&exports, Some(&db));
        assert_eq!(before, (snapshot(tmp.path()), fs::read(&db).unwrap()));
        assert!(dir.is_dir());

        let missing = tmp.path().join("absent");
        let err = list_snapshots(&missing, None, &GenerationStates::NotConfigured).unwrap_err();
        assert!(err.to_string().starts_with("EXPORT_IO:"), "{err}");
        assert!(!missing.exists(), "listing must not create the root");
    }
}
