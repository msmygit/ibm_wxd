//! A generic, service-agnostic preflight module: verifies the external CLIs the
//! later modules need are present (via the `CommandRunner` seam, so it is
//! hermetic in tests). It is part of the spine — real install modules register
//! after it during Phase B.

use async_trait::async_trait;
use sw_core::{Module, Step, StepContext, StepOutcome};

/// Checks one required tool by running `<tool> <probe-arg>` and treating a
/// zero exit as "present".
struct ToolCheck {
    tool: &'static str,
    probe: &'static str,
}

#[async_trait]
impl Step for ToolCheck {
    fn id(&self) -> &str {
        self.tool
    }

    fn title(&self) -> &str {
        self.tool
    }

    async fn run(&self, ctx: &StepContext) -> StepOutcome {
        ctx.log(format!("checking for `{}`", self.tool));
        match ctx.runner().run(self.tool, &[self.probe.to_string()]).await {
            Ok(out) if out.success() => {
                ctx.progress(100);
                StepOutcome::Completed
            }
            Ok(out) => StepOutcome::Failed {
                error: format!("`{}` returned exit {}", self.tool, out.status),
                next_steps: vec![format!(
                    "Ensure `{}` is installed and on your PATH, then retry this step.",
                    self.tool
                )],
            },
            Err(e) => StepOutcome::Failed {
                error: format!("could not run `{}`: {e}", self.tool),
                next_steps: vec![format!(
                    "Install `{}` and make sure it is on your PATH, then retry.",
                    self.tool
                )],
            },
        }
    }
}

/// The preflight module.
pub struct PreflightModule;

impl Module for PreflightModule {
    fn id(&self) -> &str {
        "preflight"
    }

    fn title(&self) -> &str {
        "Preflight checks"
    }

    fn steps(&self) -> Vec<Box<dyn Step>> {
        vec![
            Box::new(ToolCheck {
                tool: "oc",
                probe: "version",
            }),
            Box::new(ToolCheck {
                tool: "helm",
                probe: "version",
            }),
            Box::new(ToolCheck {
                tool: "aws",
                probe: "--version",
            }),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use sw_core::{EventBus, MockCommandRunner, MockResponse};

    #[tokio::test]
    async fn tool_present_completes() {
        let runner = Arc::new(MockCommandRunner::new(vec![MockResponse::ok(
            "oc version",
            "ok",
        )]));
        let ctx = StepContext::new(
            "r".into(),
            "preflight/oc".into(),
            runner,
            EventBus::new(),
            Default::default(),
            Default::default(),
        );
        let step = ToolCheck {
            tool: "oc",
            probe: "version",
        };
        assert_eq!(step.run(&ctx).await, StepOutcome::Completed);
    }

    #[tokio::test]
    async fn tool_missing_fails_with_next_steps() {
        let runner = Arc::new(MockCommandRunner::new(vec![MockResponse::fail(
            "helm version",
            127,
            "not found",
        )]));
        let ctx = StepContext::new(
            "r".into(),
            "preflight/helm".into(),
            runner,
            EventBus::new(),
            Default::default(),
            Default::default(),
        );
        let step = ToolCheck {
            tool: "helm",
            probe: "version",
        };
        match step.run(&ctx).await {
            StepOutcome::Failed { next_steps, .. } => assert!(!next_steps.is_empty()),
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}
