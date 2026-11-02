//! One audit line per finished MCP `tools/call` (XDM-167).
//!
//! The line goes to stderr, which systemd forwards to journald; stdout stays
//! protocol-only (`docs/design.md`: "stdout 仅协议输出，日志独立通道"). The fields
//! are fixed: `entry`, `tool`, `request_id` (when the caller supplied one),
//! `task_id` (when the call is bound to a task), `outcome` (`ok` or the tool's
//! stable error code) and `elapsed_ms`.
//!
//! What is deliberately absent: every other argument value (securities lists,
//! ranges, filters) and every result payload. Only the two identifiers the
//! audit rule names are read out of the arguments/result, and they are capped
//! and sanitized so one line always stays under [`MAX_LINE_BYTES`] — including
//! the high-frequency `query_data` calls.
//!
//! The line is a JSON object after a fixed prefix, so a journal filter is just
//! `journalctl -u x-data-mcp-data | grep 'XDM_CALL '` and downstream consumers
//! never parse free-form text.

use serde_json::{Map, Value};
use std::time::Duration;

/// Fixed marker so audit lines stay greppable in a journal that also carries
/// startup warnings and periodic progress lines.
pub const PREFIX: &str = "XDM_CALL ";
/// Ceiling for one line (task XDM-167 §2). [`ID_LIMIT`] is derived from it, so
/// the ceiling holds by construction for arbitrary input, not just for today's
/// identifiers.
pub const MAX_LINE_BYTES: usize = 300;
/// Cap for every string field (`entry`, `tool`, identifiers, `outcome`).
///
/// Worst-case line with all five fields at the cap and `elapsed_ms = u64::MAX`:
/// prefix 9 + `{` 1 + `"elapsed_ms":<20 digits>` 33 + entry 49 + tool 48 +
/// `request_id` 54 + `task_id` 51 + `outcome` 51 + `}` 1 = **297 B** ≤ 300.
/// All characters are filtered to an alphabet that JSON never escapes, so no
/// field can grow past its cap. With a real entry (`research`, 8 B) the widest
/// line is 267 B. Real identifiers still fit: `task-<32 hex>` is 37,
/// `daily-20261101-xdm139-activate` is 29, a UUID is 36.
const ID_LIMIT: usize = 38;

/// Emits one audit line. `entry` is the serving surface (`ops`, `research`,
/// `maint`, `acquire`; stdio/dev surfaces report `stdio`).
pub fn record(
    entry: &str,
    tool: &str,
    request_id: Option<&str>,
    task_id: Option<&str>,
    outcome: &str,
    elapsed: Duration,
) {
    let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    eprintln!(
        "{}",
        line(entry, tool, request_id, task_id, outcome, elapsed_ms)
    );
}

/// The audit line without the trailing newline (tests and [`record`]).
pub fn line(
    entry: &str,
    tool: &str,
    request_id: Option<&str>,
    task_id: Option<&str>,
    outcome: &str,
    elapsed_ms: u64,
) -> String {
    let mut object = Map::new();
    object.insert("entry".to_owned(), Value::String(clean(entry)));
    object.insert("tool".to_owned(), Value::String(clean(tool)));
    if let Some(request_id) = request_id {
        object.insert("request_id".to_owned(), Value::String(clean(request_id)));
    }
    if let Some(task_id) = task_id {
        object.insert("task_id".to_owned(), Value::String(clean(task_id)));
    }
    object.insert("outcome".to_owned(), Value::String(clean(outcome)));
    object.insert("elapsed_ms".to_owned(), Value::from(elapsed_ms));
    format!(
        "{PREFIX}{}",
        serde_json::to_string(&Value::Object(object)).unwrap_or_else(|_| "{}".to_owned())
    )
}

