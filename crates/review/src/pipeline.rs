use std::time::Instant;

use async_trait::async_trait;
use doc::Document;
use llm::LlmProvider;
use serde::Serialize;
use thiserror::Error;

use crate::{Fact, Finding, Question, Requirement, stages::ScaffoldStage};

#[derive(Debug, Error)]
pub enum StageError {
    #[error("Стадия «{stage}»: {source}")]
    Stage {
        stage: &'static str,
        #[source]
        source: Box<StageError>,
    },
    #[error(transparent)]
    Llm(#[from] llm::LlmError),
    #[error("Ошибка подготовки данных для стадии: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, StageError>;

#[derive(Debug, Serialize)]
pub struct StageTrace {
    pub stage: &'static str,
    pub duration_ms: u64,
    pub succeeded: bool,
}

pub struct RunContext<'a> {
    pub document: Document,
    pub provider: &'a dyn LlmProvider,
    pub checks: &'a CheckRegistry,
    pub requirements: Vec<Requirement>,
    pub facts: Vec<Fact>,
    pub findings: Vec<Finding>,
    pub questions: Vec<Question>,
    pub trace: Vec<StageTrace>,
    pub(crate) rules_completed: bool,
    pub(crate) checks_complete: bool,
}

impl<'a> RunContext<'a> {
    pub fn new(
        document: Document,
        provider: &'a dyn LlmProvider,
        checks: &'a CheckRegistry,
    ) -> Self {
        Self {
            document,
            provider,
            checks,
            requirements: Vec::new(),
            facts: Vec::new(),
            findings: Vec::new(),
            questions: Vec::new(),
            trace: Vec::new(),
            rules_completed: false,
            checks_complete: false,
        }
    }
}

#[async_trait]
pub trait Stage: Send + Sync {
    fn name(&self) -> &'static str;
    async fn run(&self, ctx: &mut RunContext<'_>) -> Result<()>;
}

pub struct Pipeline {
    stages: Vec<Box<dyn Stage>>,
    full_review: bool,
}

impl Default for Pipeline {
    fn default() -> Self {
        Self {
            stages: ScaffoldStage::ORDER
                .into_iter()
                .map(|stage| Box::new(stage) as Box<dyn Stage>)
                .collect(),
            full_review: true,
        }
    }
}

impl Pipeline {
    pub fn new(stages: Vec<Box<dyn Stage>>) -> Self {
        Self {
            stages,
            full_review: false,
        }
    }

    pub async fn run(&self, ctx: &mut RunContext<'_>) -> Result<()> {
        ctx.rules_completed = false;
        ctx.checks_complete = false;
        for stage in &self.stages {
            let started = Instant::now();
            let result = stage.run(ctx).await;
            ctx.trace.push(StageTrace {
                stage: stage.name(),
                duration_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                succeeded: result.is_ok(),
            });
            result.map_err(|source| StageError::Stage {
                stage: stage.name(),
                source: Box::new(source),
            })?;
        }
        ctx.checks_complete = self.full_review
            && ctx.rules_completed
            && ctx.provider.model() != "mock"
            && ctx.requirements.iter().all(|requirement| {
                ctx.document
                    .contains_quote(requirement.location, &requirement.quote)
            });
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckPhase {
    Rules,
    Requirement,
    Set,
}

#[async_trait]
pub trait Check: Send + Sync {
    fn phase(&self) -> CheckPhase;
    fn applies_to(&self, _ctx: &RunContext<'_>) -> bool {
        true
    }
    async fn run(&self, ctx: &RunContext<'_>) -> Result<Vec<Finding>>;
}

#[derive(Default)]
pub struct CheckRegistry {
    checks: Vec<Box<dyn Check>>,
}

impl CheckRegistry {
    pub fn with_builtin_rules() -> Self {
        let mut registry = Self::default();
        registry.register(crate::rules::TestingSection);
        registry
    }

    pub fn register(&mut self, check: impl Check + 'static) {
        self.checks.push(Box::new(check));
    }

    pub(crate) fn can_complete_rules(&self, ctx: &RunContext<'_>) -> bool {
        let mut rules = self
            .checks
            .iter()
            .filter(|check| check.phase() == CheckPhase::Rules)
            .peekable();
        rules.peek().is_some() && rules.all(|check| check.applies_to(ctx))
    }

    pub async fn run(&self, phase: CheckPhase, ctx: &RunContext<'_>) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for check in self
            .checks
            .iter()
            .filter(|check| check.phase() == phase && check.applies_to(ctx))
        {
            findings.extend(check.run(ctx).await?);
        }
        Ok(findings)
    }
}
