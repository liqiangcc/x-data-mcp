//! XDM-161: a successful commit refreshes every generation's derived-fact
//! summaries, so a later daily run that re-points older rows to its own
//! binding cannot leave `replay --scope generation_facts --verify-only` in
//! `drift`. The tests below drive the same tables the daily commit writes
//! (the `UPDATE … source_binding_digest` in `series_commit.rs`) plus the
//! writer hook, then compare with the real replay and the retire gate.
use super::*;
use crate::compact_market::{Digest, EvidenceKind, SourceBinding};
use crate::generation_gate::{
    ActivationRecord, KIND_ADJUSTMENT_EVENTS, KIND_CAPITAL_EVENTS, KIND_DAILY_SERIES, RetireGate,
    facts_summary, industry_fact_rows_from_store,
};
use crate::market_storage::Error;
use crate::source_store::replay::{ReplayRequest, Scope, verify};
use crate::source_store::{
    INCON_PARSER_VERSION, INCON_SOURCE_ID, Registration, SourceStore, StoreKind,
    TDXHY_PARSER_VERSION, TDXHY_SOURCE_ID,
};
use crate::sqlite_market::compact_repo::SqliteCompactMarket;
use crate::sqlite_market::evidence_store::{put_evidence, put_source_binding};
use crate::sqlite_tasks::SqliteTasks;
use rusqlite::{Connection, params};
use serde_json::json;
use std::path::{Path, PathBuf};

const GEN1: Digest = [0x0b; 32];
const GEN2: Digest = [0x0c; 32];

struct Fixture {
    _dir: tempfile::TempDir,
    db: PathBuf,
    store: SourceStore,
    gen1: String,
    gen2: String,
}

fn fixture() -> Fixture {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("market.sqlite");
    SqliteTasks::new(&db).initialize_compact().unwrap();
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
    let store = SourceStore::new(dir.path().join("source-store"));
    let gen1 = hex(&GEN1);
    let gen2 = hex(&GEN2);
    // Only the first generation has source-store evidence. The second one is
    // the daily run that re-points some rows; the gate test below is about the
    // generation that lost its bindings, not about the new one's evidence.
    for (source_id, parser, bytes) in [
        (
            TDXHY_SOURCE_ID,
            TDXHY_PARSER_VERSION,
            b"1|600006|T090302|||X6102\r\n".as_slice(),
        ),
        (
            INCON_SOURCE_ID,
            INCON_PARSER_VERSION,
            b"T090302|bank\r\n".as_slice(),
        ),
    ] {
        let mut registration = Registration::new(
            source_id,
            StoreKind::GenerationMember,
            format!("generation:{gen1}"),
            "2026-11-01T00:00:00Z",
        );
        registration.parser_version = Some(parser.to_string());
        store.register(bytes, &registration).unwrap();
    }
    Fixture {
        _dir: dir,
        db,
        store,
        gen1,
        gen2,
    }
}

/// The writer hook under test: open, run, close (the replay must be able to
/// take the database lease afterwards).
fn with_repo<T>(db: &Path, f: impl FnOnce(&SqliteCompactMarket) -> T) -> T {
    let repo = SqliteCompactMarket::open(db).unwrap();
    let value = f(&repo);
    drop(repo);
    value
}

fn binding(c: &Connection, id: &str, manifest: Digest) -> Result<Digest, Error> {
    let evidence = put_evidence(
        c,
        EvidenceKind::Generation,
        &json!({
            "generation_id": id,
            "snapshot_inputs": [{
                "role": "daily",
                "member": "sh.600006.day",
                "archive_sha256": "12".repeat(32),
                "member_sha256": "cd".repeat(32)
            }]
        }),
    )?;
    put_source_binding(
        c,
        &SourceBinding {
            generation_id: id.into(),
            generation_manifest_sha256: manifest,
            day_member_sha256: [0x0d; 32],
            security_event_sha256: [0x0e; 32],
            evidence_digest: evidence,
        },
    )
}

fn row_evidence(c: &Connection) -> Result<Digest, Error> {
    put_evidence(
        c,
        EvidenceKind::Generation,
        &json!({"fixture": "generation_facts refresh"}),
    )
}