/// The two identifiers the audit line names, taken from the request arguments
/// and (as a fallback) the success result's top-level object. No other key is
/// ever read: `entities`, ranges, filters and every other argument stay out of
/// the log.
pub fn identifiers(
    args: Option<&Value>,
    result: Option<&Value>,
) -> (Option<String>, Option<String>) {
    let mut request_id = None;
    let mut task_id = None;
    for source in [args, result].into_iter().flatten() {
        let Some(object) = source.as_object() else {
            continue;
        };
        if request_id.is_none() {
            request_id = object
                .get("request_id")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        if task_id.is_none() {
            task_id = object
                .get("task_id")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
    }
    (request_id, task_id)
}

/// Caps the value and keeps it on one line: anything outside a conservative
/// identifier alphabet (including quotes, backslashes and control characters)
/// becomes `_`. This is what makes the byte ceiling hold for arbitrary input.
fn clean(value: &str) -> String {
    value
        .chars()
        .take(ID_LIMIT)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '-' | '@' | '?') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(raw: &str) -> Value {
        assert!(raw.starts_with(PREFIX), "{raw}");
        assert!(raw.len() <= MAX_LINE_BYTES, "{} bytes: {raw}", raw.len());
        serde_json::from_str(&raw[PREFIX.len()..]).unwrap()
    }

    #[test]
    fn line_carries_the_fixed_fields_and_stays_inside_the_byte_ceiling() {
        let raw = line(
            "research",
            "query_data",
            Some("0123456789abcdef0123456789abcdef0123"),
            Some("task-0123456789abcdef0123456789abcdef"),
            "invalid_retry_selection",
            18446744073709551615,
        );
        let parsed = parse(&raw);
        assert_eq!(parsed["entry"], "research");
        assert_eq!(parsed["tool"], "query_data");
        assert_eq!(parsed["outcome"], "invalid_retry_selection");
        assert_eq!(parsed["elapsed_ms"], u64::MAX);
        assert!(parsed["request_id"].is_string());
        assert!(parsed["task_id"].is_string());
        // Omitted, not empty, when the call has no such identifier.
        let plain = parse(&line("ops", "list_tasks", None, None, "ok", 1));
        assert!(plain.get("request_id").is_none());
        assert!(plain.get("task_id").is_none());
        parse(&line(
            "acquire",
            "refresh_exchange_sources",
            None,
            None,
            "ok",
            0,
        ));
    }

    #[test]
    fn every_field_at_its_cap_still_fits_the_byte_ceiling() {
        // `research` is the longest serving entry; the other four fields are
        // pushed to the cap and the elapsed value to u64::MAX, i.e. the widest
        // line this module can produce.
        let cap = "x".repeat(ID_LIMIT);
        let raw = line("research", &cap, Some(&cap), Some(&cap), &cap, u64::MAX);
        assert!(
            raw.len() <= MAX_LINE_BYTES,
            "{} bytes at cap {}: {}",
            raw.len(),
            ID_LIMIT,
            raw
        );
        let parsed = parse(&raw);
        assert_eq!(parsed["entry"], "research");
        assert_eq!(parsed["tool"].as_str().unwrap().len(), ID_LIMIT);
        // Even a hypothetical cap-length entry keeps the construction bound.
        let widest = line(&cap, &cap, Some(&cap), Some(&cap), &cap, u64::MAX);
        assert!(
            widest.len() <= MAX_LINE_BYTES,
            "{} bytes with every field at cap {}: {}",
            widest.len(),
            ID_LIMIT,
            widest
        );
    }

    #[test]
    fn arguments_other_than_the_two_identifiers_never_reach_the_line() {
        let args = json!({
            "contract_version": "v1",
            "request_id": "audit-success",
            "task_id": "task-audit",
            "entities": ["sh.600000", "sz.000001"],
            "selection": {"kind": "batch", "entities": [{"exchange": "sh", "code": "600006"}]}
        });
        let (request_id, task_id) = identifiers(Some(&args), None);
        let raw = line(
            "ops",
            "query_data",
            request_id.as_deref(),
            task_id.as_deref(),
            "absent",
            12,
        );
        assert!(raw.contains("audit-success"), "{raw}");
        assert!(raw.contains("task-audit"), "{raw}");
        for secret in ["600000", "000001", "600006", "entities", "selection"] {
            assert!(!raw.contains(secret), "{secret} leaked: {raw}");
        }
    }

    #[test]
    fn hostile_identifiers_are_capped_and_cannot_break_the_line() {
        let hostile = "x\"\n}={\\".repeat(40);
        let raw = line(
            "ops",
            "get_request",
            Some(&hostile),
            Some(&hostile),
            "ok",
            1,
        );
        assert!(!raw.contains('\n'), "line feed survived: {raw}");
        let parsed = parse(&raw);
        let id = parsed["request_id"].as_str().unwrap();
        assert!(id.len() <= ID_LIMIT, "{id}");
        assert!(
            id.chars()
                .all(|c| c.is_ascii_alphanumeric() || "._:/-@?".contains(c))
        );
    }

    #[test]
    fn a_result_task_id_is_used_when_the_arguments_only_name_the_request() {
        let args = json!({"request_id": "daily-run", "selection": {"kind": "batch", "entities": ["sh.600006"]}});
        let result = json!({"task_id": "task-returned", "status": "pending"});
        let (request_id, task_id) = identifiers(Some(&args), Some(&result));
        assert_eq!(request_id.as_deref(), Some("daily-run"));
        assert_eq!(task_id.as_deref(), Some("task-returned"));
    }
}
