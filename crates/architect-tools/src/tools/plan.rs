//! Save and read back the agent's current plan for a task — a list of
//! steps, each optionally with sub-steps, each with a status. Unlike every
//! other tool here, `write_plan`'s input *is* `architect_core::Plan`
//! directly (it already derives `Deserialize`), and `read_plan` returns
//! whatever `ToolContext::current_plan` was handed at the start of this
//! turn — see that type's doc comment for why a plan written earlier in
//! the *same* turn won't show up there yet (the model already knows what
//! it just wrote; this is for a fresh turn or a resumed session).

use architect_core::{Plan, StepStatus};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{
    context::ToolContext,
    tool::{Tool, ToolOutput},
};

fn plan_input_schema() -> Value {
    let step_status = json!({
        "type": "string",
        "enum": ["pending", "in_progress", "completed"],
        "description": "Defaults to \"pending\" if omitted.",
    });

    json!({
        "type": "object",
        "properties": {
            "goal": {
                "type": "string",
                "description": "A one-line summary of the overall task — distinct from any one step's own description.",
            },
            "steps": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "description": {"type": "string"},
                        "status": step_status,
                        "substeps": {
                            "type": "array",
                            "description": "Optional — most steps won't have any.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "description": {"type": "string"},
                                    "status": step_status,
                                },
                                "required": ["description"],
                                "additionalProperties": false,
                            },
                        },
                    },
                    "required": ["description"],
                    "additionalProperties": false,
                },
            },
        },
        "required": ["steps"],
        "additionalProperties": false,
    })
}

pub struct WritePlan;

#[async_trait]
impl Tool for WritePlan {
    fn name(&self) -> &'static str {
        "write_plan"
    }

    fn description(&self) -> &'static str {
        "Save the current plan for this task: a list of steps, each optionally with sub-steps, \
         each with a status (pending, in_progress, completed). Replaces whatever plan was saved \
         before — always pass the FULL plan, not just what changed. Call this when the plan is \
         first laid out, and again whenever it changes: a step's status moves along, a step is \
         added, or the plan is revised."
    }

    fn input_schema(&self) -> Value {
        plan_input_schema()
    }

    fn mutates(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutput, String> {
        let plan: Plan = serde_json::from_value(input).map_err(|error| error.to_string())?;

        let rendered = format_plan(&plan);
        ctx.record_plan(plan);
        Ok(rendered.into())
    }
}

pub struct ReadPlan;

#[async_trait]
impl Tool for ReadPlan {
    fn name(&self) -> &'static str {
        "read_plan"
    }

    fn description(&self) -> &'static str {
        "Read the plan currently saved for this task, if one exists."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        })
    }

    async fn call(&self, _input: Value, ctx: &ToolContext) -> Result<ToolOutput, String> {
        match ctx.current_plan() {
            Some(plan) => Ok(format_plan(plan).into()),
            None => Ok("No plan saved yet for this session.".to_owned().into()),
        }
    }
}

fn status_glyph(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => " ",
        StepStatus::InProgress => "~",
        StepStatus::Completed => "x",
    }
}

fn format_plan(plan: &Plan) -> String {
    let mut lines = Vec::new();
    if let Some(goal) = &plan.goal {
        lines.push(format!("Goal: {goal}\n"));
    }

    if plan.steps.is_empty() {
        lines.push("(no steps)".to_owned());
    }

    for (index, step) in plan.steps.iter().enumerate() {
        let number = index + 1;
        lines.push(format!(
            "{number}. [{}] {}",
            status_glyph(step.status),
            step.description
        ));
        for (sub_index, substep) in step.substeps.iter().enumerate() {
            lines.push(format!(
                "   {number}.{}. [{}] {}",
                sub_index + 1,
                status_glyph(substep.status),
                substep.description
            ));
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use architect_core::{PlanStep, PlanSubstep};

    use super::*;

    fn plan_with_goal_and_substeps() -> Plan {
        Plan {
            goal: Some("Add dark mode support".to_owned()),
            steps: vec![
                PlanStep {
                    description: "Set up the theme context".to_owned(),
                    status: StepStatus::Completed,
                    substeps: vec![
                        PlanSubstep {
                            description: "Add a ThemeMode enum".to_owned(),
                            status: StepStatus::Completed,
                        },
                        PlanSubstep {
                            description: "Wire it into the root component".to_owned(),
                            status: StepStatus::Completed,
                        },
                    ],
                },
                PlanStep {
                    description: "Implement the toggle".to_owned(),
                    status: StepStatus::InProgress,
                    substeps: vec![PlanSubstep {
                        description: "Persist the choice".to_owned(),
                        status: StepStatus::Pending,
                    }],
                },
                PlanStep {
                    description: "Update the docs".to_owned(),
                    status: StepStatus::Pending,
                    substeps: vec![],
                },
            ],
        }
    }

    #[test]
    fn formats_a_plan_with_goal_and_substeps() {
        let text = format_plan(&plan_with_goal_and_substeps());

        assert!(text.starts_with("Goal: Add dark mode support"));
        assert!(text.contains("1. [x] Set up the theme context"));
        assert!(text.contains("1.1. [x] Add a ThemeMode enum"));
        assert!(text.contains("1.2. [x] Wire it into the root component"));
        assert!(text.contains("2. [~] Implement the toggle"));
        assert!(text.contains("2.1. [ ] Persist the choice"));
        assert!(text.contains("3. [ ] Update the docs"));
    }

    #[test]
    fn a_plan_with_no_goal_omits_the_goal_line() {
        let plan = Plan {
            goal: None,
            steps: vec![PlanStep {
                description: "Just one step".to_owned(),
                status: StepStatus::Pending,
                substeps: vec![],
            }],
        };

        let text = format_plan(&plan);
        assert!(!text.contains("Goal:"));
        assert!(text.starts_with("1. [ ] Just one step"));
    }

    #[test]
    fn an_empty_plan_says_so() {
        let plan = Plan {
            goal: None,
            steps: vec![],
        };

        assert_eq!(format_plan(&plan), "(no steps)");
    }

    #[tokio::test]
    async fn write_plan_records_and_returns_the_formatted_plan() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path()).unwrap();

        let result = WritePlan
            .call(
                json!({"goal": "Ship it", "steps": [{"description": "Do the thing"}]}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.text.contains("Goal: Ship it"));
        assert!(result.text.contains("1. [ ] Do the thing"));
    }

    #[tokio::test]
    async fn read_plan_reports_none_saved_when_there_is_no_current_plan() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path()).unwrap();

        let result = ReadPlan.call(json!({}), &ctx).await.unwrap();

        assert_eq!(result.text, "No plan saved yet for this session.");
    }

    #[tokio::test]
    async fn read_plan_returns_whatever_current_plan_the_context_was_given() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path())
            .unwrap()
            .with_current_plan(Some(plan_with_goal_and_substeps()));

        let result = ReadPlan.call(json!({}), &ctx).await.unwrap();

        assert!(result.text.contains("Goal: Add dark mode support"));
        assert!(result.text.contains("2. [~] Implement the toggle"));
    }
}