/// One `market_series` row, the shape `commit_series_inner` writes.
fn insert_series(
    c: &Connection,
    series_key: i64,
    entity: &str,
    digest_byte: u8,
    binding: &Digest,
    evidence: &Digest,
) -> Result<(), Error> {
    let content = [digest_byte; 32];
    c.execute(
        "INSERT INTO market_series(series_key,entity,period,adjustment,\
         instrument_class,price_scale,version_id,content_digest,digest_format,\
         record_count,first_day,last_day,coverage_status,source_binding_digest,\
         evidence_digest,last_event_seq,created_at,updated_at) \
         VALUES(?1,?2,1,1,0,2,?3,?4,'series-fold-v2',1,0,0,1,?5,?6,1,\
         '2026-11-01T00:00:00Z','2026-11-01T00:00:00Z')",
        params![
            series_key,
            entity,
            hex(&content),
            content.as_slice(),
            binding.as_slice(),
            evidence.as_slice(),
        ],
    )
    .map_err(storage)?;
    Ok(())
}

fn insert_capital(c: &Connection, binding: &Digest, evidence: &Digest) -> Result<(), Error> {
    c.execute(
        "INSERT INTO capital_events(exchange,code,ordinal,effective_day,category,\
         float_before_bits,total_before_bits,float_after_bits,total_after_bits,\
         float_after,total_after,prev_is_lagged,suspicious,known_at,\
         source_binding_digest,evidence_digest) \
         VALUES('sh','600006',0,0,2,1,1,1,1,10,10,1,0,'2026-11-01T00:00:00Z',?1,?2)",
        params![binding.as_slice(), evidence.as_slice()],
    )
    .map_err(storage)?;
    Ok(())
}

fn insert_adjustment(c: &Connection, binding: &Digest, evidence: &Digest) -> Result<(), Error> {
    c.execute(
        "INSERT INTO adjustment_events(entity,ordinal,effective_day,category,\
         c_bits_1,c_bits_2,c_bits_3,c_bits_4,c1,c2,c3,c4,source_binding_digest,\
         evidence_digest,known_at) \
         VALUES('sh.600006',0,0,1,1,1,1,1,'1','2','3','4',?1,?2,\
         '2026-11-01T00:00:00Z')",
        params![binding.as_slice(), evidence.as_slice()],
    )
    .map_err(storage)?;
    Ok(())
}

fn facts_of(db: &Path, generation: &str) -> Vec<GenerationFact> {
    with_repo(db, |repo| {
        repo.read_generation_facts()
            .unwrap()
            .into_iter()
            .filter(|fact| fact.generation_manifest_sha256 == generation)
            .collect()
    })
}

fn fact<'a>(facts: &'a [GenerationFact], kind: &str) -> Option<&'a GenerationFact> {
    facts.iter().find(|fact| fact.fact_kind == kind)
}

fn assert_replay_match(store: &SourceStore, db: &Path) {
    let report = verify(
        store,
        &ReplayRequest::new(Some(Scope::GenerationFacts), true, Some(db)),
    )
    .unwrap();
    assert_eq!(report.status, "ok", "{report:?}");
    assert_eq!(report.scopes[0].database, Some("match"), "{report:?}");
}

fn assert_replay_drift(store: &SourceStore, db: &Path) {
    let report = verify(
        store,
        &ReplayRequest::new(Some(Scope::GenerationFacts), true, Some(db)),
    )
    .unwrap();
    assert_eq!(report.status, "drift", "{report:?}");
    assert_eq!(report.scopes[0].database, Some("drift"), "{report:?}");
    assert!(!report.scopes[0].drift.is_empty(), "{report:?}");
}

