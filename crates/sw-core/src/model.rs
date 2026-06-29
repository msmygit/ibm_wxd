//! Core data model: runs, steps, their statuses, and the outcome a step returns.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Opaque run identifier (UUID v4 string).
pub type RunId = String;

/// Stable step identifier, unique within a run ("module_id/step_id").
pub type StepId = String;

/// Lifecycle status of a whole run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Created but not yet started.
    Pending,
    /// Actively executing steps.
    Running,
    /// A step returned `NeedsInput`; waiting on the user.
    AwaitingInput,
    /// Paused at a step boundary by the user.
    Paused,
    /// A step failed; the run is halted pending retry.
    Failed,
    /// All steps completed.
    Completed,
}

/// Lifecycle status of a single step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Running,
    AwaitingInput,
    Completed,
    Failed,
    Skipped,
}

/// What a step reports back to the orchestrator when its `run` returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// The step finished successfully.
    Completed,
    /// The step needs the user to supply named inputs before it can proceed.
    NeedsInput { prompt: String, fields: Vec<InputField> },
    /// The step failed; carries a human error and actionable next steps.
    Failed { error: String, next_steps: Vec<String> },
}

/// A single input a step requests from the user when it returns `NeedsInput`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputField {
    pub key: String,
    pub label: String,
    /// Render as a masked field and never persist/log in plaintext.
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub default: Option<String>,
}

/// Persisted state of one step within a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepState {
    pub id: StepId,
    pub module_id: String,
    pub title: String,
    pub status: StepStatus,
    /// Last error message, if the step failed.
    #[serde(default)]
    pub error: Option<String>,
    /// Actionable next steps from the last failure, if any.
    #[serde(default)]
    pub next_steps: Vec<String>,
}

impl StepState {
    pub fn new(module_id: &str, id: &str, title: &str) -> Self {
        Self {
            id: format!("{module_id}/{id}"),
            module_id: module_id.to_string(),
            title: title.to_string(),
            status: StepStatus::Pending,
            error: None,
            next_steps: Vec::new(),
        }
    }
}

/// Default run mode when an older `state.json` predates the `mode` field.
fn default_mode() -> String {
    "provision".to_string()
}

/// The full persisted state of a run. Secrets are NEVER stored here — only
/// non-secret inputs live in `inputs`; secrets go to the separate secret store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunState {
    pub id: RunId,
    pub status: RunStatus,
    /// Which step graph this run uses (e.g. `"provision"` or `"existing"`).
    #[serde(default = "default_mode")]
    pub mode: String,
    pub steps: Vec<StepState>,
    /// Index of the step currently being driven (or to resume at).
    pub cursor: usize,
    /// Non-secret inputs collected so far (key -> value).
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
    /// When a step is `AwaitingInput`, the fields it asked for.
    #[serde(default)]
    pub pending_inputs: Vec<InputField>,
    /// Prompt shown alongside `pending_inputs`.
    #[serde(default)]
    pub pending_prompt: Option<String>,
}

impl RunState {
    pub fn new(id: RunId, steps: Vec<StepState>) -> Self {
        Self {
            id,
            status: RunStatus::Pending,
            mode: default_mode(),
            steps,
            cursor: 0,
            inputs: BTreeMap::new(),
            pending_inputs: Vec::new(),
            pending_prompt: None,
        }
    }

    /// The step the cursor points at, if any remain.
    pub fn current_step(&self) -> Option<&StepState> {
        self.steps.get(self.cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_id_is_namespaced_by_module() {
        let s = StepState::new("mod-provision", "create-cluster", "Create cluster");
        assert_eq!(s.id, "mod-provision/create-cluster");
        assert_eq!(s.status, StepStatus::Pending);
    }

    #[test]
    fn run_state_roundtrips_through_json() {
        let mut run = RunState::new(
            "abc".into(),
            vec![StepState::new("m", "s1", "Step one")],
        );
        run.inputs.insert("region".into(), "us-east-1".into());
        let json = serde_json::to_string(&run).unwrap();
        let back: RunState = serde_json::from_str(&json).unwrap();
        assert_eq!(run, back);
        assert_eq!(back.current_step().unwrap().title, "Step one");
    }
}
