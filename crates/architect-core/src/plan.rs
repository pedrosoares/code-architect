//! A multi-step plan the agent lays out and updates as it works — what
//! `write_plan`/`read_plan` (`architect-tools`) operate on, `architect-
//! session` persists, and the desktop UI's Inspector shows.
//!
//! A snapshot, not a log: `write_plan` replaces the whole plan each call
//! (including status flips on earlier steps), so there is exactly one
//! current `Plan` per session, not a history of edits — the same "only the
//! latest state matters" shape as `Conversation.status`, unlike
//! `FileChange`, where every edit matters for rollback.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

/// A step's optional nested step — one level deep only (not an arbitrarily
/// deep tree): matches "each step with an optional sub-steps" literally,
/// and keeps both the JSON schema and the UI's rendering simple.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanSubstep {
    pub description: String,
    #[serde(default)]
    pub status: StepStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanStep {
    pub description: String,
    #[serde(default)]
    pub status: StepStatus,
    #[serde(default)]
    pub substeps: Vec<PlanSubstep>,
}

/// The whole plan for one session — what `write_plan` takes as input and
/// `read_plan` returns, verbatim (see `architect-tools::tools::plan`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Plan {
    /// A one-line summary of the overall task, distinct from any one
    /// step's own description. Optional — a plan can be just a step list.
    #[serde(default)]
    pub goal: Option<String>,
    pub steps: Vec<PlanStep>,
}

/// Where a tool reports a saved plan — the `Plan` analog of
/// [`crate::ChangeRecorder`], for the same reason: what persistence
/// listens on, kept separate from what the model sees in the tool result.
pub trait PlanRecorder: Send + Sync {
    fn record(&self, plan: Plan);
}

/// Discards every plan. The default for tools used outside a session —
/// tests, a future CLI, anywhere nothing is listening.
pub struct NoPlanRecorder;

impl PlanRecorder for NoPlanRecorder {
    fn record(&self, _plan: Plan) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minimal_plan_defaults_status_to_pending() {
        let plan: Plan = serde_json::from_str(r#"{"steps": [{"description": "Do it"}]}"#).unwrap();

        assert_eq!(plan.goal, None);
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].status, StepStatus::Pending);
        assert!(plan.steps[0].substeps.is_empty());
    }

    #[test]
    fn a_substeps_status_also_defaults_to_pending() {
        let plan: Plan = serde_json::from_str(
            r#"{"steps": [{"description": "Parent", "substeps": [{"description": "Child"}]}]}"#,
        )
        .unwrap();

        assert_eq!(plan.steps[0].substeps[0].status, StepStatus::Pending);
    }

    #[test]
    fn round_trips_a_full_plan_through_json() {
        let plan = Plan {
            goal: Some("Ship the feature".to_owned()),
            steps: vec![PlanStep {
                description: "Write the code".to_owned(),
                status: StepStatus::InProgress,
                substeps: vec![PlanSubstep {
                    description: "Write the tests".to_owned(),
                    status: StepStatus::Completed,
                }],
            }],
        };

        let json = serde_json::to_string(&plan).unwrap();
        let round_tripped: Plan = serde_json::from_str(&json).unwrap();

        assert_eq!(round_tripped, plan);
    }
}
