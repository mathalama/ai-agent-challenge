use std::time::Instant;

use async_trait::async_trait;
use doc::Document;
use llm::{LlmProvider, TraceLog};
use serde::Serialize;
use thiserror::Error;

use crate::{
    ContextItem, Fact, Finding, ProgressEvent, ProgressSink, Question, Requirement,
    stages::ScaffoldStage,
};

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
    pub context_items: Vec<ContextItem>,
    pub facts: Vec<Fact>,
    pub findings: Vec<Finding>,
    pub questions: Vec<Question>,
    pub trace: Vec<StageTrace>,
    progress: Option<(&'a dyn ProgressSink, TraceLog)>,
    pub(crate) rules_completed: bool,
    pub(crate) checks_complete: bool,
    pub(crate) facts_complete: bool,
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
            context_items: Vec::new(),
            facts: Vec::new(),
            findings: Vec::new(),
            questions: Vec::new(),
            trace: Vec::new(),
            progress: None,
            rules_completed: false,
            checks_complete: false,
            facts_complete: true,
        }
    }

    /// Включает события прогресса. Расход токенов по стадиям берётся из `llm_trace`,
    /// поэтому это должна быть та же трасса, что передана в `RetryProvider`.
    pub fn with_progress(mut self, sink: &'a dyn ProgressSink, llm_trace: TraceLog) -> Self {
        self.progress = Some((sink, llm_trace));
        self
    }

    pub(crate) fn emit(&self, event: impl FnOnce() -> ProgressEvent) {
        if let Some((sink, _)) = &self.progress {
            sink.emit(&event());
        }
    }

    fn llm_mark(&self) -> usize {
        self.progress
            .as_ref()
            .map_or(0, |(_, trace)| trace.entry_count())
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
        ctx.facts_complete = true;
        let run_started = Instant::now();
        let total = self.stages.len();
        ctx.emit(|| ProgressEvent::RunStarted {
            document: ctx.document.title.clone(),
            model: ctx.provider.model().into(),
            stages: self.stages.iter().map(|stage| stage.name()).collect(),
        });
        for (position, stage) in self.stages.iter().enumerate() {
            let index = position + 1;
            ctx.emit(|| ProgressEvent::StageStarted {
                stage: stage.name(),
                index,
                total,
            });
            let started = Instant::now();
            let mark = ctx.llm_mark();
            let result = stage.run(ctx).await;
            let duration_ms = elapsed_ms(started);
            ctx.trace.push(StageTrace {
                stage: stage.name(),
                duration_ms,
                succeeded: result.is_ok(),
            });
            ctx.emit(|| {
                let (usage, llm_calls, cached_calls) = ctx
                    .progress
                    .as_ref()
                    .map(|(_, trace)| trace.usage_since(mark))
                    .unwrap_or_default();
                ProgressEvent::StageFinished {
                    stage: stage.name(),
                    index,
                    total,
                    duration_ms,
                    succeeded: result.is_ok(),
                    llm_calls,
                    cached_calls,
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    requirements: ctx.requirements.len(),
                    findings: ctx.findings.len(),
                    questions: ctx.questions.len(),
                }
            });
            if let Err(source) = result {
                ctx.emit(|| ProgressEvent::RunFinished {
                    duration_ms: elapsed_ms(run_started),
                    succeeded: false,
                });
                return Err(StageError::Stage {
                    stage: stage.name(),
                    source: Box::new(source),
                });
            }
        }
        ctx.checks_complete = self.full_review
            && ctx.rules_completed
            && ctx.facts_complete
            && ctx.provider.model() != "mock"
            && ctx.requirements.iter().all(|requirement| {
                ctx.document
                    .contains_quote(requirement.location, &requirement.quote)
            });
        ctx.emit(|| ProgressEvent::RunFinished {
            duration_ms: elapsed_ms(run_started),
            succeeded: true,
        });
        Ok(())
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
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
        registry.register(crate::rules::EmptySection);
        registry.register(crate::rules::DocumentVersionDate);
        registry.register(crate::rules::ApproversSection);
        registry.register(crate::rules::ReviewChecklist);
        registry.register(crate::rules::AcceptanceCriteria);
        registry.register(crate::rules::RequiredTableFields);
        registry.register(crate::vague::VagueTermsCheck::default());
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
