//! The v1 and v2 answers do not change when the page stops cloning the
//! manifest (VM1-41). Pages are compared with golden files that were written
//! by the code that still cloned it. Fabricated fixtures only.
//!
//! Rewrite the files with `UPDATE_GOLDEN=1 cargo test --test memory golden`.
use super::fake::*;
use serde_json::{Value, json};
use x_data_mcp::data_supply::{Access, query::Queries, wire::Query};

const ENTITIES: usize = 12;

fn query(cursor: Option<&str>, version: &str, selection: Value, filter: Value) -> Query {
    serde_json::from_value(json!({
        "dataset": "market.daily",
        "contract_version": version,
        "selection": selection,
        "filter": filter,
        "version": {"kind": "latest_per_entity"},
        "page_size": 5,
        "cursor": cursor,
    }))
    .unwrap()
}

fn cross_section() -> Value {
    json!({"kind": "cross_section", "start_date": DAY, "end_date": DAY})
}

fn panel() -> Value {
    json!({"kind": "daily", "start_date": DAY, "end_date": DAY})
}

fn batch(count: usize) -> Value {
    let entities: Vec<Value> = (0..count)
        .map(|n| {
            let s = security(n);
            json!({"exchange": s.exchange, "code": s.code})
        })
        .collect();
    json!({"kind": "batch", "entities": entities})
}

/// The page with the values that differ per run replaced.
fn stable(mut page: Value) -> String {
    fn walk(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, inner) in map.iter_mut() {
                    if ["snapshot_id", "snapshot_time", "query_time", "next_cursor"]
                        .contains(&key.as_str())
                        && !inner.is_null()
                    {
                        *inner = json!("<varies>");
                    } else {
                        walk(inner);
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(walk),
            _ => {}
        }
    }
    walk(&mut page);
    serde_json::to_string_pretty(&page).unwrap() + "\n"
}

fn check(name: &str, page: Value) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/memory/golden")
        .join(format!("{name}.json"));
    let actual = stable(page);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    assert!(
        actual == expected,
        "{name} differs from {path:?}:\n{actual}"
    );
}

fn queries() -> Queries<Fake> {
    Queries::new(Fake { entities: ENTITIES })
}

/// Pages of one query until it is complete: `(name, page)` per page.
fn v1_pages(selection: Value, filter: Value) -> Vec<Value> {
    let q = queries();
    let mut pages = vec![];
    let mut cursor: Option<String> = None;
    loop {
        let page = q
            .query(
                query(cursor.as_deref(), "v1", selection.clone(), filter.clone()),
                &Access::local(),
            )
            .unwrap();
        cursor = page.next_cursor.clone();
        pages.push(serde_json::to_value(&page).unwrap());
        if cursor.is_none() {
            return pages;
        }
    }
}

fn v2_pages(selection: Value, filter: Value) -> Vec<Value> {
    let q = queries();
    let mut pages = vec![];
    let mut cursor: Option<String> = None;
    loop {
        let page = q
            .query_v2(
                query(cursor.as_deref(), "v2", selection.clone(), filter.clone()),
                &Access::local(),
            )
            .unwrap();
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        pages.push(page);
        if cursor.is_none() {
            return pages;
        }
    }
}

fn pin(prefix: &str, pages: Vec<Value>, expected: usize) {
    assert_eq!(pages.len(), expected, "{prefix}: page count");
    for (n, page) in pages.into_iter().enumerate() {
        check(&format!("{prefix}-{n}"), page);
    }
}

#[test]
fn v1_batch_pages_keep_their_bytes() {
    // v1 has no `universe`; a 12-entity batch pages the same way.
    pin("v1-batch-pages", v1_pages(batch(12), panel()), 3);
}

#[test]
fn v1_batch_panel_page_keeps_its_bytes() {
    pin("v1-batch", v1_pages(batch(4), panel()), 1);
}

#[test]
fn v2_universe_pages_keep_their_bytes() {
    pin(
        "v2-universe",
        v2_pages(json!({"kind": "universe"}), cross_section()),
        3,
    );
}

#[test]
fn v2_batch_page_keeps_its_bytes() {
    pin("v2-batch", v2_pages(batch(4), panel()), 1);
}

#[test]
fn v2_universe_panel_pages_keep_their_bytes() {
    pin(
        "v2-universe-panel",
        v2_pages(json!({"kind": "universe"}), panel()),
        3,
    );
}