#[test]
fn commit_refreshes_every_generation_and_orphaned_facts_survive_the_gate() {
    let fixture = fixture();
    let Fixture {
        _dir,
        db,
        store,
        gen1,
        gen2,
    } = fixture;

    // First generation commits: one series, one capital event, one adjustment
    // event, all bound to GEN1, plus the industry rows the item registers.
    with_repo(&db, |repo| {
        repo.tx(|c| {
            let first = binding(c, "gen-1", GEN1)?;
            let evidence = row_evidence(c)?;
            insert_series(c, 1, "sh.600006", 0xa1, &first, &evidence)?;
            insert_capital(c, &first, &evidence)?;
            insert_adjustment(c, &first, &evidence)?;
            Ok(())
        })
        .unwrap();
        let industry = industry_fact_rows_from_store(&store).unwrap();
        assert_eq!(industry.len(), 3);
        repo.record_generation_facts(&industry).unwrap();
        repo.refresh_derived_generation_facts().unwrap();
    });

    let first = facts_of(&db, &gen1);
    assert_eq!(first.len(), 6, "{first:#?}");
    for kind in [
        KIND_DAILY_SERIES,
        KIND_CAPITAL_EVENTS,
        KIND_ADJUSTMENT_EVENTS,
    ] {
        let registered = fact(&first, kind).unwrap_or_else(|| panic!("missing {kind}"));
        assert_ne!(
            registered.fact_digest,
            content_digest(kind, &GEN1, &[]),
            "{kind} should still reference live rows"
        );
    }
    assert_replay_match(&store, &db);

    // Second generation: a partial daily run re-points the series row to GEN2
    // (the `UPDATE market_series … source_binding_digest` in series_commit)
    // and appends one more series under GEN2. Capital/adjustment stay on GEN1.
    with_repo(&db, |repo| {
        repo.tx(|c| {
            let second = binding(c, "gen-2", GEN2)?;
            let evidence = row_evidence(c)?;
            c.execute(
                "UPDATE market_series SET source_binding_digest=?1,\
                 last_event_seq=last_event_seq+1,updated_at='2026-11-02T00:00:00Z' \
                 WHERE series_key=1",
                params![second.as_slice()],
            )
            .map_err(storage)?;
            insert_series(c, 2, "sh.600007", 0xa2, &second, &evidence)?;
            Ok(())
        })
        .unwrap();
    });
    // Without the refresh the stored GEN1 daily summary is stale: this is the
    // drift XDM-161 fixes.
    assert_replay_drift(&store, &db);

    with_repo(&db, |repo| {
        repo.refresh_derived_generation_facts().unwrap();
    });
    let first = facts_of(&db, &gen1);
    let second = facts_of(&db, &gen2);
    assert_eq!(
        fact(&first, KIND_DAILY_SERIES).unwrap().fact_digest,
        content_digest(KIND_DAILY_SERIES, &GEN1, &[]),
        "GEN1 no longer references any daily series"
    );
    assert_ne!(
        fact(&first, KIND_CAPITAL_EVENTS).unwrap().fact_digest,
        content_digest(KIND_CAPITAL_EVENTS, &GEN1, &[]),
    );
    assert_ne!(
        fact(&second, KIND_DAILY_SERIES).unwrap().fact_digest,
        content_digest(KIND_DAILY_SERIES, &GEN2, &[]),
    );
    assert!(
        fact(&second, KIND_CAPITAL_EVENTS).is_none(),
        "GEN2 wrote no capital events, so it must not invent the fact"
    );
    assert_replay_match(&store, &db);

    // The rest of the daily run moves the remaining rows. GEN1 now holds no
    // binding at all, but its fact rows stay with the empty-set digest.
    with_repo(&db, |repo| {
        repo.tx(|c| {
            let second = binding(c, "gen-2", GEN2)?;
            c.execute(
                "UPDATE capital_events SET source_binding_digest=?1",
                params![second.as_slice()],
            )
            .map_err(storage)?;
            c.execute(
                "UPDATE adjustment_events SET source_binding_digest=?1",
                params![second.as_slice()],
            )
            .map_err(storage)?;
            Ok(())
        })
        .unwrap();
        repo.refresh_derived_generation_facts().unwrap();
    });

    let first = facts_of(&db, &gen1);
    assert_eq!(first.len(), 6, "an orphaned generation keeps its rows");
    for kind in [
        KIND_DAILY_SERIES,
        KIND_CAPITAL_EVENTS,
        KIND_ADJUSTMENT_EVENTS,
    ] {
        assert_eq!(
            fact(&first, kind).unwrap().fact_digest,
            content_digest(kind, &GEN1, &[]),
            "{kind} no longer references live rows"
        );
    }
    assert_replay_match(&store, &db);

    // The retired generation still passes the real gate: row existence is
    // what it checks, and the source-store evidence is untouched.
    let check = RetireGate::new(store.clone(), &db)
        .check(&gen1, ActivationRecord::Activated)
        .unwrap();
    assert!(check.passes(), "{check:?}");

    // A second refresh is an idempotent upsert: same rows, same summary.
    let before = {
        let facts = with_repo(&db, |repo| repo.read_generation_facts().unwrap());
        (facts.len(), facts_summary(&facts))
    };
    with_repo(&db, |repo| {
        repo.refresh_derived_generation_facts().unwrap();
    });
    let after = {
        let facts = with_repo(&db, |repo| repo.read_generation_facts().unwrap());
        (facts.len(), facts_summary(&facts))
    };
    assert_eq!(before, after);
    assert_replay_match(&store, &db);
}
