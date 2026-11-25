use std::time::Instant;

use async_trait::async_trait;
use doc::{Document, SourceSpan};
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
    pub source: &'a str,
    pub provider: &'a dyn LlmProvider,
    pub checks: &'a CheckRegistry,
    pub requirements: Vec<Requirement>,
    pub facts: Vec<Fact>,
    pub findings: Vec<Finding>,
    pub questions: Vec<Question>,
    pub trace: Vec<StageTrace>,
    line_offsets: Vec<usize>,
}

impl<'a> RunContext<'a> {
    pub fn new(
        document: Document,
        source: &'a str,
        provider: &'a dyn LlmProvider,
        checks: &'a CheckRegistry,
    ) -> Self {
        Self {
            document,
            source,
            provider,
            checks,
            requirements: Vec::new(),
            facts: Vec::new(),
            findings: Vec::new(),
            questions: Vec::new(),
            trace: Vec::new(),
            line_offsets: std::iter::once(0)
                .chain(source.match_indices('\n').map(|(offset, _)| offset + 1))
                .collect(),
        }
    }

    pub fn source_at(&self, span: SourceSpan) -> Option<&str> {
        if span.page.is_some() || span.start_line > span.end_line {
            return None;
        }
        let start = *self.line_offsets.get(span.start_line.checked_sub(1)?)?;
        self.line_offsets.get(span.end_line.checked_sub(1)?)?;
        let end = self
            .line_offsets
            .get(span.end_line)
            .copied()
            .unwrap_or(self.source.len());
        self.source.get(start..end)
    }
}

#[async_trait]
pub trait Stage: Send + Sync {
    fn name(&self) -> &'static str;
    async fn run(&self, ctx: &mut RunContext<'_>) -> Result<()>;
}

pub struct Pipeline {
    stages: Vec<Box<dyn Stage>>,
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new(
            ScaffoldStage::ORDER
                .into_iter()
                .map(|stage| Box::new(stage) as Box<dyn Stage>)
                .collect(),
        )
    }
}

impl Pipeline {
    pub fn new(stages: Vec<Box<dyn Stage>>) -> Self {
        Self { stages }
    }

    pub async fn run(&self, ctx: &mut RunContext<'_>) -> Result<()> {
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
    async fn run(&self, ctx: &RunContext<'_>) -> Result<Vec<Finding>>;
}

#[derive(Default)]
pub struct CheckRegistry {
    checks: Vec<Box<dyn Check>>,
}

impl CheckRegistry {
    pub fn register(&mut self, check: impl Check + 'static) {
        self.checks.push(Box::new(check));
    }

    pub async fn run(&self, phase: CheckPhase, ctx: &RunContext<'_>) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for check in self.checks.iter().filter(|check| check.phase() == phase) {
            findings.extend(check.run(ctx).await?);
        }
        Ok(findings)
    }
}
