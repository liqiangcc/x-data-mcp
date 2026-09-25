use crate::support_metrics as fixture;
use x_data_mcp::{
    data_supply::Access,
    metrics::{service::Metrics, wire::*},
    sqlite_market::SqliteMarket,
    sqlite_metrics::SqliteMetrics,
    sqlite_tasks::SqliteTasks,
};
#[test]
fn verified_sqlite_completion_evidence_is_required_and_computation_is_read_only() {
    for coverage in [false, true] {
        let d = tempfile::tempdir().unwrap();
        let t = SqliteTasks::new(d.path().join("db"));
        t.initialize().unwrap();
        let store = SqliteMarket::new(t.clone());
        let reference = fixture::publish(&store, coverage);
        let compute = Metrics {
            repository: SqliteMetrics {
                market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(store),
            },
        };
        let request: Compute = serde_json::from_value(fixture::request(reference)).unwrap();
        let c = rusqlite::Connection::open(t.path()).unwrap();
        let before: i64 = c
            .query_row("SELECT count(*) FROM tasks", [], |r| r.get(0))
            .unwrap();
        let first = compute.compute(request.clone(), &Access::local()).unwrap();
        assert_eq!(first, compute.compute(request, &Access::local()).unwrap());
        if coverage {
            assert!(matches!(
                first.result,
                Outcome::Computed {
                    run_length: 3,
                    matches: Some(true),
                    ..
                }
            ));
        } else {
            assert!(matches!(
                first.result,
                Outcome::NotComputable {
                    reason: Reason::UnknownCompletion,
                    ..
                }
            ));
        }
        assert_eq!(
            before,
            c.query_row("SELECT count(*) FROM tasks", [], |r| r.get::<_, i64>(0))
                .unwrap()
        );
    }
}
#[test]
fn fixed_digest_or_projection_damage_never_switches_versions() {
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    let store = SqliteMarket::new(t.clone());
    let reference = fixture::publish(&store, true);
    let compute = Metrics {
        repository: SqliteMetrics {
            market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(store),
        },
    };
    let request: Compute = serde_json::from_value(fixture::request(reference)).unwrap();
    let mut wrong = request.clone();
    wrong.inputs[0].version_ref.integrity_sha256 = "f".repeat(64);
    assert!(compute.compute(wrong, &Access::local()).is_err());
    rusqlite::Connection::open(t.path())
        .unwrap()
        .execute("UPDATE yearly_bars SET close='0'", [])
        .unwrap();
    assert!(compute.compute(request, &Access::local()).is_err());
}
#[test]
fn compute_unknown_fields_mode_latest_and_budget_are_rejected() {
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("missing"));
    let compute = Metrics {
        repository: SqliteMetrics {
            market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(SqliteMarket::new(t)),
        },
    };
    let example = x_data_mcp::data_supply::registry::entries()
        .into_iter()
        .find(|e| e.name == "compute_metrics")
        .unwrap();
    let mut unknown = example.example.clone();
    unknown["options"] = serde_json::json!({});
    assert!(serde_json::from_value::<Compute>(unknown).is_err());
    let mut task = example.example.clone();
    task["execution"] = serde_json::json!("task");
    assert!(serde_json::from_value::<Compute>(task).is_err());
    let mut r: Compute = serde_json::from_value(example.example).unwrap();
    r.inputs[0].version_ref.version_id = "latest".into();
    assert!(compute.compute(r.clone(), &Access::local()).is_err());
    r.inputs[0].version_ref.version_id = "fixed".into();
    r.range.start_year = 1;
    r.range.end_year = 1001;
    assert!(compute.compute(r, &Access::local()).is_err());
}
#[test]
fn sqlite_unfinished_tail_honors_both_explicit_modes() {
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    let store = SqliteMarket::new(t);
    let reference = fixture::publish_at(&store, true, true);
    let compute = Metrics {
        repository: SqliteMetrics {
            market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(store),
        },
    };
    let mut request: Compute = serde_json::from_value(fixture::request(reference)).unwrap();
    assert!(matches!(
        compute
            .compute(request.clone(), &Access::local())
            .unwrap()
            .result,
        Outcome::Computed {
            run_length: 2,
            matches: None,
            provisional: false,
            ..
        }
    ));
    request.metrics[0].parameters.completed_only = false;
    assert!(matches!(
        compute.compute(request, &Access::local()).unwrap().result,
        Outcome::Computed {
            run_length: 3,
            matches: Some(true),
            provisional: true,
            ..
        }
    ));
}
#[test]
fn production_yearly_completion_derives_from_embedded_derivation_evidence() {
    // Issue #225 D1=A/D2=C: certified base coverage + finished year reads
    // Complete; certified-but-open reads Incomplete; uncertified years and
    // absent evidence fail closed to Unknown.
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    let store = SqliteMarket::new(t);
    let reference = fixture::publish_derived(
        &store,
        Some(fixture::derivation_evidence(
            "Complete",
            &[2020, 2021, 2022, 2023],
        )),
        false,
    );
    let compute = Metrics {
        repository: SqliteMetrics {
            market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(store),
        },
    };
    let request: Compute = serde_json::from_value(fixture::request(reference)).unwrap();
    assert!(matches!(
        compute.compute(request, &Access::local()).unwrap().result,
        Outcome::Computed {
            run_length: 3,
            matches: Some(true),
            ..
        }
    ));
}
#[test]
fn production_yearly_completion_fail_closed_on_uncertified_or_unfinished() {
    for (evidence, expected) in [
        // Year outside the verifier-certified set: unknown.
        (
            Some(fixture::derivation_evidence(
                "Complete",
                &[2020, 2021, 2022],
            )),
            Reason::UnknownCompletion,
        ),
        // Verifier explicitly reported partial coverage: incomplete.
        (
            Some(fixture::derivation_evidence(
                "Incomplete",
                &[2020, 2021, 2022, 2023],
            )),
            Reason::IncompletePeriod,
        ),
        // No embedded derivation evidence at all: fail closed.
        (None, Reason::UnknownCompletion),
    ] {
        let d = tempfile::tempdir().unwrap();
        let t = SqliteTasks::new(d.path().join("db"));
        t.initialize().unwrap();
        let store = SqliteMarket::new(t);
        let reference = fixture::publish_derived(&store, evidence, false);
        let compute = Metrics {
            repository: SqliteMetrics {
                market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(store),
            },
        };
        let request: Compute = serde_json::from_value(fixture::request(reference)).unwrap();
        assert!(matches!(
            compute.compute(request, &Access::local()).unwrap().result,
            Outcome::NotComputable { reason, .. } if reason == expected
        ));
    }
    // Certified but still-open tail year reads Incomplete: completed_only
    // excludes it and computes over the finished certified years; the
    // non-completed mode keeps it as the provisional end year.
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    let store = SqliteMarket::new(t);
    let reference = fixture::publish_derived(
        &store,
        Some(fixture::derivation_evidence(
            "Complete",
            &[2020, 2021, 2022, 2023],
        )),
        true,
    );
    let compute = Metrics {
        repository: SqliteMetrics {
            market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(store),
        },
    };
    let mut request: Compute = serde_json::from_value(fixture::request(reference)).unwrap();
    assert!(matches!(
        compute
            .compute(request.clone(), &Access::local())
            .unwrap()
            .result,
        Outcome::Computed {
            run_length: 2,
            provisional: false,
            ..
        }
    ));
    request.metrics[0].parameters.completed_only = false;
    assert!(matches!(
        compute.compute(request, &Access::local()).unwrap().result,
        Outcome::Computed {
            run_length: 3,
            provisional: true,
            ..
        }
    ));
}
#[test]
fn malformed_embedded_derivation_evidence_is_integrity_error() {
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    let store = SqliteMarket::new(t);
    let reference = fixture::publish_derived(
        &store,
        Some(serde_json::json!({"years": "not-an-array"})),
        false,
    );
    let compute = Metrics {
        repository: SqliteMetrics {
            market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(store),
        },
    };
    let request: Compute = serde_json::from_value(fixture::request(reference)).unwrap();
    assert!(compute.compute(request, &Access::local()).is_err());
}
