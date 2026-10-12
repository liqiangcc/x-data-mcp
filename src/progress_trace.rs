//! Private diagnostic timing, disabled unless explicitly enabled at startup.

use std::cell::{Cell, RefCell};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Instant;

#[derive(Clone, Copy)]
#[allow(dead_code)] // chain-era probes; names are index-coupled to NAMES
pub(crate) enum Stage {
    TaskAssembly,
    Reconstruct,
    VerifyNode,
    VerifyHeader,
    SegmentSql,
    SegmentDigest,
    VerifyVersions,
    VerifyRanges,
    VerifyYearly,
    WireBudget,
}

const NAMES: [&str; 10] = [
    "task_assembly",
    "reconstruct",
    "verify_node",
    "verify_header",
    "segment_sql",
    "segment_digest",
    "verify_versions",
    "verify_ranges",
    "verify_yearly",
    "wire_budget",
];

#[derive(Default)]
struct State {
    id: u64,
    totals_ns: [u128; 10],
    calls: [u64; 10],
    segment_rows: u64,
    active: usize,
    valid: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static NEXT_ID: Cell<u64> = const { Cell::new(0) };
}

pub(crate) struct Trace {
    started: Option<Instant>,
    success: bool,
    local: PhantomData<Rc<()>>,
}

impl Trace {
    pub(crate) fn begin() -> Self {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        let enabled = *ENABLED
            .get_or_init(|| std::env::var("X_DATA_MCP_PROGRESS_STAGE_TRACE").as_deref() == Ok("1"));
        Self::begin_enabled(enabled)
    }

    fn begin_enabled(enabled: bool) -> Self {
        let active = enabled
            && STATE.with(|state| {
                let mut state = state.borrow_mut();
                if let Some(previous) = state.as_mut() {
                    previous.valid = false;
                    return false;
                }
                let Some(id) = NEXT_ID.with(|next| {
                    let id = next.get().checked_add(1)?;
                    next.set(id);
                    Some(id)
                }) else {
                    return false;
                };
                *state = Some(State {
                    id,
                    valid: true,
                    ..State::default()
                });
                true
            });
        Self {
            started: active.then(Instant::now),
            success: false,
            local: PhantomData,
        }
    }

    pub(crate) fn finish(mut self, success: bool) {
        self.success = success;
    }
}

impl Drop for Trace {
    fn drop(&mut self) {
        let Some(started) = self.started else {
            return;
        };
        let total_ns = started.elapsed().as_nanos();
        let Some(state) = STATE.with(|state| state.borrow_mut().take()) else {
            return;
        };
        let stages: serde_json::Map<String, serde_json::Value> = NAMES
            .iter()
            .enumerate()
            .map(|(i, name)| {
                (
                    (*name).to_owned(),
                    serde_json::json!({
                        "inclusive_ns": state.totals_ns[i],
                        "calls": state.calls[i],
                    }),
                )
            })
            .collect();
        eprintln!(
            "XP019_STAGE_TRACE {}",
            serde_json::json!({
                "schema": "xp019-stage-trace-v1",
                "success": self.success,
                "valid": state.valid && state.active == 0,
                "application_ns": total_ns,
                "segment_rows": state.segment_rows,
                "stages": stages,
                "semantics": "inclusive_wall_ns; nested stages overlap; not CPU",
            })
        );
    }
}

pub(crate) struct Span {
    stage: Stage,
    id: Option<u64>,
    started: Option<Instant>,
    local: PhantomData<Rc<()>>,
}

impl Span {
    pub(crate) fn enter(stage: Stage) -> Self {
        let id = STATE.with(|state| {
            let mut state = state.borrow_mut();
            let state = state.as_mut()?;
            let Some(active) = state.active.checked_add(1) else {
                state.valid = false;
                return None;
            };
            state.active = active;
            Some(state.id)
        });
        Self {
            stage,
            id,
            started: id.map(|_| Instant::now()),
            local: PhantomData,
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some(started) = self.started else {
            return;
        };
        let elapsed = started.elapsed().as_nanos();
        STATE.with(|state| {
            if let Some(state) = state.borrow_mut().as_mut() {
                if Some(state.id) != self.id {
                    state.valid = false;
                    return;
                }
                let i = self.stage as usize;
                match (
                    state.totals_ns[i].checked_add(elapsed),
                    state.calls[i].checked_add(1),
                    state.active.checked_sub(1),
                ) {
                    (Some(total), Some(calls), Some(active)) => {
                        state.totals_ns[i] = total;
                        state.calls[i] = calls;
                        state.active = active;
                    }
                    _ => state.valid = false,
                }
            }
        });
    }
}

#[allow(dead_code)] // caller retired with the segment store
pub(crate) fn segment_rows(rows: usize) {
    STATE.with(|state| {
        if let Some(state) = state.borrow_mut().as_mut() {
            match u64::try_from(rows)
                .ok()
                .and_then(|n| state.segment_rows.checked_add(n))
            {
                Some(sum) => state.segment_rows = sum,
                None => state.valid = false,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inactive_trace_retains_nothing() {
        let trace = Trace::begin_enabled(false);
        let span = Span::enter(Stage::SegmentSql);
        segment_rows(512);
        drop(span);
        trace.finish(true);
        STATE.with(|state| assert!(state.borrow().is_none()));
    }

    #[test]
    fn active_trace_counts_scopes_and_cleans_on_error() {
        let trace = Trace::begin_enabled(true);
        {
            let _parent = Span::enter(Stage::Reconstruct);
            let _child = Span::enter(Stage::SegmentSql);
            segment_rows(512);
        }
        STATE.with(|state| {
            let state = state.borrow();
            let state = state.as_ref().unwrap();
            assert!(state.valid);
            assert_eq!(state.active, 0);
            assert_eq!(state.calls[Stage::Reconstruct as usize], 1);
            assert_eq!(state.calls[Stage::SegmentSql as usize], 1);
            assert_eq!(state.segment_rows, 512);
            assert!(
                state.totals_ns[Stage::Reconstruct as usize]
                    >= state.totals_ns[Stage::SegmentSql as usize]
            );
        });
        trace.finish(false);
        STATE.with(|state| assert!(state.borrow().is_none()));
    }

    #[test]
    fn escaped_span_invalidates_instead_of_polluting_another_trace() {
        let first = Trace::begin_enabled(true);
        let escaped = Span::enter(Stage::SegmentSql);
        first.finish(false);
        let second = Trace::begin_enabled(true);
        drop(escaped);
        STATE.with(|state| {
            let state = state.borrow();
            let state = state.as_ref().unwrap();
            assert!(!state.valid);
            assert_eq!(state.active, 0);
            assert_eq!(state.calls, [0; 10]);
        });
        second.finish(false);
        STATE.with(|state| assert!(state.borrow().is_none()));
    }
}
