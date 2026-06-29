//! The set of installed modules, in execution order. The orchestrator flattens
//! it into a single ordered step list; the `/modules` API renders it.

use crate::model::StepState;
use crate::module::{Module, Step};
use serde::Serialize;

/// Ordered collection of modules that make up an install run.
#[derive(Default)]
pub struct ModuleRegistry {
    modules: Vec<Box<dyn Module>>,
}

/// Serializable view of a module and its steps, for the `/modules` endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct ModuleView {
    pub id: String,
    pub title: String,
    pub steps: Vec<StepView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StepView {
    pub id: String,
    pub title: String,
}

impl ModuleRegistry {
    pub fn new() -> Self {
        Self { modules: Vec::new() }
    }

    /// Append a module to the end of the execution order.
    pub fn register(&mut self, module: Box<dyn Module>) {
        self.modules.push(module);
    }

    /// Builder-style registration.
    pub fn with(mut self, module: Box<dyn Module>) -> Self {
        self.register(module);
        self
    }

    /// Flatten every module's steps into one ordered list, paired with the
    /// owning module id. This is the canonical run order.
    pub fn flatten(&self) -> Vec<(String, Box<dyn Step>)> {
        let mut out = Vec::new();
        for m in &self.modules {
            for step in m.steps() {
                out.push((m.id().to_string(), step));
            }
        }
        out
    }

    /// Build the initial per-step state list for a fresh run.
    pub fn initial_steps(&self) -> Vec<StepState> {
        self.flatten()
            .iter()
            .map(|(module_id, step)| StepState::new(module_id, step.id(), step.title()))
            .collect()
    }

    /// Serializable view for the API.
    pub fn views(&self) -> Vec<ModuleView> {
        self.modules
            .iter()
            .map(|m| ModuleView {
                id: m.id().to_string(),
                title: m.title().to_string(),
                steps: m
                    .steps()
                    .iter()
                    .map(|s| StepView {
                        id: s.id().to_string(),
                        title: s.title().to_string(),
                    })
                    .collect(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::StepOutcome;
    use crate::module::StepContext;
    use async_trait::async_trait;

    struct StubStep {
        id: String,
    }

    #[async_trait]
    impl Step for StubStep {
        fn id(&self) -> &str {
            &self.id
        }
        fn title(&self) -> &str {
            "Stub"
        }
        async fn run(&self, _ctx: &StepContext) -> StepOutcome {
            StepOutcome::Completed
        }
    }

    struct StubModule {
        id: String,
        step_ids: Vec<String>,
    }

    impl Module for StubModule {
        fn id(&self) -> &str {
            &self.id
        }
        fn title(&self) -> &str {
            "Stub module"
        }
        fn steps(&self) -> Vec<Box<dyn Step>> {
            self.step_ids
                .iter()
                .map(|s| Box::new(StubStep { id: s.clone() }) as Box<dyn Step>)
                .collect()
        }
    }

    fn module(id: &str, steps: &[&str]) -> Box<dyn Module> {
        Box::new(StubModule {
            id: id.to_string(),
            step_ids: steps.iter().map(|s| s.to_string()).collect(),
        })
    }

    #[test]
    fn flatten_preserves_module_then_step_order() {
        let reg = ModuleRegistry::new()
            .with(module("mod-a", &["a1", "a2"]))
            .with(module("mod-b", &["b1"]));
        let steps = reg.initial_steps();
        let ids: Vec<_> = steps.iter().map(|s| s.id.clone()).collect();
        assert_eq!(ids, vec!["mod-a/a1", "mod-a/a2", "mod-b/b1"]);
    }

    #[test]
    fn views_expose_titles() {
        let reg = ModuleRegistry::new().with(module("mod-a", &["a1"]));
        let v = reg.views();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].id, "mod-a");
        assert_eq!(v[0].steps[0].id, "a1");
    }
}
