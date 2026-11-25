mod domain;
mod pipeline;
mod report;
mod rules;
pub mod segment;
mod stages;

pub use domain::*;
pub use pipeline::Result as StageResult;
pub use pipeline::{
    Check, CheckPhase, CheckRegistry, Pipeline, RunContext, Stage, StageError, StageTrace,
};
pub use report::{
    JsonRenderer, MarkdownRenderer, Readiness, Report, ReportRenderer, ReportSummary,
};
pub use segment::{extract_source_id, extract_table_requirements};

pub const AGENT_VERSION: &str = env!("AGENT_VERSION");
