//! The `tail`/`full` request plan (issue-446 §5.1/§5.5, D-66 Q2/Q3).
//!
//! The plan is derived from the adapters' static `plans()` and window helpers,
//! never from a second URL list: a refresh run must request the same
//! fingerprints the deriving item requests, otherwise the run's members do not
//! describe the bytes that item would select.

use chrono::NaiveDate;

use crate::exchange_source::{AcquisitionPlan, Segment, SourceId, quarter_segments};
use crate::official_security::{a1a2, a3, risk_warnings};

use super::wire::{PlannedUnit, RefreshMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanClass {
    Window,
    Table,
    /// A whole table whose daily observation is page 1 only; a change triggers
    /// a full table acquisition in the same run (D-66 Q3).
    Probe,
}

impl PlanClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::Table | Self::Probe => "table",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SourcePlan {
    pub source_id: SourceId,
    pub class: PlanClass,
    /// `(label, plan)`: several plans belong to one source (SSE main/star
    /// boards, SZSE main/STAR delisting, two bulletin title filters).
    pub plans: Vec<(String, AcquisitionPlan)>,
    /// Present for window sources; empty for tables/probes.
    pub windows: Vec<Segment>,
    /// Window bounds parameter names (ignored for tables).
    pub from_param: &'static str,
    pub to_param: &'static str,
}

impl SourcePlan {
    pub fn planned_units(&self) -> usize {
        if self.windows.is_empty() {
            self.plans.len()
        } else {
            self.plans.len() * self.windows.len()
        }
    }

    pub fn planned_windows(&self) -> usize {
        self.windows.len()
    }

    pub fn planned(&self) -> Vec<PlannedUnit> {
        let mut units = Vec::new();
        if self.windows.is_empty() {
            for (label, _) in &self.plans {
                units.push(PlannedUnit {
                    source_id: self.source_id.as_str().to_string(),
                    label: label.clone(),
                    kind: self.class.as_str().to_string(),
                    window_start: None,
                    window_end: None,
                });
            }
            return units;
        }
        for window in &self.windows {
            for (label, _) in &self.plans {
                units.push(PlannedUnit {
                    source_id: self.source_id.as_str().to_string(),
                    label: label.clone(),
                    kind: self.class.as_str().to_string(),
                    window_start: Some(window.start().to_string()),
                    window_end: Some(window.end().to_string()),
                });
            }
        }
        units
    }
}

/// Build the run's plan for `mode`, restricted to `only` when non-empty.
///
/// Unknown names are refused instead of silently ignored: a typo in a scoped
/// refresh must not look like a successful seven-source run.
pub fn build(
    mode: RefreshMode,
    today: NaiveDate,
    only: &[String],
) -> Result<Vec<SourcePlan>, String> {
    let mut selected = Vec::new();
    for name in only {
        let Some(source) = SourceId::parse(name) else {
            let known: Vec<&str> = SourceId::ALL.iter().map(|id| id.as_str()).collect();
            return Err(format!(
                "unknown source {name:?}; known sources: {}",
                known.join(", ")
            ));
        };
        if selected.contains(&source) {
            return Err(format!("source {name:?} is listed twice"));
        }
        selected.push(source);
    }
    if selected.is_empty() {
        selected.extend(SourceId::ALL);
    }

    let a1a2_plans = a1a2::plans();
    let risk_plans = risk_warnings::plans();
    let mut plans = Vec::new();
    for source in selected {
        let plan = match source {
            SourceId::SseListing | SourceId::SseDelisting => {
                let plans = a1a2_plans
                    .iter()
                    .filter(|(_, plan)| plan.source_id == source)
                    .map(|(kind, plan)| (kind.label().to_string(), plan.clone()))
                    .collect::<Vec<_>>();
                SourcePlan {
                    source_id: source,
                    class: PlanClass::Table,
                    plans,
                    windows: Vec::new(),
                    from_param: "",
                    to_param: "",
                }
            }
            SourceId::SzseListing | SourceId::SzseDelisting => {
                let plans = a1a2_plans
                    .iter()
                    .filter(|(_, plan)| plan.source_id == source)
                    .map(|(kind, plan)| (kind.label().to_string(), plan.clone()))
                    .collect::<Vec<_>>();
                let class = if source == SourceId::SzseListing && mode == RefreshMode::Tail {
                    PlanClass::Probe
                } else {
                    PlanClass::Table
                };
                SourcePlan {
                    source_id: source,
                    class,
                    plans,
                    windows: Vec::new(),
                    from_param: "",
                    to_param: "",
                }
            }
            SourceId::SzseShortName => {
                let plans = risk_plans
                    .iter()
                    .filter(|(_, plan)| plan.source_id == source)
                    .map(|(kind, plan)| (kind.label().to_string(), plan.clone()))
                    .collect::<Vec<_>>();
                let class = if mode == RefreshMode::Tail {
                    PlanClass::Probe
                } else {
                    PlanClass::Table
                };
                SourcePlan {
                    source_id: source,
                    class,
                    plans,
                    windows: Vec::new(),
                    from_param: "",
                    to_param: "",
                }
            }
            SourceId::SseBulletin => {
                let plans = risk_plans
                    .iter()
                    .filter(|(_, plan)| plan.source_id == source)
                    .map(|(kind, plan)| (kind.label().to_string(), plan.clone()))
                    .collect::<Vec<_>>();
                let start = NaiveDate::from_ymd_opt(2000, 1, 1)
                    .expect("2000-01-01 is a valid archive start");
                let mut windows = quarter_segments(start, today)
                    .map_err(|error| format!("bulletin windows: {error}"))?;
                if mode == RefreshMode::Tail {
                    trim_tail(&mut windows);
                }
                SourcePlan {
                    source_id: source,
                    class: PlanClass::Window,
                    plans,
                    windows,
                    from_param: "START_DATE",
                    to_param: "END_DATE",
                }
            }
            SourceId::SseSuspension => {
                let mut windows =
                    a3::windows(today).map_err(|error| format!("suspension windows: {error}"))?;
                if mode == RefreshMode::Tail {
                    trim_tail(&mut windows);
                }
                SourcePlan {
                    source_id: source,
                    class: PlanClass::Window,
                    plans: vec![("sse_suspension".to_string(), a3::plan())],
                    windows,
                    from_param: a3::FROM_PARAM,
                    to_param: a3::TO_PARAM,
                }
            }
        };
        plans.push(plan);
    }
    Ok(plans)
}

/// Keep the open window and the previous natural quarter (D-66 Q2, L=1).
fn trim_tail(windows: &mut Vec<Segment>) {
    if windows.len() > 2 {
        let keep_from = windows.len() - 2;
        windows.drain(..keep_from).for_each(drop);
    }
}
