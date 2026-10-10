//! Diagnostic stage timing (XP-025 E), disabled unless the process starts
//! with `X_DATA_MCP_STAGE_TRACE=1`. Each finished span / note is one
//! stderr line `XP025_STAGE {json}`; durations are inclusive wall time, so
//! nested stages overlap. Disabled cost: one `OnceLock` load per span.

use std::sync::OnceLock;
use std::time::Instant;

pub(crate) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("X_DATA_MCP_STAGE_TRACE").as_deref() == Ok("1"))
}

pub(crate) struct Span {
    stage: &'static str,
    started: Option<Instant>,
}

/// Starts a timed stage; the line is written when the guard drops.
pub(crate) fn span(stage: &'static str) -> Span {
    Span {
        stage,
        started: enabled().then(Instant::now),
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            emit(self.stage, Some(started.elapsed().as_nanos()), &[]);
        }
    }
}

/// Records a counter event (e.g. chain depth, bar counts) for a stage.
pub(crate) fn note(stage: &'static str, counts: &[(&'static str, u64)]) {
    if enabled() {
        emit(stage, None, counts);
    }
}

fn emit(stage: &str, ns: Option<u128>, counts: &[(&'static str, u64)]) {
    let mut line = serde_json::json!({
        "schema": "xp025-stage-trace-v1",
        "stage": stage,
        "tid": format!("{:?}", std::thread::current().id()),
        "unix_ns": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64),
    });
    if let Some(ns) = ns {
        line["inclusive_ns"] = serde_json::json!(ns as u64);
    }
    for (key, value) in counts {
        line[*key] = serde_json::json!(value);
    }
    eprintln!("XP025_STAGE {line}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_by_default_and_spans_are_inert() {
        // Tests never set the variable; a disabled span holds no timer.
        assert!(!enabled());
        let span = span("test.stage");
        assert!(span.started.is_none());
        note("test.note", &[("n", 1)]);
    }
}
