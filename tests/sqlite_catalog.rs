use x_data_mcp::{
    catalog::*, market_storage::Error, sqlite_catalog::SqliteCatalog, sqlite_tasks::SqliteTasks,
};
fn fixture() -> CatalogSnapshot {
    let now = chrono::Utc::now();
    let source = "fixture:catalog".to_string();
    CatalogSnapshot {
        source: source.clone(),
        acquired_at: now,
        completeness: CompletenessEvidence::Complete { expected_count: 1 },
        records: vec![SecurityRecord {
            symbol: "600006".into(),
            name: "样例".into(),
            exchange: Exchange::Shanghai,
            security_type: SecurityType::AShare,
            source_security_id: "sh600006".into(),
            industry: IndustryInfo::Missing {
                reason: "fixture unknown".into(),
            },
            source,
            acquired_at: now,
        }],
    }
}
#[test]
fn real_catalog_fixture_preserves_missing_industry_and_rejects_tampering() {
    for sql in [
        "UPDATE catalog_members SET name='bad'",
        "UPDATE catalog_versions SET source='bad'",
        "UPDATE catalog_versions SET compat_json='{}'",
    ] {
        let d = tempfile::tempdir().unwrap();
        let tasks = SqliteTasks::new(d.path().join("db"));
        tasks.initialize().unwrap();
        let store = SqliteCatalog::new(tasks.clone());
        store.publish_fixture("v1", fixture(), None).unwrap();
        assert!(matches!(
            store.fixed("v1").unwrap().records[0].industry,
            IndustryInfo::Missing { .. }
        ));
        rusqlite::Connection::open(tasks.path())
            .unwrap()
            .execute_batch(sql)
            .unwrap();
        assert!(matches!(store.fixed("v1"), Err(Error::Integrity(_))));
    }
}
#[test]
fn failed_refresh_and_head_conflict_keep_old_catalog() {
    let d = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(d.path().join("db"));
    tasks.initialize().unwrap();
    let store = SqliteCatalog::new(tasks);
    store.publish_fixture("v1", fixture(), None).unwrap();
    let old = store.fixed("v1").unwrap();
    let mut gone = fixture();
    gone.records.clear();
    assert!(store.publish_fixture("v2", gone, Some("v1")).is_err());
    assert!(matches!(
        store.publish_fixture("v2", fixture(), None),
        Err(Error::Conflict(_))
    ));
    assert_eq!(store.fixed("v1").unwrap(), old);
}
#[test]
fn catalog_read_does_not_initialize() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("absent");
    assert!(SqliteCatalog::new(SqliteTasks::new(&p)).fixed("x").is_err());
    assert!(!p.exists());
}

fn official_snapshot() -> CatalogSnapshot {
    let mut snapshot = fixture();
    let source = "official-file:dcdf6b4989892667".to_string();
    snapshot.source = source.clone();
    for record in &mut snapshot.records {
        record.source = source.clone();
    }
    snapshot
}
#[test]
fn publish_official_binds_manifest_sha_and_keeps_expected_head() {
    let d = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(d.path().join("db"));
    tasks.initialize().unwrap();
    let store = SqliteCatalog::new(tasks);
    for bad_source in ["fixture:catalog", "official-file:", "official-file:XYZ"] {
        let mut snapshot = official_snapshot();
        snapshot.source = bad_source.into();
        for record in &mut snapshot.records {
            record.source = bad_source.into();
        }
        assert!(
            matches!(
                store.publish_official(snapshot, None, chrono::Utc::now()),
                Err(Error::Invalid(_))
            ),
            "{bad_source} must not pass the binding gate"
        );
    }
    assert!(store.selection_facts().unwrap().catalog_head.is_none());
    let id = store
        .publish_official(official_snapshot(), None, chrono::Utc::now())
        .unwrap();
    assert!(id.starts_with("catalog-"));
    assert_eq!(
        store.fixed(&id).unwrap().source,
        "official-file:dcdf6b4989892667"
    );
    // expected-head optimistic lock: a stale expected id conflicts, the
    // published head is retained.
    assert!(matches!(
        store.publish_official(official_snapshot(), Some("stale"), chrono::Utc::now()),
        Err(Error::Conflict(_))
    ));
    assert_eq!(
        store.selection_facts().unwrap().catalog_head.as_deref(),
        Some(id.as_str())
    );
}
