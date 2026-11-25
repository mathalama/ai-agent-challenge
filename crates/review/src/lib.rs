mod domain;
mod pipeline;
mod report;
mod stages;

pub use domain::*;
pub use pipeline::Result as StageResult;
pub use pipeline::{
    Check, CheckPhase, CheckRegistry, Pipeline, RunContext, Stage, StageError, StageTrace,
};
pub use report::{
    JsonRenderer, MarkdownRenderer, Readiness, Report, ReportRenderer, ReportSummary,
};

pub const AGENT_VERSION: &str = env!("TZ_AGENT_VERSION");
